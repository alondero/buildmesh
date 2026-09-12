//! OpenCode transcript adapter (issue #1296).
//!
//! OpenCode stores all session messages in a single local SQLite database
//! (`~/.local/share/opencode/opencode.db`), not in per-session JSONL files.
//! The reader bypasses the file-based `locate`/`parse` chain entirely:
//! `locate` returns `None`, `parse` is never reached. The reader's
//! `read_tail` / `read_last_assistant_message` / `read_assistant_report`
//! short-circuit on `OpenCodeAdapter` and call
//! `read_opencode_tail` / `read_opencode_digest` / `opencode_assistant_report`
//! directly.
//!
//! Issue #1661 step 7: OpenCode is the **fifth harness migrated end-to-end**.
//! The entire OpenCode reader surface — constants + DB read paths +
//! message-shape parser + helper functions + the session-id gate +
//! the env-aware DB-path resolver — moves here. The capture poller
//! (`services::opencode_session`) imports both helpers from this
//! adapter so the two readers (transcript + session-id capture poller)
//! cannot drift on what an OpenCode session id looks like or where its
//! DB lives.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::agent::session_lifecycle::LifecycleKind;
use crate::env;
use crate::models::EnvType;
use crate::services::transcript_reader::adapter::{
    HookClassification, HookDecision, LocateCtx, TranscriptAdapter,
};
use crate::services::transcript_reader::types::{
    build_tail, cap_tool_calls, effective_tail, empty_or_shape_changed, push_bounded, truncate,
    Parsed, ToolCall, TranscriptTail, Turn, UnavailableReason, MAX_TURN_TEXT,
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
        EnvType::WindowsInterop => crate::env::windows_cli_home(".local/share/opencode/opencode.db"),
        EnvType::Wsl => {
            let linux = crate::env::wsl_home()?.join(".local/share/opencode/opencode.db");
            Some(PathBuf::from(crate::env::to_host_path(&linux.to_string_lossy())))
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
/// `locate` returns `None` and `parse` is unreachable because the reader
/// short-circuits on `id() == "opencode"` to call the SQLite reader
/// directly (see [`read_tail`], [`read_last_assistant_message`], and
/// [`read_assistant_report`] in the parent module).
pub(crate) struct OpenCodeAdapter;

impl TranscriptAdapter for OpenCodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn locate(&self, _ctx: LocateCtx<'_>) -> Option<PathBuf> {
        // OpenCode has no per-session transcript file (issue #1296); the
        // reader routes through `read_opencode_*` instead. Returning
        // `None` is the documented marker for "this adapter is
        // database-backed".
        None
    }

    fn parse(&self, _lines: Box<dyn Iterator<Item = String> + '_>, _keep: usize) -> Parsed {
        // Unreachable: the reader short-circuits before this is called.
        Parsed {
            turns: Vec::new(),
            last_assistant_message: None,
            saw_malformed: false,
        }
    }

    fn line_has_assistant_text(&self, _line: &str) -> bool {
        // OpenCode's per-line JSON is the SQLite `message.data` blob, not
        // a file-based JSONL line — the digest window uses
        // `read_opencode_digest` instead, so this is unreachable in
        // practice.
        false
    }

    fn classify_hook(
        &self,
        body: &[u8],
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
        let payload: serde_json::Value = serde_json::from_slice(body).ok()?;
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
/// up-to-`row_budget` newest rows in chronological order (oldest →
/// newest), matching how `opencode export <id>` orders them. Returns
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
            "SELECT id, data FROM message \
             WHERE session_id = ?1 \
             ORDER BY time_created DESC \
             LIMIT ?2",
        )
        .ok()?;
    let mut latest = Vec::new();
    let mut rows = stmt
        .query(rusqlite::params![session_id, row_budget as i64])
        .ok()?;
    while let Some(row) = rows.next().ok()? {
        let id: String = row.get(0).ok()?;
        let data: String = row.get(1).ok()?;
        // Each row's `data` is one message record. We accept any JSON
        // shape here — structural validation lives in the parser so an
        // unknown shape degrades as `ShapeChanged`, not a panic. Rows
        // that aren't valid JSON are silently dropped (graceful failure
        // on bad rows).
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) {
            latest.push((id, value));
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
pub(crate) fn read_opencode_tail_from_messages(
    messages: &[serde_json::Value],
    tail: usize,
) -> TranscriptTail {
    build_tail(parse_opencode_messages(messages, effective_tail(tail)))
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
pub(crate) fn read_opencode_digest_from_messages(
    messages: &[serde_json::Value],
) -> TranscriptTail {
    let parsed = parse_opencode_messages(messages, 1);
    // Only degrade when the parser saw no turns at all — i.e., empty
    // window (brand-new session before the user's first prompt) or
    // every line was malformed (ShapeChanged). An active node with
    // `parsed.turns = [user turn]` is fine: the digest records `None`
    // for `last_assistant_message` until the agent speaks.
    if parsed.turns.is_empty() {
        return TranscriptTail::unavailable(empty_or_shape_changed(parsed.saw_malformed));
    }
    TranscriptTail::Available {
        turns: Vec::new(),
        last_assistant_message: parsed.last_assistant_message,
    }
}

/// Open the OpenCode SQLite DB and read the latest `tail` turns (with a
/// `factor` row budget to span assistant coalescing) for a session_id.
/// Wraps [`read_opencode_tail_from_messages`] over the rows the locator
/// returns, and degrades through the same [`UnavailableReason`] ladder
/// as every other harness so the Coordinator's `/nodes/{id}/log`
/// endpoint sees a uniform error surface.
pub(crate) fn read_opencode_tail(
    session_id: Option<&str>,
    node_path: &str,
    tail: usize,
) -> TranscriptTail {
    let (db_path, session_id) = match opencode_resolve(session_id, node_path) {
        Ok(pair) => pair,
        Err(reason) => return TranscriptTail::unavailable(reason),
    };
    let row_budget = effective_tail(tail).saturating_mul(OPENCODE_TURN_TO_MESSAGE_FACTOR);
    let Some(messages) = read_opencode_messages(&db_path, session_id, row_budget) else {
        return TranscriptTail::unavailable(UnavailableReason::Unreadable);
    };
    read_opencode_tail_from_messages(&messages, tail)
}

/// Coordinator digest read path (`GET /nodes`). Resolves the env-aware
/// DB path, fetches a fixed-size window (independent of the caller-
/// supplied `tail`, which is ignored on every digest path), and shapes
/// the result through [`read_opencode_digest_from_messages`]. Mirrors
/// the AGY split: test the semantics with explicit messages, drive the
/// dispatch through this.
pub(crate) fn read_opencode_digest(
    session_id: Option<&str>,
    node_path: &str,
) -> TranscriptTail {
    let (db_path, session_id) = match opencode_resolve(session_id, node_path) {
        Ok(pair) => pair,
        Err(reason) => return TranscriptTail::unavailable(reason),
    };
    let Some(messages) = read_opencode_messages(&db_path, session_id, OPENCODE_DIGEST_WINDOW)
    else {
        return TranscriptTail::unavailable(UnavailableReason::Unreadable);
    };
    read_opencode_digest_from_messages(&messages)
}

/// Shared session-id + DB-path resolver for both OpenCode read paths.
/// Returns `Some((PathBuf, String))` on success; `None` on NoSession /
/// NoTranscript, with `session_id: None` on the error path so the
/// caller can map back to the right [`UnavailableReason`].
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
pub(crate) fn parse_opencode_messages(
    messages: &[serde_json::Value],
    keep: usize,
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
                    text: truncate(&text, MAX_TURN_TEXT),
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
            text: truncate(&text, MAX_TURN_TEXT),
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
pub(crate) fn parse_opencode_export(
    export: &serde_json::Value,
    keep: usize,
) -> Parsed {
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
            let input = state.get("input").cloned().unwrap_or(serde_json::Value::Null);
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
