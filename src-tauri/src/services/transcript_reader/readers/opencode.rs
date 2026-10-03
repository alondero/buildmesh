//! OpenCode owns transcript reads from its native read-only SQLite store.
//! The shared dispatcher validates sessions and shapes results; this reader
//! resolves the store and applies row budgets, message parsing and report identity.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(test)]
use super::super::types::{effective_tail, TranscriptTail};
use rusqlite::{Connection, OpenFlags};

use crate::agent::session_lifecycle::LifecycleKind;
use crate::env;
use crate::models::EnvType;
use crate::services::transcript_reader::adapter::{
    HookClassification, HookDecision, LocateCtx, TranscriptAdapter,
};
use crate::services::transcript_reader::types::{
    cap_tool_calls, push_bounded, truncate, Parsed, ToolCall, Turn, UnavailableReason,
    MAX_TURN_TEXT,
};

/// Fixed-row window for the Coordinator digest path. Chosen to be wide
/// enough to span a typical user → assistant exchange (a few turns each
/// with reasoning + tool + text parts) but bounded so a 10k-row session
/// doesn't full-scan on every poll. Tuned so the digest finds the
/// blocking question even when the latest row is a user reply.
pub(crate) const OPENCODE_DIGEST_WINDOW: usize = 50;

/// Row-to-turn factor for the full /log read path. OpenCode rows are
/// message events, not turns — assistant turn coalescing, reasoning-only
/// drop, and tool-only drop mean `factor` rows typically produce 1 turn.
/// Factor > 1 ensures the caller can always extract `tail` turns once the
/// parser has coalesced/dropped, even on dense conversations.
pub(crate) const OPENCODE_TURN_TO_MESSAGE_FACTOR: usize = 3;

/// OpenCode session IDs start with `ses_` (schema `SessionID`).
/// `pub(crate)` so the capture poller (`services::opencode_session`)
/// can share the same gate without duplicating the prefix check — the
/// two readers (transcript + session-id capture poller) must agree on
/// what an OpenCode session id looks like.
pub(crate) fn is_opencode_session_id(id: &str) -> bool {
    id.starts_with("ses_") && id.len() > 4
}

/// Resolve the on-disk SQLite path OpenCode uses for its session +
/// message store. Mirrors the env handling in `services::usage`
/// (which opens the same DB for the billing rollup); on WSL the
/// Linux-side path is converted to the Windows-side UNC form so a
/// Rust reader can `Connection::open` it directly. `pub(crate)` so
/// the capture poller resolves the same DB without duplicating the
/// env↔host mapping.
pub(crate) fn opencode_db_path(env_type: EnvType) -> Option<PathBuf> {
    match env_type {
        EnvType::WindowsInterop => {
            crate::env::windows_cli_home(".local/share/opencode/opencode.db")
        }
        EnvType::Wsl => {
            let linux = crate::env::wsl_home()?.join(".local/share/opencode/opencode.db");
            Some(PathBuf::from(crate::env::to_host_path(
                &linux.to_string_lossy(),
            )))
        }
        EnvType::Windows => {
            let home = std::env::var("USERPROFILE")
                .ok()
                .or_else(|| std::env::var("HOME").ok())?;
            Some(
                PathBuf::from(home)
                    .join(".local")
                    .join("share")
                    .join("opencode")
                    .join("opencode.db"),
            )
        }
    }
}

/// SQLite busy_timeout the OpenCode reader applies on every open: lets a
/// concurrent writer (the live OpenCode CLI) hold the lock briefly
/// instead of returning `SQLITE_BUSY` to a Coordinator poll. **Bounded
/// to 100 ms** so a `GET /nodes` poll over N OpenCode nodes cannot park
/// a Tokio worker thread for longer than `100 ms × N` if every node hits
/// a writer-held lock — the same worst-case bound every other adapter
/// already accepts from `cwrap` / Claude-Code JSONL reads on the Tokio
/// pool (issue #1380).
const OPENCODE_READER_BUSY_TIMEOUT_MS: u64 = 100;

/// Drop-in [`TranscriptAdapter`] for OpenCode.
///
/// Store-backed reads override the shared JSONL methods.
pub(crate) struct OpenCodeAdapter;

impl TranscriptAdapter for OpenCodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        opencode_resolve(Some(ctx.session_id), ctx.node_path)
            .ok()
            .map(|(path, _)| path)
    }

    fn parse(
        &self,
        _lines: Box<dyn Iterator<Item = String> + '_>,
        _keep: usize,
        _max_text: usize,
    ) -> Parsed {
        // Store reads query message rows instead of parsing JSONL.
        Parsed {
            turns: Vec::new(),
            last_assistant_message: None,
            saw_malformed: false,
        }
    }

    fn line_has_assistant_text(&self, _line: &str) -> bool {
        // SQLite message rows are read through the store methods.
        false
    }

    fn read_tail(
        &self,
        path: &Path,
        session_id: &str,
        keep: usize,
    ) -> Result<Parsed, UnavailableReason> {
        let budget = keep.saturating_mul(OPENCODE_TURN_TO_MESSAGE_FACTOR);
        let messages = read_opencode_messages(path, session_id, budget)
            .ok_or(UnavailableReason::Unreadable)?;
        Ok(parse_opencode_messages(&messages, keep))
    }

    fn last_assistant_message(
        &self,
        path: &Path,
        session_id: &str,
    ) -> Result<Parsed, UnavailableReason> {
        let messages = read_opencode_messages(path, session_id, OPENCODE_DIGEST_WINDOW)
            .ok_or(UnavailableReason::Unreadable)?;
        Ok(parse_opencode_messages(&messages, 1))
    }

    fn assistant_report(
        &self,
        path: &Path,
        session_id: &str,
    ) -> Option<super::super::AssistantReport> {
        read_opencode_message_rows(path, session_id, OPENCODE_DIGEST_WINDOW)?
            .into_iter()
            .rev()
            .find_map(|(id, message)| {
                let preview = parse_opencode_messages(std::slice::from_ref(&message), 1)
                    .last_assistant_message?;
                let text = parse_opencode_messages_with_text_limit(&[message], 1, usize::MAX)
                    .last_assistant_message?;
                Some(super::super::AssistantReport {
                    revision: super::super::assistant_revision(&id, &preview, &text),
                    text,
                })
            })
    }

    fn classify_hook_value(
        &self,
        payload: &serde_json::Value,
        provider: &str,
    ) -> Option<HookClassification> {
        // OpenCode's plugin events are harness-specific — the
        // `session.idle` / `session.created` names aren't shared with
        // Claude Code, Codex, or AGY. Gate on `provider == "opencode"`
        // (or empty, matching the legacy hook POSTs from
        // `~/.opencode/plugins/buildmesh-attention.js` that don't set
        // a `provider` field) so a sibling harness that ever borrowed
        // the same event names cannot false-positive this adapter's
        // classification.
        if provider != "opencode" && !provider.is_empty() {
            return None;
        }
        // OpenCode's plugin fires `session.idle` when the agent finishes
        // a turn and waits for another prompt — classify as Ready.
        // Only explicit question/permission requests need human attention.
        // `session.created` fires once at TUI boot
        // carrying the freshly minted `ses_…` id; it's lifecycle-neutral
        // (the id-capture path persists the session id, the attention
        // route must not flip a fresh spawn into `AwaitingInput`).
        let event = payload
            .get("hook_event_name")
            .or_else(|| payload.get("hookEventName"))
            .and_then(|n| n.as_str())
            .map(str::to_ascii_lowercase);
        match event.as_deref() {
            Some("session.idle") => Some(HookClassification {
                decision: HookDecision::Ready,
                kind: None,
            }),
            Some("question.asked") => Some(HookClassification {
                decision: HookDecision::MarkInput,
                kind: Some(LifecycleKind::QuestionRequested),
            }),
            Some("session.created") => Some(HookClassification {
                decision: HookDecision::Ignore,
                kind: None,
            }),
            _ => None,
        }
    }
}

/// Read the row tail of an OpenCode session's `message` table. Returns the
/// up-to-`row_budget` newest messages with their parts in chronological order
/// (oldest → newest), matching how `opencode export <id>` orders them. Returns
/// `None` on any I/O or query failure so callers can degrade to
/// `Unreadable`.
///
/// `Limit` is bound as a real SQL parameter (`?2`) — no `format!` SQL,
/// even though `limit` is server-controlled. The shape matches the rest
/// of the reader's prepared statements and stays parameter-bound for the
/// case where the upstream schema adds a filter column we don't control
/// yet.
pub(crate) fn read_opencode_messages(
    db_path: &Path,
    session_id: &str,
    row_budget: usize,
) -> Option<Vec<serde_json::Value>> {
    Some(
        read_opencode_message_rows(db_path, session_id, row_budget)?
            .into_iter()
            .map(|(_, message)| message)
            .collect(),
    )
}

pub(crate) fn read_opencode_message_rows(
    db_path: &Path,
    session_id: &str,
    row_budget: usize,
) -> Option<Vec<(String, serde_json::Value)>> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    if let Err(error) = conn.busy_timeout(Duration::from_millis(OPENCODE_READER_BUSY_TIMEOUT_MS)) {
        // Busytimeout is best-effort — log but don't fail the read;
        // a non-zero busy_timeout simply means concurrent writers will
        // surface as SQLITE_BUSY (the previous behaviour).
        tracing::debug!(
            "opencode transcript reader: busy_timeout set failed ({error}); \
             concurrent writes may degrade to Unreadable"
        );
    }
    let mut stmt = conn
        .prepare(
            // LIMIT applies to messages, not joined parts. One statement keeps
            // metadata and content on the same SQLite read snapshot.
            // Unary + disqualifies the session predicate as an index lookup:
            // use the message-id index instead of rescanning the whole session
            // for every message, while still checking part session ownership.
            "WITH recent AS (SELECT id, session_id, time_created, data FROM message \
                WHERE session_id = ?1 ORDER BY time_created DESC, id DESC LIMIT ?2) \
             SELECT m.id, m.data, p.data FROM recent m \
             LEFT JOIN part p ON p.message_id = m.id AND +p.session_id = m.session_id \
             ORDER BY m.time_created DESC, m.id DESC, p.id ASC",
        )
        .ok()?;
    let mut latest: Vec<(String, serde_json::Value)> = Vec::new();
    let mut rows = stmt
        .query(rusqlite::params![session_id, row_budget as i64])
        .ok()?;
    while let Some(row) = rows.next().ok()? {
        let id: String = row.get(0).ok()?;
        let data: String = row.get(1).ok()?;
        if latest.last().is_none_or(|(previous, _)| previous != &id) {
            let info =
                serde_json::from_str::<serde_json::Value>(&data).unwrap_or(serde_json::Value::Null);
            latest.push((id, serde_json::json!({"info": info, "parts": []})));
        }
        if let Some(data) = row.get::<_, Option<String>>(2).ok()? {
            let (_, message) = latest.last_mut()?;
            match serde_json::from_str::<serde_json::Value>(&data) {
                Ok(part) if part.is_object() => message["parts"].as_array_mut()?.push(part),
                // Preserve corruption as malformed evidence. Dropping it could
                // expose an older answer while hiding newer input/tool work.
                _ => message["info"] = serde_json::Value::Null,
            }
        }
    }
    // The query returned DESC; the parser consumes ASC (matches the
    // natural timeline that `opencode export` orders by). Reverse once
    // here so the parser never has to know about the bound direction.
    latest.reverse();
    Some(latest)
}

/// Pure tail reader over a Vec of OpenCode message values. Splits
/// parse + result shaping so tests can drive the dispatch (env path
/// resolution) and the parsing semantics independently — same split the
/// `agy_locator_in` / `agy_locator` pair uses for the AGY adapter. The
/// `tail` argument flows through `effective_tail` to the parser's
/// rolling-buffer keep.
#[cfg(test)]
pub(crate) fn read_opencode_tail_from_messages(
    messages: &[serde_json::Value],
    tail: usize,
) -> TranscriptTail {
    super::super::result(
        Ok(parse_opencode_messages(messages, effective_tail(tail))),
        false,
    )
}

/// Pure digest reader. Always returns `turns: Vec::new()` so the
/// digest consumer's bounded-memory optimisation holds for OpenCode
/// (issue #1296 review finding A — a pre-review implementation
/// returned `Vec<Turn>` from this path and broke the digest contract).
///
/// **The degrade gate is on `turns.is_empty()`, NOT on
/// `last_assistant_message.is_none()`.** A node that has only user
/// turns (no assistant reply yet — actively running, or the user
/// repeatedly typed prompts) must NOT degrade as `Empty`: that would
/// mask an `awaiting_input` or in-flight node from the Coordinator
/// digest. The digest returns `Available { turns: Vec::new(),
/// last_assistant_message: None }` for an actively-working node and
/// the Coordinator renders the spine fields (status, needs_feedback)
/// regardless of the digest content.
#[cfg(test)]
pub(crate) fn read_opencode_digest_from_messages(messages: &[serde_json::Value]) -> TranscriptTail {
    super::super::result(Ok(parse_opencode_messages(messages, 1)), true)
}

/// Resolve a valid session id to its existing env-aware SQLite store.
pub(crate) fn opencode_resolve<'a>(
    session_id: Option<&'a str>,
    node_path: &str,
) -> Result<(PathBuf, &'a str), UnavailableReason> {
    let session_id = session_id
        .filter(|s| !s.is_empty())
        .ok_or(UnavailableReason::NoSession)?;
    if !is_opencode_session_id(session_id) {
        // A non-`ses_` id cannot match any OpenCode row; degrade
        // quietly rather than opening the DB to find nothing. The
        // gate is shared with `services::opencode_session` so the
        // two readers (transcript + capture poller) cannot drift on
        // what an OpenCode session id looks like — both import the
        // same `is_opencode_session_id` from this adapter.
        return Err(UnavailableReason::NoTranscript);
    }
    let env_type = env::runtime_for_spawn_path(node_path);
    let db_path = opencode_db_path(env_type).ok_or(UnavailableReason::NoTranscript)?;
    if !db_path.exists() {
        return Err(UnavailableReason::NoTranscript);
    }
    Ok((db_path, session_id))
}

/// Parse a slice of OpenCode message envelopes into the shared
/// [`Parsed`] contract: rolling `keep`-bounded turn window,
/// whole-stream last-assistant-message tracking, malformed-flag so a
/// renamed-field message degrades as `ShapeChanged`. Maps each
/// message's `parts` array onto text + tool calls:
///
/// - `text` parts → concatenated into `Turn.text`
/// - `reasoning` parts → silently dropped (chain-of-thought, not
///   dialogue)
/// - `tool` parts → converted to [`ToolCall`]s using `state.input` and
///   `state.title` as the tool name (falling back to the part's `name`)
/// - `step-start` / `step-finish` / unknown parts → silently skipped,
///   never flagged (the "graceful failure on unknown event types" rule)
///
/// A user turn with no `text` parts, or an assistant turn with
/// neither text nor tool calls, is dropped — same as Claude's
/// thinking-only line.
pub(crate) fn parse_opencode_messages(messages: &[serde_json::Value], keep: usize) -> Parsed {
    parse_opencode_messages_with_text_limit(messages, keep, MAX_TURN_TEXT)
}

pub(crate) fn parse_opencode_messages_with_text_limit(
    messages: &[serde_json::Value],
    keep: usize,
    max_text: usize,
) -> Parsed {
    let keep = keep.max(1);
    let mut turns: VecDeque<Turn> = VecDeque::new();
    let mut last_assistant_message: Option<String> = None;
    let mut saw_malformed = false;

    for message in messages {
        // Each message envelope is `{"info": {role, ...}, "parts": [...]}`.
        // A missing `info.role` is a structural break on a recognized
        // message shape — flag malformed. A missing `parts` is empty
        // (no text, no tool calls); the message is then dropped as a
        // no-op.
        let Some(info) = message.get("info") else {
            saw_malformed = true;
            continue;
        };
        let Some(role) = info.get("role").and_then(|r| r.as_str()) else {
            saw_malformed = true;
            continue;
        };
        if role != "user" && role != "assistant" {
            // Unknown role on a recognized envelope — flag as malformed
            // so a future "system" or "tool" role doesn't silently
            // degrade.
            saw_malformed = true;
            continue;
        }
        let parts = message
            .get("parts")
            .and_then(|p| p.as_array())
            .map(|a| a.as_slice())
            .unwrap_or(&[]);

        let text = concat_opencode_text_parts(parts);
        let mut tool_calls = extract_opencode_tool_calls(parts);

        if role == "user" {
            // Empty user prompts (e.g. a file-only attachment with no
            // text) are dropped — mirrors Claude's empty `user` line
            // rule.
            if text.trim().is_empty() {
                continue;
            }
            push_bounded(
                &mut turns,
                Turn {
                    role: "user".to_string(),
                    text: truncate(&text, max_text),
                    tool_calls: Vec::new(),
                },
                keep,
            );
            continue;
        }

        // Assistant turn: drop thinking-only (text empty AND no tool
        // calls).
        if text.trim().is_empty() && tool_calls.is_empty() {
            continue;
        }
        cap_tool_calls(&mut tool_calls);
        let turn = Turn {
            role: "assistant".to_string(),
            text: truncate(&text, max_text),
            tool_calls,
        };
        if !turn.text.is_empty() {
            last_assistant_message = Some(turn.text.clone());
        }
        push_bounded(&mut turns, turn, keep);
    }

    Parsed {
        turns: turns.into(),
        last_assistant_message,
        saw_malformed,
    }
}

/// Parse the on-disk export JSON shape (`{info, messages}`) used by the
/// fixture + the `opencode export <id>` CLI. Pure: passes the
/// `messages` slice straight to [`parse_opencode_messages`]. A missing
/// `messages` array is a structural break — `ShapeChanged`, not
/// `Empty`. An empty `messages` array is a brand-new session — `Empty`.
///
/// **Test-only helper** — the production runtime path uses
/// [`read_opencode_messages`] (which returns `Vec<serde_json::Value>`
/// directly, with no envelope) and feeds that to
/// [`parse_opencode_messages`] without going through this wrapper. The
/// fixture + parser contract tests are the only callers; cargo's
/// `dead_code` analysis doesn't see `#[cfg(test)]` use-sites in some
/// versions, hence the `#[allow(dead_code)]`.
#[allow(dead_code)]
pub(crate) fn parse_opencode_export(export: &serde_json::Value, keep: usize) -> Parsed {
    match export.get("messages").and_then(|m| m.as_array()) {
        Some(messages) => parse_opencode_messages(messages, keep),
        None => Parsed {
            turns: Vec::new(),
            last_assistant_message: None,
            saw_malformed: true,
        },
    }
}

/// Concatenate the `text` parts of an OpenCode message, separated by
/// newlines. Reasoning parts are deliberately excluded — chain-of-
/// thought is transport plumbing, not Coordinator dialogue (matches
/// Claude's `thinking`-block skip). Returns the empty string when no
/// `text` parts exist (an assistant turn then degrades to "tool calls
/// only").
fn concat_opencode_text_parts(parts: &[serde_json::Value]) -> String {
    parts
        .iter()
        .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Pull `tool` parts out of an OpenCode message into the shared
/// [`ToolCall`] wire shape (`{name, input}`). The OpenCode export
/// places the tool name on `state.title` (e.g. `"read_file"`,
/// `"search_replace"`) and the raw input on `state.input`; output
/// lives on `state.output` but the Coordinator only consumes `input`
/// — output re-emission is a future harness-shape addition. Unknown
/// part types (`file`, `patch`, `agent`, …) are silently dropped,
/// mirroring Grok's "graceful failure on unknown event types" rule
/// (#1281).
fn extract_opencode_tool_calls(parts: &[serde_json::Value]) -> Vec<ToolCall> {
    parts
        .iter()
        .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("tool"))
        .filter_map(|p| {
            let state = p.get("state")?;
            let name = state
                .get("title")
                .and_then(|n| n.as_str())
                .or_else(|| p.get("name").and_then(|n| n.as_str()))
                .unwrap_or("")
                .to_string();
            let input = state
                .get("input")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Some(ToolCall {
                name,
                input: crate::services::transcript_reader::types::truncate_json_strings(
                    input,
                    crate::services::transcript_reader::types::MAX_TOOL_STRING,
                ),
            })
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn create_store(conn: &Connection) {
        conn.execute_batch("CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);
            CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);").unwrap();
    }

    /// Store export fixtures in the native two-table schema, not as envelopes.
    pub(crate) fn insert_message(
        conn: &Connection,
        id: &str,
        session: &str,
        created: i64,
        data: &str,
    ) {
        let value = serde_json::from_str::<serde_json::Value>(data).ok();
        let info = value.as_ref().and_then(|value| value.get("info"));
        let raw_info = info
            .map(|info| info.to_string())
            .unwrap_or_else(|| data.into());
        conn.execute(
            "INSERT INTO message VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, session, created, raw_info],
        )
        .unwrap();
        if let Some(parts) = value
            .as_ref()
            .and_then(|value| value.get("parts"))
            .and_then(|parts| parts.as_array())
        {
            for (index, part) in parts.iter().enumerate() {
                conn.execute(
                    "INSERT INTO part VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        format!("{id}-part-{index:04}"),
                        id,
                        session,
                        created,
                        part.to_string()
                    ],
                )
                .unwrap();
            }
        }
    }
}

#[cfg(test)]
mod contract_tests {
    use crate::services::transcript_reader::types::build_tail;
    fn tempfile_opencode_db(rows: &[(i64, &str, &str)]) -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        let conn = rusqlite::Connection::open(tmp.path()).expect("open temp db");
        super::test_support::create_store(&conn);
        for (idx, (time, _role, data)) in rows.iter().enumerate() {
            super::test_support::insert_message(
                &conn,
                &format!("row-{idx}"),
                "ses_fixedsid000000000000000000001",
                *time,
                data,
            );
        }
        // RAII: the connection drops when this function returns.
        drop(conn);
        tmp
    }
    use super::*;
    use crate::services::transcript_reader::test_support::fixture;
    use crate::services::transcript_reader::types::MAX_TOOL_STRING;
    use crate::services::transcript_reader::{
        read_last_assistant_message, read_tail, TranscriptFormat, TranscriptTail, UnavailableReason,
    };

    #[test]
    fn opencode_contract_parses_export_fixture() {
        let raw = std::fs::read_to_string(fixture("opencode", "opencode_export.json"))
            .expect("opencode fixture should be readable");
        let value: serde_json::Value =
            serde_json::from_str(&raw).expect("opencode fixture should be valid JSON");
        let parsed = parse_opencode_export(&value, 20);
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = build_tail(parsed)
        else {
            panic!("expected available, got ShapeChanged or Empty");
        };

        // The fixture spans user → assistant tool → user → assistant
        // (text + reasoning + tool) → assistant (text + step-finish).
        // reasoning and step-finish must not surface as turns, so the
        // surviving sequence is [user, assistant, user, assistant, assistant].
        let roles: Vec<&str> = turns.iter().map(|t| t.role.as_str()).collect();
        assert_eq!(
            roles,
            vec!["user", "assistant", "user", "assistant", "assistant"],
            "turns: {turns:#?}"
        );
        assert_eq!(turns[0].text, "Inspect src/login.ts for the redirect bug.");
        // First assistant turn: text + tool call (no reasoning, no
        // step-finish).
        assert_eq!(turns[1].text, "I'll inspect the file first.");
        assert_eq!(turns[1].tool_calls.len(), 1);
        assert_eq!(turns[1].tool_calls[0].name, "read_file");
        assert_eq!(
            turns[1].tool_calls[0].input["file_path"], "src/login.ts",
            "OpenCode `state.input` is mapped onto the shared `input` wire shape"
        );
        // Second assistant turn: reasoning (skipped), text, tool call — the
        // reasoning block must NOT pollute Turn.text.
        assert_eq!(
            turns[3].text,
            "Found it — the redirect drops the query string. Shall I apply the fix?"
        );
        assert!(
            !turns[3].text.contains("URL"),
            "reasoning content must not leak into Turn.text, got: {}",
            turns[3].text
        );
        assert_eq!(turns[3].tool_calls[0].name, "search_replace");
        assert_eq!(turns[3].tool_calls[0].input["file_path"], "src/login.ts");
        // Final assistant turn: text only, with a trailing step-finish part
        // that must be silently dropped.
        assert_eq!(
            turns[4].text,
            "The redirect now preserves the query string."
        );
        assert_eq!(turns[4].tool_calls.len(), 0);
        // The last assistant message is the whole-stream recovery — same
        // contract as every other parser.
        assert_eq!(
            last_assistant_message.as_deref(),
            Some("The redirect now preserves the query string."),
        );
    }

    #[test]
    fn opencode_digest_user_reply_at_latest_does_not_wipe_blocking_question() {
        let messages: Vec<serde_json::Value> = vec![
            serde_json::json!({
                "info": { "role": "assistant" },
                "parts": [{ "type": "text", "text": "Did the fix work?" }]
            }),
            // User reply at the latest row.
            serde_json::json!({
                "info": { "role": "user" },
                "parts": [{ "type": "text", "text": "Yes, ship it." }]
            }),
        ];
        let tail = read_opencode_digest_from_messages(&messages);
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = tail
        else {
            panic!(
                "digest must remain available when the latest row is a user reply; \
                 got {tail:?}"
            );
        };
        // Contract: digest returns Vec::new() — materialising the turn list
        // would defeat the bounded-memory optimisation.
        assert!(
            turns.is_empty(),
            "digest path must return turns: Vec::new(); got {turns:?}"
        );
        assert_eq!(
            last_assistant_message.as_deref(),
            Some("Did the fix work?"),
            "the digest must surface the assistant message BEFORE the user \
             reply — fetching only the latest row wipes the blocking question"
        );
    }

    #[test]
    fn opencode_digest_only_user_turns_returns_available_with_none() {
        let messages: Vec<serde_json::Value> = vec![
            serde_json::json!({
                "info": { "role": "user" },
                "parts": [{ "type": "text", "text": "fix the login redirect" }]
            }),
            serde_json::json!({
                "info": { "role": "user" },
                "parts": [{ "type": "text", "text": "are you there?" }]
            }),
        ];
        let tail = read_opencode_digest_from_messages(&messages);
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = tail
        else {
            panic!(
                "digest must remain Available when the agent hasn't spoken yet — \r
                 the Coordinator renders spine fields regardless of digest content; \r
                 got {tail:?}"
            );
        };
        assert!(
            turns.is_empty(),
            "digest path must return turns: Vec::new()"
        );
        assert!(
            last_assistant_message.is_none(),
            "no assistant message in the window —> last_assistant_message: None"
        );
    }

    #[test]
    fn opencode_digest_reasoning_only_assistant_with_user_turns_returns_available() {
        let messages: Vec<serde_json::Value> = vec![
            serde_json::json!({
                "info": { "role": "user" },
                "parts": [{ "type": "text", "text": "think about it" }]
            }),
            serde_json::json!({
                "info": { "role": "assistant" },
                "parts": [{ "type": "reasoning", "text": "pondering..." }]
            }),
            serde_json::json!({
                "info": { "role": "user" },
                "parts": [{ "type": "text", "text": "any answer?" }]
            }),
        ];
        let tail = read_opencode_digest_from_messages(&messages);
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = tail
        else {
            panic!(
                "reasoning-only assistant with user turns must NOT degrade — \
                 node is actively working; got {tail:?}"
            );
        };
        assert!(turns.is_empty());
        assert!(
            last_assistant_message.is_none(),
            "reasoning content is transport plumbing, not dialogue"
        );
    }

    #[test]
    fn opencode_digest_truly_empty_window_degrades_to_empty() {
        let messages: Vec<serde_json::Value> = vec![];
        let tail = read_opencode_digest_from_messages(&messages);
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::Empty),
            "an empty `messages` window represents a brand-new session that \
             hasn't reached its first user prompt"
        );
    }

    #[test]
    fn opencode_tail_from_messages_keeps_rolling_tail_in_timeline_order() {
        let messages: Vec<serde_json::Value> = (0..50)
            .map(|i| {
                serde_json::json!({
                    "info": { "role": if i % 2 == 0 { "user" } else { "assistant" } },
                    "parts": [{ "type": "text", "text": format!("msg #{i}") }]
                })
            })
            .collect();
        let tail = read_opencode_tail_from_messages(&messages, 2);
        let TranscriptTail::Available { turns, .. } = tail else {
            panic!("expected available, got {tail:?}");
        };
        assert_eq!(turns.len(), 2);
        // Last two turns are user(48) and assistant(49).
        assert_eq!(turns[0].text, "msg #48");
        assert_eq!(turns[1].text, "msg #49");
    }

    #[test]
    fn opencode_renamed_role_field_degrades_to_shape_changed() {
        let value = serde_json::json!({
            "info": { "id": "ses_abc" },
            "messages": [
                { "info": { "role": "human" }, "parts": [] },
                { "info": { "role": "user", "content": "ok" }, "parts": [{"type": "text", "text": "hi"}] },
            ]
        });
        let parsed = parse_opencode_export(&value, 10);
        // The malformed message is flagged; the well-formed one still
        // surfaces. The malformed flag trips `build_tail`'s
        // `empty_or_shape_changed` only when no turns survive — with a
        // surviving turn, the result is `Available` carrying the recovered
        // turn + the `saw_malformed` flag is preserved for callers that
        // want to surface it. The contract test pins the surviving path;
        // the "all broken" path is pinned separately below.
        assert!(parsed.saw_malformed, "renamed role is malformed");
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].text, "hi");
    }

    #[test]
    fn opencode_all_broken_messages_degrade_to_shape_changed() {
        let value = serde_json::json!({
            "info": { "id": "ses_abc" },
            "messages": [
                { "info": { "type": "user" }, "parts": [] }, // role missing
                { "info": { "role": "system" }, "parts": [] }, // role unknown
            ]
        });
        let tail = build_tail(parse_opencode_export(&value, 10));
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::ShapeChanged),
            "all-broken messages must degrade as ShapeChanged"
        );
    }

    #[test]
    fn opencode_missing_messages_field_degrades_to_shape_changed() {
        let value = serde_json::json!({ "info": { "id": "ses_abc" } });
        let tail = build_tail(parse_opencode_export(&value, 10));
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::ShapeChanged),
            "missing top-level `messages` is a structural break"
        );
    }

    #[test]
    fn opencode_empty_messages_array_degrades_to_empty() {
        let value = serde_json::json!({
            "info": { "id": "ses_abc" },
            "messages": []
        });
        let tail = build_tail(parse_opencode_export(&value, 10));
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::Empty),
            "zero messages is a quiet session, not a shape break"
        );
    }

    #[test]
    fn opencode_unknown_part_types_are_silently_skipped() {
        let value = serde_json::json!({
            "info": { "id": "ses_abc" },
            "messages": [
                {
                    "info": { "role": "assistant" },
                    "parts": [
                        { "type": "file", "url": "https://example.com/spec.md" },
                        { "type": "patch", "hash": "abc123" },
                        { "type": "agent", "source": { "value": "plan" } },
                        { "type": "step-start", "snapshot": "snap-1" },
                        { "type": "text", "text": "Real reply." },
                    ]
                }
            ]
        });
        let parsed = parse_opencode_export(&value, 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].text, "Real reply.");
        assert!(
            !parsed.saw_malformed,
            "unknown part types must not flag malformed, got: {parsed:?}"
        );
    }

    #[test]
    fn opencode_reasoning_only_assistant_turn_is_dropped() {
        let value = serde_json::json!({
            "info": { "id": "ses_abc" },
            "messages": [
                {
                    "info": { "role": "assistant" },
                    "parts": [
                        { "type": "reasoning", "text": "I should think about this carefully." },
                    ]
                }
            ]
        });
        let tail = build_tail(parse_opencode_export(&value, 10));
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::Empty),
            "reasoning-only turn produces no dialogue, so the file is empty"
        );
    }

    #[test]
    fn opencode_rolling_buffer_retains_only_last_keep() {
        let mut messages = Vec::new();
        for i in 0..50 {
            messages.push(serde_json::json!({
                "info": { "role": "user" },
                "parts": [{"type": "text", "text": format!("prompt {i}")}]
            }));
            messages.push(serde_json::json!({
                "info": { "role": "assistant" },
                "parts": [{"type": "text", "text": format!("reply {i}")}]
            }));
        }
        let value = serde_json::json!({ "info": { "id": "ses_abc" }, "messages": messages });
        let parsed = parse_opencode_export(&value, 2);
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].text, "prompt 49");
        assert_eq!(parsed.turns[1].text, "reply 49");
        assert_eq!(
            parsed.last_assistant_message.as_deref(),
            Some("reply 49"),
            "last assistant message survives eviction of older turns"
        );
    }

    #[test]
    fn opencode_tool_input_truncates_large_string_leaves() {
        let big = "x".repeat(MAX_TOOL_STRING + 50);
        let value = serde_json::json!({
            "info": { "id": "ses_abc" },
            "messages": [{
                "info": { "role": "assistant" },
                "parts": [{
                    "type": "tool",
                    "state": {
                        "status": "completed",
                        "input": { "path": "src/main.rs", "content": big },
                        "output": "ok",
                        "title": "write_file"
                    }
                }]
            }]
        });
        let parsed = parse_opencode_export(&value, 10);
        let call = &parsed.turns[0].tool_calls[0];
        assert_eq!(call.name, "write_file");
        let content = call.input["content"].as_str().unwrap();
        assert!(content.ends_with('…'), "large args body must be truncated");
        assert!(content.chars().count() <= MAX_TOOL_STRING + 1);
    }

    #[test]
    fn opencode_tool_name_falls_back_to_part_name_when_state_title_missing() {
        let value = serde_json::json!({
            "info": { "id": "ses_abc" },
            "messages": [{
                "info": { "role": "assistant" },
                "parts": [{
                    "type": "tool",
                    "name": "bash",
                    "state": {
                        "status": "completed",
                        "input": { "command": "ls" },
                        "output": ""
                    }
                }]
            }]
        });
        let parsed = parse_opencode_export(&value, 10);
        assert_eq!(parsed.turns[0].tool_calls[0].name, "bash");
    }

    #[test]
    fn opencode_transcript_format_for_harness_routes_opencode() {
        assert_eq!(
            TranscriptFormat::for_harness("opencode"),
            Some(TranscriptFormat::OpenCode),
            "the dispatch table must include the OpenCode variant"
        );
    }

    fn opencode_text_message(role: &str, text: &str) -> String {
        serde_json::json!({
            "info": { "role": role, "time": { "created": 1 } },
            "parts": [{ "type": "text", "text": text }]
        })
        .to_string()
    }

    #[test]
    fn opencode_locator_reads_messages_from_file_backed_db() {
        let tmp = tempfile_opencode_db(&[
            (
                100,
                "user",
                &opencode_text_message("user", "Inspect src/login.ts."),
            ),
            (
                200,
                "assistant",
                &opencode_text_message("assistant", "Looking now."),
            ),
            // Row for a *different* session id — must not leak in. We
            // re-open and write to that session id below.
        ]);
        let conn = rusqlite::Connection::open(tmp.path()).expect("reopen for foreign row");
        super::test_support::insert_message(
            &conn,
            "foreign",
            "ses_othersessionid0000000000001",
            150,
            &opencode_text_message("user", "this should not surface"),
        );
        drop(conn);

        let messages = read_opencode_messages(tmp.path(), "ses_fixedsid000000000000000000001", 10)
            .expect("locator should return a value");
        let parsed = parse_opencode_messages(&messages, 10);
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].role, "user");
        assert_eq!(parsed.turns[0].text, "Inspect src/login.ts.");
        assert_eq!(parsed.turns[1].role, "assistant");
        assert_eq!(parsed.turns[1].text, "Looking now.");
        assert_eq!(
            parsed.last_assistant_message.as_deref(),
            Some("Looking now.")
        );
        for turn in &parsed.turns {
            assert!(
                !turn.text.contains("this should not surface"),
                "session-id filter leaked a row from the other session id"
            );
        }
    }

    #[test]
    fn circuit_opencode_report_identity_survives_restart_and_identical_replies() {
        let session = "ses_fixedsid000000000000000000001";
        let tmp = tempfile_opencode_db(&[
            (
                100,
                "assistant",
                &opencode_text_message("assistant", "Done."),
            ),
            (200, "user", &opencode_text_message("user", "Finish now")),
        ]);
        let first = OpenCodeAdapter
            .assistant_report(tmp.path(), session)
            .unwrap();
        assert_eq!(first.text, "Done.");
        let conn = rusqlite::Connection::open(tmp.path()).unwrap();
        super::test_support::insert_message(
            &conn,
            "new-reply",
            session,
            300,
            &opencode_text_message("assistant", "Done."),
        );
        drop(conn);
        let second = OpenCodeAdapter
            .assistant_report(tmp.path(), session)
            .unwrap();
        assert_eq!(second.text, first.text);
        assert_ne!(second.revision, first.revision);
        assert_eq!(
            OpenCodeAdapter
                .assistant_report(tmp.path(), session)
                .unwrap()
                .revision,
            second.revision
        );
    }

    #[test]
    fn opencode_locator_returns_empty_for_unknown_session() {
        let tmp = tempfile_opencode_db(&[(100, "user", &opencode_text_message("user", "hi"))]);
        let messages =
            read_opencode_messages(tmp.path(), "ses_unknown0000000000000000000000001", 10)
                .expect("locator must not error when the session has no rows");
        let parsed = parse_opencode_messages(&messages, 10);
        assert_eq!(
            parsed,
            Parsed {
                turns: Vec::new(),
                last_assistant_message: None,
                saw_malformed: false,
            },
            "an unknown session id is a quiet session, not a shape break"
        );
    }

    #[test]
    fn opencode_locator_preserves_malformed_rows() {
        let tmp = tempfile_opencode_db(&[
            (100, "user", "not valid json"),
            (200, "user", &opencode_text_message("user", "good")),
        ]);
        let messages = read_opencode_messages(tmp.path(), "ses_fixedsid000000000000000000001", 10)
            .expect("locator must not error on a bad row");
        let parsed = parse_opencode_messages(&messages, 10);
        assert_eq!(
            messages.len(),
            2,
            "corrupt records must not disappear from the snapshot"
        );
        assert!(parsed.saw_malformed);
        assert_eq!(
            parsed.turns.len(),
            1,
            "the display can still show readable records"
        );
        assert_eq!(parsed.turns[0].text, "good");
    }

    #[test]
    fn opencode_locator_returns_rows_in_ascending_order() {
        let tmp = tempfile_opencode_db(&[
            (0, "user", &opencode_text_message("user", "first ever row")),
            (
                50_000,
                "assistant",
                &opencode_text_message("assistant", "early assistant"),
            ),
            (
                100_000,
                "user",
                &opencode_text_message("user", "near latest 2"),
            ),
            (
                150_000,
                "assistant",
                &opencode_text_message("assistant", "latest 1"),
            ),
            (200_000, "user", &opencode_text_message("user", "latest 2")),
        ]);
        let messages = read_opencode_messages(tmp.path(), "ses_fixedsid000000000000000000001", 3)
            .expect("locator must accept the bounded row_budget");
        assert_eq!(
            messages.len(),
            3,
            "locator must cap the row fetch at the parser's row_budget"
        );
        // SQL emits DESC LIMIT 3 → [latest 2 (200_000), latest 1 (150_000),
        // near latest 2 (100_000)]. After the Rust-side reverse the
        // natural timeline is restored: ascending time order.
        assert!(messages[0].to_string().contains("near latest 2"));
        assert!(messages[1].to_string().contains("latest 1"));
        assert!(messages[2].to_string().contains("latest 2"));
    }

    #[test]
    fn opencode_locator_short_circuits_non_ses_ids() {
        let tail = read_tail(
            TranscriptFormat::OpenCode,
            Some("not-an-opencode-id"),
            "/home/adam/src/proj",
            10,
        );
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::NoTranscript),
            "non-`ses_` ids must short-circuit before any disk read"
        );
        let tail = read_last_assistant_message(
            TranscriptFormat::OpenCode,
            Some("not-an-opencode-id"),
            "/home/adam/src/proj",
        );
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::NoTranscript),
            "non-`ses_` ids must short-circuit on the digest path too"
        );
        // A missing session_id is the supported-provider-but-no-session
        // state, distinct from `NoTranscript`.
        let tail = read_tail(TranscriptFormat::OpenCode, None, "/home/adam/src/proj", 10);
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::NoSession),
            "missing session id is NoSession, not NoTranscript"
        );
        let tail =
            read_last_assistant_message(TranscriptFormat::OpenCode, None, "/home/adam/src/proj");
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::NoSession),
            "missing session id is NoSession on the digest path too"
        );
    }
}

#[cfg(test)]
mod store_contract_tests {
    use super::test_support::{create_store, insert_message};
    use super::*;
    use crate::services::transcript_reader::types::build_tail;

    #[test]
    fn reader_handles_tail_digest_malformed_empty_shape_changed_and_unreadable_stores() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let db = Connection::open(&path).unwrap();
        create_store(&db);
        let value = serde_json::json!({"info":{"role":"assistant"},"parts":[{"type":"text","text":"OpenCode blocking question?"}]});
        insert_message(&db, "msg-1", "ses-contract", 1, &value.to_string());
        assert_eq!(
            OpenCodeAdapter
                .read_tail(&path, "ses-contract", 1)
                .unwrap()
                .last_assistant_message
                .as_deref(),
            Some("OpenCode blocking question?")
        );
        assert_eq!(
            OpenCodeAdapter
                .last_assistant_message(&path, "ses-contract")
                .unwrap()
                .last_assistant_message
                .as_deref(),
            Some("OpenCode blocking question?")
        );
        assert_eq!(
            OpenCodeAdapter
                .assistant_report(&path, "ses-contract")
                .unwrap()
                .text,
            "OpenCode blocking question?"
        );
        insert_message(&db, "msg-broken", "ses-contract", 2, "not json");
        assert_eq!(
            OpenCodeAdapter
                .last_assistant_message(&path, "ses-contract")
                .unwrap()
                .last_assistant_message
                .as_deref(),
            Some("OpenCode blocking question?")
        );
        assert_eq!(
            build_tail(OpenCodeAdapter.read_tail(&path, "ses-empty", 1).unwrap()),
            TranscriptTail::unavailable(UnavailableReason::Empty)
        );
        insert_message(
            &db,
            "msg-changed",
            "ses-changed",
            3,
            "{\"info\":{\"author\":\"assistant\"},\"parts\":[]}",
        );
        assert_eq!(
            build_tail(OpenCodeAdapter.read_tail(&path, "ses-changed", 1).unwrap()),
            TranscriptTail::unavailable(UnavailableReason::ShapeChanged)
        );
        for bad in [dir.path().join("missing.db"), dir.path().to_path_buf()] {
            assert_eq!(
                OpenCodeAdapter.read_tail(&bad, "ses-contract", 1),
                Err(UnavailableReason::Unreadable)
            );
            assert_eq!(
                OpenCodeAdapter.last_assistant_message(&bad, "ses-contract"),
                Err(UnavailableReason::Unreadable)
            );
        }
    }
}
