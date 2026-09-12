//! Muse Code transcript adapter (issue #1708).
//!
//! Muse writes per-session JSONL at
//! `~/.local/share/muse/sessions/YYYY/MM/DD/<session-id>/session.jsonl`. The
//! path is recorded in `~/.local/share/muse/session-index.db` (a SQLite index
//! with `sessions(session_id TEXT, session_log_path TEXT)`). The reader walks
//! the index (env-aware, WSL-translated) and parses the per-session JSONL into
//! the shared `Turn` / `ToolCall` wire shape — same approach as the AGY
//! adapter (issue #1283), which is also file-based after a SQLite index
//! lookup.
//!
//! ## On-disk record shape
//!
//! Each JSONL line is a `record` with `payload_type` at the top level and
//! `event` nested under `payload`. Recognised transcript events:
//!
//! - `payload_type = "runtime.session"`, `event.kind = "user_prompt_display"`
//!   → user turn (`event.prompt`)
//! - `payload_type = "runtime.session"`, `event.kind = "started"` *with*
//!   `event.prompt` → fallback user turn (defensive; some sessions emit a
//!   bare `started` first then a `user_prompt_display`)
//! - `payload_type = "runtime.session"`,
//!   `event.kind = "assistant_message_committed"` → assistant text
//!   (`event.text`)
//! - `payload_type = "runtime.session"`,
//!   `event.kind = "assistant_tool_calls_committed"` → assistant tool calls
//!   (`event.tool_calls: [{name, args, call_id, id}]`)
//!
//! ## Skipped event kinds (forward-compat discipline)
//!
//! - `runtime.session.task` lifecycle (`started`/`completed`/`failed`/…):
//!   task plumbing, not dialogue.
//! - `runtime.session.metadata` / `route_facts`: per-session bootstrap
//!   records, already consumed by session-id recovery.
//! - `runtime.session` with `event.kind = "reasoning_committed"`: per the
//!   Muse export docs the reasoning payload is "verbatim encrypted" — the
//!   digest must not surface it (issue #1708 privacy rule).
//! - `runtime.session` with `event.kind = "output"`: tool execution output,
//!   not assistant text.
//! - `runtime.session` with `event.kind = "tool_result_batch_committed"`:
//!   tool results, not dialogue.
//! - Any unknown `payload_type` or unknown `event.kind`: silently dropped,
//!   the "graceful failure on unknown event types" rule used by Grok (#1281)
//!   and OpenCode (#1296).
//!
//! ## Coalescing
//!
//! Consecutive assistant lines sharing `event.message_id` merge into one
//! `Turn` (text + tool calls), mirroring Claude Code's `message.id`
//! coalescing (`adapters/claude_code.rs`). Defensive — if `message_id` is
//! absent (a future muse release drops the field), each line emits its own
//! turn; nothing breaks.
//!
//! ## Live-parse vs `muse export` subprocess
//!
//! The reader does NOT shell out to `muse export`. The export schema
//! (`export_schema_version 1`) is a superset of the on-disk `session.jsonl`
//! events, so live-parsing is faster, avoids a subprocess per read, and
//! respects the project's hard rule against subprocess work while holding a
//! DB connection or writer mutex.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::env;
use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::{
    cap_tool_calls, merge_into, push_bounded, truncate, truncate_json_strings, Parsed, ToolCall,
    Turn, MAX_TOOL_STRING, MAX_TURN_TEXT,
};

/// SQLite busy_timeout the muse reader applies on every open: lets a
/// concurrent writer (the live muse process committing a new line) hold
/// the lock briefly instead of returning `SQLITE_BUSY`. Mirrors the
/// existing `find_session` value at `agent/provider/adapters/muse.rs:152`
/// — bounded to 200 ms so a Coordinator digest poll cannot park a Tokio
/// worker thread indefinitely if the live harness is writing a new line.
const MUSE_READER_BUSY_TIMEOUT_MS: u64 = 200;

/// Drop-in [`TranscriptAdapter`] for Muse Code.
pub(crate) struct MuseAdapter;

impl TranscriptAdapter for MuseAdapter {
    fn id(&self) -> &'static str {
        "muse"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        let index = muse_index_path_for(ctx.node_path)?;
        muse_locator_in(&index, ctx.session_id)
    }

    fn parse(&self, lines: Box<dyn Iterator<Item = String> + '_>, keep: usize) -> Parsed {
        parse_muse_turns(lines, keep)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        // Cheap per-line check used by the digest reader's bounded tail
        // window (issue #341). The recognised shape is
        // `payload_type=runtime.session` AND
        // `payload.event.kind=assistant_message_committed` AND
        // `payload.event.text` non-empty after trim. Anything else — a
        // `started`, a tool-call commit, reasoning, output — is not
        // "assistant text" for the digest and falls through to a full
        // re-parse.
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        if value.get("payload_type").and_then(|p| p.as_str()) != Some("runtime.session") {
            return false;
        }
        let event = value.pointer("/payload/event");
        let Some(kind) = event.and_then(|e| e.get("kind")).and_then(|k| k.as_str()) else {
            return false;
        };
        if kind != "assistant_message_committed" {
            return false;
        }
        event
            .and_then(|e| e.get("text"))
            .and_then(|t| t.as_str())
            .is_some_and(|text| !text.trim().is_empty())
    }
}

/// Resolve the on-disk SQLite index path Muse uses for session recovery,
/// env-aware (WSL / Windows / WindowsInterop). Mirrors the path resolution
/// in `agent/provider/adapters/muse.rs::recover_suspended_session_id`
/// (line 108) — same `cli_dir_for_spawn` helper, same guest-relative
/// directory, same Windows host-path translation when running on WSL.
///
/// `pub(crate)` so the contract test can drive the path with a custom
/// root (the existing `find_session` keeps its own copy; refactor is a
/// follow-up — see plan §Out of scope).
pub(crate) fn muse_index_path_for(node_path: &str) -> Option<PathBuf> {
    let native = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(std::path::PathBuf::from)?
        .join(".local/share/muse");
    env::cli_dir_for_spawn(native, ".local/share/muse", node_path)
        .map(|p| p.join("session-index.db"))
}

/// Look up a session's log path in `session-index.db`. Returns `None`
/// when the DB is missing, the id is unknown, or the path column is NULL
/// (the column is nullable per the on-disk schema — see
/// `adapters/muse.rs::find_session`). `pub(crate)` so tests can drive it
/// against a temp DB without touching `~/.local/share/muse`.
///
/// The returned path is the guest-side path stored in the index (Linux
/// absolute, e.g. `/home/alond/.local/share/muse/...`); on Windows the
/// caller must wrap it in [`crate::env::to_host_path`] to read it from
/// the host. The reader relies on `cli_dir_for_spawn` (called upstream
/// from `locate`) to translate the *index* path; this helper returns the
/// raw guest path the index stores for symmetry with the existing
/// `find_session`.
pub(crate) fn muse_locator_in(index_db: &Path, session_id: &str) -> Option<PathBuf> {
    let connection = Connection::open_with_flags(index_db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    if connection
        .busy_timeout(Duration::from_millis(MUSE_READER_BUSY_TIMEOUT_MS))
        .is_err()
    {
        // best-effort — fall through with the default behaviour; a
        // non-zero busy_timeout would have helped if the live harness
        // is mid-write, but absence of one is not fatal.
    }
    let mut statement = connection
        .prepare("SELECT session_log_path FROM sessions WHERE session_id = ?1 LIMIT 1")
        .ok()?;
    let mut rows = statement
        .query(rusqlite::params![session_id])
        .ok()?;
    let row = rows.next().ok()?;
    let path: String = row?.get(0).ok()?;
    if path.is_empty() {
        return None;
    }
    Some(PathBuf::from(path))
}

/// Pull Muse `tool_calls` (`[{name, args, call_id, id}]`) into the
/// shared [`ToolCall`] wire shape. `args` is a JSON string (matches
/// Grok's `arguments`-as-text convention at `adapters/grok.rs:208-214`);
/// parse it through `serde_json::from_str` falling back to the raw text
/// so a malformed `args` doesn't drop the whole tool call.
fn extract_muse_tool_calls(value: Option<&serde_json::Value>) -> Vec<ToolCall> {
    let Some(serde_json::Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let name = obj
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let input = obj
                .get("args")
                .map(|value| match value {
                    serde_json::Value::String(text) => serde_json::from_str(text)
                        .unwrap_or_else(|_| value.clone()),
                    _ => value.clone(),
                })
                .unwrap_or(serde_json::Value::Null);
            Some(ToolCall {
                name,
                input: truncate_json_strings(input, MAX_TOOL_STRING),
            })
        })
        .collect()
}

/// Parse Muse session.jsonl lines into logical turns, honouring the same
/// [`Parsed`] contract as every other harness parser: rolling `keep`-
/// bounded turn window, whole-stream `last_assistant_message` tracking,
/// `saw_malformed` flag for renamed-shape detection.
///
/// Coalescing: consecutive assistant lines (`assistant_message_committed`
/// or `assistant_tool_calls_committed`) sharing `event.message_id` merge
/// into one turn. Lines without a `message_id` (or with a different id)
/// each emit their own turn — defensive so a future muse release that
/// drops `message_id` doesn't silently widen the turn window.
pub(crate) fn parse_muse_turns(lines: impl Iterator<Item = String>, keep: usize) -> Parsed {
    let keep = keep.max(1);
    let mut turns: VecDeque<Turn> = VecDeque::new();
    let mut last_assistant_message: Option<String> = None;
    let mut saw_malformed = false;
    // Tracks the `message_id` of the open assistant turn so a continuation
    // line coalesces into it (mirrors Claude Code's `open_assistant_id`
    // pattern at `adapters/claude_code.rs:156`). `None` means "no open
    // assistant turn" — either we just emitted a user turn, or we never
    // saw an assistant line yet.
    let mut open_message_id: Option<String> = None;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        // Fast path: only `runtime.session` events carry dialogue; every
        // other `payload_type` is plumbing. This matches the AGY
        // `match val.get("source")` discipline at `adapters/agy.rs:102`.
        if value.get("payload_type").and_then(|p| p.as_str()) != Some("runtime.session") {
            continue;
        }
        let Some(event) = value.pointer("/payload/event") else {
            saw_malformed = true;
            continue;
        };
        let Some(kind) = event.get("kind").and_then(|k| k.as_str()) else {
            saw_malformed = true;
            continue;
        };
        match kind {
            // --- User turns --------------------------------------------------
            "user_prompt_display" => {
                let Some(prompt) = event.get("prompt").and_then(|p| p.as_str()) else {
                    saw_malformed = true;
                    continue;
                };
                if prompt.trim().is_empty() {
                    continue;
                }
                // A user turn closes any open assistant coalescing window.
                open_message_id = None;
                push_bounded(
                    &mut turns,
                    Turn {
                        role: "user".to_string(),
                        text: truncate(prompt, MAX_TURN_TEXT),
                        tool_calls: Vec::new(),
                    },
                    keep,
                );
            }
            "started" => {
                // Defensive fallback: some sessions emit `started` carrying
                // `prompt` instead of (or in addition to) `user_prompt_display`.
                // Only treat as a user turn when `prompt` is present and
                // non-empty — pure task-lifecycle `started` events
                // (`payload_type=runtime.session.task`) are filtered by the
                // outer guard above and never reach this arm.
                let Some(prompt) = event.get("prompt").and_then(|p| p.as_str()) else {
                    continue;
                };
                if prompt.trim().is_empty() {
                    continue;
                }
                open_message_id = None;
                push_bounded(
                    &mut turns,
                    Turn {
                        role: "user".to_string(),
                        text: truncate(prompt, MAX_TURN_TEXT),
                        tool_calls: Vec::new(),
                    },
                    keep,
                );
            }
            // --- Assistant turns ---------------------------------------------
            "assistant_message_committed" => {
                let text = event.get("text").and_then(|t| t.as_str()).unwrap_or("");
                let id = event
                    .get("message_id")
                    .and_then(|m| m.as_str())
                    .map(str::to_string);
                coalesce_or_open_assistant(
                    &mut turns,
                    &mut last_assistant_message,
                    &mut open_message_id,
                    id.as_deref(),
                    text,
                    Vec::new(),
                    keep,
                );
            }
            "assistant_tool_calls_committed" => {
                let mut tool_calls = extract_muse_tool_calls(event.get("tool_calls"));
                cap_tool_calls(&mut tool_calls);
                let id = event
                    .get("message_id")
                    .and_then(|m| m.as_str())
                    .map(str::to_string);
                coalesce_or_open_assistant(
                    &mut turns,
                    &mut last_assistant_message,
                    &mut open_message_id,
                    id.as_deref(),
                    "",
                    std::mem::take(&mut tool_calls),
                    keep,
                );
            }
            // --- Silently skipped (privacy + plumbing) -----------------------
            "reasoning_committed" => {
                // PRIVACY: per the Muse export docs, the reasoning payload
                // is "verbatim encrypted" — refuse to surface it in the
                // digest. If a future muse release decodes this client-
                // side, the parser must still skip (and only an explicit
                // issue can add a follow-up that surfaces a decoded
                // preview, with a dedicated redaction audit). Mirrors the
                // AGY "graceful failure on unknown event types" rule.
            }
            "output" | "tool_result_batch_committed" => {
                // Tool execution output / tool result batches are not
                // dialogue. Skip silently.
            }
            _ => {
                // Unknown event kind on a recognised `runtime.session`
                // envelope — silently skip (forward-compat discipline;
                // same rule as Grok #1281 and OpenCode #1296).
            }
        }
    }
    Parsed {
        turns: turns.into(),
        last_assistant_message,
        saw_malformed,
    }
}

/// Helper for the two assistant-side event kinds: either merge into the
/// open assistant turn (when `message_id` matches) or open a new one.
/// An empty `text` + empty `tool_calls` is a no-op (mirrors Claude
/// Code's thinking-only line drop at `adapters/claude_code.rs:206-209`).
fn coalesce_or_open_assistant(
    turns: &mut VecDeque<Turn>,
    last_assistant_message: &mut Option<String>,
    open_message_id: &mut Option<String>,
    new_id: Option<&str>,
    text: &str,
    mut more_tools: Vec<ToolCall>,
    keep: usize,
) {
    // No-op guard — an assistant line with neither text nor tool calls
    // carries nothing the Coordinator can use.
    if text.trim().is_empty() && more_tools.is_empty() {
        return;
    }
    // Coalesce by `message_id` when both the open and incoming turn
    // carry the same id (Claude Code's pattern at line 218-228).
    if let (Some(open_id), Some(new_id)) = (open_message_id.as_deref(), new_id) {
        if open_id == new_id {
            if let Some(last) = turns.back_mut() {
                merge_into(last, text, std::mem::take(&mut more_tools));
                if !last.text.trim().is_empty() {
                    *last_assistant_message = Some(last.text.clone());
                }
                return;
            }
        }
    }
    // Open a fresh turn. The `cap_tool_calls` cap is reapplied via
    // `merge_into`; we still cap here for the fresh-turn case.
    cap_tool_calls(&mut more_tools);
    let turn = Turn {
        role: "assistant".to_string(),
        text: truncate(text, MAX_TURN_TEXT),
        tool_calls: std::mem::take(&mut more_tools),
    };
    if !turn.text.is_empty() {
        *last_assistant_message = Some(turn.text.clone());
    }
    *open_message_id = new_id.map(str::to_string);
    push_bounded(turns, turn, keep);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(payload_type: &str, kind: &str, mut event: serde_json::Value) -> String {
        // Inject the `kind` discriminator alongside the caller's fields.
        if let serde_json::Value::Object(map) = &mut event {
            map.insert("kind".to_string(), serde_json::Value::String(kind.to_string()));
        }
        serde_json::json!({
            "schema_version": 1,
            "id": "01a0a000-0000-7000-8000-000000000000",
            "stream": {"kind": "session", "id": "sess-test"},
            "sequence": 1,
            "recorded_at": 0,
            "record_type": "event",
            "durability": "durable",
            "causation_id": null,
            "payload_type": payload_type,
            "payload_schema_version": 1,
            "payload": {"event": event},
        })
        .to_string()
    }

    #[test]

    fn user_prompt_then_assistant_text_emits_two_turns() {
        let lines = [
            line(
                "runtime.session",
                "user_prompt_display",
                serde_json::json!({"prompt": "Inspect the file."}),
            ),
            line(
                "runtime.session",
                "assistant_message_committed",
                serde_json::json!({"text": "Reading now.", "message_id": "m1"}),
            ),
        ];
        let parsed = parse_muse_turns(lines.into_iter(), 10);
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].role, "user");
        assert_eq!(parsed.turns[0].text, "Inspect the file.");
        assert_eq!(parsed.turns[1].role, "assistant");
        assert_eq!(parsed.turns[1].text, "Reading now.");
        assert_eq!(parsed.last_assistant_message.as_deref(), Some("Reading now."));
        assert!(!parsed.saw_malformed);
    }

    #[test]
    fn assistant_tool_calls_commit_parsed_to_shared_tool_call_shape() {
        let lines = [line(
            "runtime.session",
            "assistant_tool_calls_committed",
            serde_json::json!({
                "message_id": "m1",
                "tool_calls": [{
                    "name": "read_file",
                    "args": "{\"file_path\":\"src/lib.rs\"}",
                    "call_id": "c1",
                    "id": "fc1"
                }]
            }),
        )];
        let parsed = parse_muse_turns(lines.into_iter(), 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].tool_calls.len(), 1);
        assert_eq!(parsed.turns[0].tool_calls[0].name, "read_file");
        assert_eq!(
            parsed.turns[0].tool_calls[0].input,
            serde_json::json!({"file_path": "src/lib.rs"})
        );
    }

    #[test]
    fn assistant_text_and_tools_sharing_message_id_coalesce_into_one_turn() {
        let lines = [
            line(
                "runtime.session",
                "assistant_tool_calls_committed",
                serde_json::json!({
                    "message_id": "msg-shared",
                    "tool_calls": [{
                        "name": "read_file",
                        "args": "{\"file_path\":\"x\"}",
                        "call_id": "c1",
                        "id": "fc1"
                    }]
                }),
            ),
            line(
                "runtime.session",
                "assistant_message_committed",
                serde_json::json!({
                    "message_id": "msg-shared",
                    "text": "Reading the file."
                }),
            ),
        ];
        let parsed = parse_muse_turns(lines.into_iter(), 10);
        // The text commit closes the open coalescing window onto the same
        // turn the tool calls opened, so one turn carries both.
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].text, "Reading the file.");
        assert_eq!(parsed.turns[0].tool_calls.len(), 1);
        assert_eq!(parsed.last_assistant_message.as_deref(), Some("Reading the file."));
    }

    #[test]
    fn reasoning_committed_is_skipped_with_no_text_leak() {
        let lines = [
            line(
                "runtime.session",
                "user_prompt_display",
                serde_json::json!({"prompt": "Hi"}),
            ),
            // Encrypted reasoning blob — the privacy guard must surface
            // nothing from it even though the parser still walks the
            // `event` object.
            line(
                "runtime.session",
                "reasoning_committed",
                serde_json::json!({
                    "encrypted_content": "<REDACTED-BLOB>",
                    "message_id": "msg-1"
                }),
            ),
            line(
                "runtime.session",
                "assistant_message_committed",
                serde_json::json!({"text": "Hi there.", "message_id": "msg-1"}),
            ),
        ];
        let parsed = parse_muse_turns(lines.into_iter(), 10);
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].role, "user");
        assert_eq!(parsed.turns[1].role, "assistant");
        assert_eq!(parsed.turns[1].text, "Hi there.");
        // Critical: no text from reasoning_committed surfaces anywhere.
        assert!(parsed.turns.iter().all(|t| !t.text.contains("REDACTED-BLOB")));
        assert_eq!(parsed.last_assistant_message.as_deref(), Some("Hi there."));
    }

    #[test]
    fn output_and_tool_results_and_unknown_kinds_are_silently_skipped() {
        let lines = [
            line(
                "runtime.session",
                "user_prompt_display",
                serde_json::json!({"prompt": "Go."}),
            ),
            line(
                "runtime.session",
                "output",
                serde_json::json!({"chunk": "tool stdout", "task_id": "t1", "final_result": true}),
            ),
            line(
                "runtime.session",
                "tool_result_batch_committed",
                serde_json::json!({"batch_id": "b1", "results": [{"text": "tool result", "tool_call_id": "c1", "tool_call_index": 0}]}),
            ),
            line(
                "runtime.session",
                "future_event_kind",
                serde_json::json!({"future_field": true}),
            ),
            line(
                "unknown.payload_type",
                "irrelevant",
                serde_json::json!({}),
            ),
            line(
                "runtime.session",
                "assistant_message_committed",
                serde_json::json!({"text": "Done.", "message_id": "m1"}),
            ),
        ];
        let parsed = parse_muse_turns(lines.into_iter(), 10);
        // Two real turns (user + assistant), no skip surface.
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].text, "Go.");
        assert_eq!(parsed.turns[1].text, "Done.");
        assert_eq!(parsed.last_assistant_message.as_deref(), Some("Done."));
        assert!(!parsed.saw_malformed);
    }

    #[test]
    fn malformed_user_prompt_without_text_is_flagged_as_malformed() {
        let lines = [
            // `user_prompt_display` event without `prompt` is a
            // structural break — must flag the shape rather than
            // silently dropping the entire session (mirrors Claude's
            // `saw_malformed` discipline at `types.rs:95`).
            line("runtime.session", "user_prompt_display", serde_json::json!({})),
        ];
        let parsed = parse_muse_turns(lines.into_iter(), 10);
        assert!(parsed.saw_malformed);
        assert!(parsed.turns.is_empty());
    }

    #[test]
    fn empty_assistant_line_does_not_open_a_turn() {
        let lines = [line(
            "runtime.session",
            "assistant_message_committed",
            serde_json::json!({"text": "", "message_id": "m1"}),
        )];
        let parsed = parse_muse_turns(lines.into_iter(), 10);
        assert!(parsed.turns.is_empty());
        assert!(parsed.last_assistant_message.is_none());
    }

    #[test]
    fn started_with_prompt_falls_back_to_user_turn() {
        let lines = [line(
            "runtime.session",
            "started",
            serde_json::json!({"prompt": "Inline prompt", "task_id": "t1"}),
        )];
        let parsed = parse_muse_turns(lines.into_iter(), 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].role, "user");
        assert_eq!(parsed.turns[0].text, "Inline prompt");
    }

    #[test]
    fn line_has_assistant_text_predicate_matches_only_committed_text() {
        // Positive: real assistant text line.
        let positive = serde_json::json!({
            "payload_type": "runtime.session",
            "payload": {"event": {"kind": "assistant_message_committed", "text": "Done."}}
        })
        .to_string();
        assert!(MuseAdapter.line_has_assistant_text(&positive));

        // Negative: empty text — falls through to a full re-parse.
        let empty = serde_json::json!({
            "payload_type": "runtime.session",
            "payload": {"event": {"kind": "assistant_message_committed", "text": ""}}
        })
        .to_string();
        assert!(!MuseAdapter.line_has_assistant_text(&empty));

        // Negative: tool calls only — not "assistant text" for the digest.
        let tools_only = serde_json::json!({
            "payload_type": "runtime.session",
            "payload": {"event": {"kind": "assistant_tool_calls_committed"}}
        })
        .to_string();
        assert!(!MuseAdapter.line_has_assistant_text(&tools_only));

        // Negative: wrong payload_type.
        let metadata = serde_json::json!({
            "payload_type": "runtime.session.metadata",
            "payload": {"event": {"kind": "assistant_message_committed", "text": "x"}}
        })
        .to_string();
        assert!(!MuseAdapter.line_has_assistant_text(&metadata));

        // Negative: malformed JSON.
        assert!(!MuseAdapter.line_has_assistant_text("not json"));
    }

    #[test]
    fn locator_returns_path_for_known_session_and_none_for_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("session-index.db");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch("CREATE TABLE sessions (session_id TEXT, session_log_path TEXT);")
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions VALUES (?1, ?2)",
                rusqlite::params![
                    "01a0a000-0000-7000-8000-000000000001",
                    "/home/test/.local/share/muse/sessions/2026/09/12/<id>/session.jsonl"
                ],
            )
            .unwrap();
        // NULL path row (mirrors real muse data — index can leave workspace
        // columns NULL).
        connection
            .execute(
                "INSERT INTO sessions VALUES (?1, NULL)",
                rusqlite::params!["01a0a000-0000-7000-8000-000000000002"],
            )
            .unwrap();

        assert_eq!(
            muse_locator_in(&database, "01a0a000-0000-7000-8000-000000000001"),
            Some(std::path::PathBuf::from(
                "/home/test/.local/share/muse/sessions/2026/09/12/<id>/session.jsonl"
            ))
        );
        assert!(muse_locator_in(&database, "01a0a000-0000-7000-8000-000000000002").is_none());
        assert!(muse_locator_in(&database, "unknown-id").is_none());
    }

    #[test]
    fn locator_returns_none_when_index_db_is_missing() {
        // Defensive: missing index → None → NoTranscript at the reader.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.db");
        assert!(muse_locator_in(&missing, "any").is_none());
    }

    #[test]
    fn redacted_fixture_carries_no_prompts_or_secrets() {
        // Same redaction policy as `agent/provider/muse/fixtures/README.md`:
        // no prompts, no tool args with real content, no credentials, no
        // real paths. Mirror that audit's discipline so the checked-in
        // fixture stays safe to publish.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/muse_transcript.jsonl");
        let body = std::fs::read_to_string(&path).expect("fixture readable");
        assert_fixture_redacted(&path, &body);
    }

    fn assert_fixture_redacted(path: &std::path::Path, body: &str) {
        let lowered = body.to_ascii_lowercase();
        for needle in [
            "bearer ",
            "sk-",
            "api_key",
            "authorization",
            "password",
            "-----begin",
            "secret",
        ] {
            assert!(
                !lowered.contains(needle),
                "{} must not contain `{needle}`",
                path.display()
            );
        }
        for line in body.lines().filter(|line| !line.trim().is_empty()) {
            let value: serde_json::Value =
                serde_json::from_str(line).expect("fixture lines are JSON");
            walk_redacted(path, &value);
        }
    }

    fn walk_redacted(path: &std::path::Path, value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    let key_l = key.to_ascii_lowercase();
                    // Hard-forbidden keys regardless of value — these
                    // would leak dialogue content (command), tool input
                    // structure (arguments/input), or secrets
                    // (auth/token/api_key). The dialogue key `text` is
                    // permitted at the *key* level so the fixture can
                    // carry `assistant_message_committed.text` lines
                    // with a redacted value (proves the parser extracts
                    // them) — the prose check below enforces that the
                    // value is opaque.
                    assert!(
                        !matches!(
                            key_l.as_str(),
                            "command"
                            | "arguments" | "input"
                            | "auth" | "token" | "apikey" | "api_key"
                        ),
                        "{} must not carry key `{key}`",
                        path.display()
                    );
                    // `prompt`, `text`, and `encrypted_content` are
                    // permitted at the key level — their string values
                    // must be opaque redaction markers, enforced by the
                    // prose check below.
                    walk_redacted(path, child);
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    walk_redacted(path, child);
                }
            }
            serde_json::Value::String(text) => {
                // The fixture uses opaque tokens (`<REDACTED>`, opaque
                // UUIDs, file paths under `/repo` and the session log
                // layout). Any string longer than the redaction markers
                // looks suspicious — fail closed so a future fixture
                // edit that pastes a real prompt trips here.
                let looks_like_prose =
                    !text.starts_with('<') && !text.starts_with('/') && text.split_whitespace().count() > 4;
                assert!(
                    !looks_like_prose,
                    "{} string value looks like prose: {text}",
                    path.display()
                );
            }
            _ => {}
        }
    }
}
