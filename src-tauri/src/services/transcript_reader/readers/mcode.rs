//! MiniMax Code transcript adapter.
//!
//! `mcode` (shipped as `@minimax-ai/code`, verified against 0.4.12)
//! persists canonical history under
//! `<dataDir>/v2/sessions/<YYYY>/<MM>/<DD>/<HH-MM-SS-mmm>-session_<base64url(sessionId)>/`
//! carrying `manifest.json` (schema v1: `sessionId`, `createdAtMs`, …) plus
//! `messages.jsonl` — one record per line shaped
//! `{message_id, turn_id, message: {role, timestamp, content}}` where `role`
//! is `user` | `assistant` | `toolResult` | `compactionSummary` and `content`
//! is an array of typed items (`{type: "text", text}`, `{type: "thinking",
//! thinking}`, `{type: "toolCall", id, name, arguments}`, `{type: "image",
//! …}`).
//!
//! Data dir resolution mirrors the CLI: `$MINIMAX_DATA_DIR` →
//! `$MAVIS_DATA_DIR` → `~/.minimax` (the `~/.minimax-code` install dir is a
//! separate choice and never holds sessions).
//!
//! The locator scans `v2/sessions/**/manifest.json` for a `sessionId` match
//! (the directory name is a timestamp + base64url id, never the raw session
//! id, so the manifest is the only reliable key) and reads the sibling
//! `messages.jsonl`. `toolResult` records are tool echoes, not turns, and
//! `compactionSummary` is bookkeeping — both are skipped the way the Grok
//! adapter drops `tool`/`system` lines.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::env;
use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::{
    cap_tool_calls, push_bounded, truncate, truncate_json_strings, Parsed, ToolCall, Turn,
    MAX_TOOL_STRING,
};

/// A background task the harness finished but never delivered to the session.
///
/// MiniMax Code records task completion out of band: the CLI's
/// `background_task_cadence_reminder` is injected **at the start of the next
/// turn**, never when the task actually ends. An idle session therefore takes
/// no turn, never receives the reminder, and never reads the finished result —
/// the stall in issue #2105. The reminder is thus evidence only once a turn
/// has already begun, which is what [`detect_background_task_stall`] turns into
/// a wake-up: the reminder names the finished task, and the assistant-message
/// ordering says whether this session has had a chance to read it yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackgroundTaskStall {
    /// Newest finished task that has not been read in this session.
    pub task_id: String,
    /// When the harness recorded the task's terminal state.
    pub ended_at_ms: i64,
    /// When the session last produced an assistant message. A turn that
    /// started after the task ended would have consumed it.
    pub last_assistant_at_ms: Option<i64>,
}

/// A task the session launched, as announced by a cadence reminder.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RemindedTask {
    task_id: String,
    ended_at_ms: i64,
}

/// Pull `endedAtMs` for every task the harness has announced as terminal out of
/// a `background_task_cadence_reminder` record's `details.tasks` array.
///
/// `undeliveredTotal` is deliberately not trusted as a completion signal: it
/// counts messages the harness has *not yet pushed*, so it stays at its last
/// value while the session is idle — precisely the state we need to detect.
fn reminded_tasks(record: &serde_json::Value) -> Vec<RemindedTask> {
    let Some(message) = record_message(record) else {
        return Vec::new();
    };
    if message
        .as_object()
        .and_then(|m| m.get("customType"))
        .and_then(|v| v.as_str())
        != Some("background_task_cadence_reminder")
    {
        return Vec::new();
    }
    let Some(details) = message
        .as_object()
        .and_then(|m| m.get("details"))
        .and_then(|d| d.get("tasks"))
        .and_then(|t| t.as_array())
    else {
        return Vec::new();
    };
    details
        .iter()
        .filter_map(|task| {
            let task_id = task.get("taskId")?.as_str()?.to_string();
            // A queued task has no `endedAtMs`: it has not finished, so it is
            // not a stall.
            let ended_at_ms = task.get("endedAtMs")?.as_i64()?;
            Some(RemindedTask {
                task_id,
                ended_at_ms,
            })
        })
        .collect()
}

/// Timestamp of the newest assistant message in the stream, if any.
fn newest_assistant_at(lines: &str) -> Option<i64> {
    lines
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|record| {
            let message = record_message(&record)?;
            if message_role(message) != Some("assistant") {
                return None;
            }
            message.get("timestamp").and_then(|t| t.as_i64())
        })
        .max()
}

/// Pure detector: does this transcript show a finished background task the
/// session has not consumed?
///
/// A task counts as stalled when the harness recorded a terminal `endedAtMs`
/// **after** the session's last assistant message. A turn that began after the
/// task ended would have received the reminder and could have read it, so the
/// ordering is what separates "the agent is working on it" from "the agent
/// will never know". Terminal silence is not evidence on its own — the
/// comparison against a real timestamp is.
pub(crate) fn detect_background_task_stall(lines: &str) -> Option<BackgroundTaskStall> {
    let last_assistant_at_ms = newest_assistant_at(lines);
    let mut stall: Option<BackgroundTaskStall> = None;
    for line in lines.lines() {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        for task in reminded_tasks(&record) {
            if let Some(last) = last_assistant_at_ms {
                if last >= task.ended_at_ms {
                    // The session has spoken since the task ended.
                    continue;
                }
            }
            let newer = stall
                .as_ref()
                .is_none_or(|current| task.ended_at_ms > current.ended_at_ms);
            if newer {
                stall = Some(BackgroundTaskStall {
                    task_id: task.task_id,
                    ended_at_ms: task.ended_at_ms,
                    last_assistant_at_ms,
                });
            }
        }
    }
    stall
}

/// Drop-in [`TranscriptAdapter`] for MiniMax Code.
pub(crate) struct McodeAdapter;

impl TranscriptAdapter for McodeAdapter {
    fn id(&self) -> &'static str {
        "mcode"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        let data_dir = minimax_data_dir_for_spawn(ctx.node_path)?;
        find_mcode_transcript_in(&data_dir.join("v2").join("sessions"), ctx.session_id)
    }

    fn parse(
        &self,
        lines: Box<dyn Iterator<Item = String> + '_>,
        keep: usize,
        max_text: usize,
    ) -> Parsed {
        parse_mcode_turns_with_text_limit(lines, keep, max_text)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        record_message(&value).is_some_and(|message| {
            message_role(message) == Some("assistant") && !message_text(message).trim().is_empty()
        })
    }

    fn stalled_background_task(&self, lines: &str) -> Option<String> {
        detect_background_task_stall(lines).map(|stall| stall.task_id)
    }
}

/// Resolve the MiniMax data dir in the environment that will execute (or
/// executed) the CLI for `spawn_path`: `$MINIMAX_DATA_DIR` → `$MAVIS_DATA_DIR`
/// → `~/.minimax`, translated through the WSL/host boundary the same way the
/// Muse adapter resolves its session store. `env` owns host-path composition.
pub(crate) fn minimax_data_dir_for_spawn(spawn_path: &str) -> Option<PathBuf> {
    let native = env::minimax_data_dir();
    env::cli_dir_for_spawn(native, ".minimax", spawn_path)
}

/// Pure MiniMax locator: walk `sessions_root` newest-first and return the
/// `messages.jsonl` beside the first `manifest.json` whose `sessionId`
/// matches. Split from the env lookup so tests drive it against a temp
/// dir. A directory carrying its own `manifest.json` IS a session
/// directory, so the walk never descends into it (`snapshots/` and
/// `reports/` are session artifacts, never sessions) and returns on the
/// first valid match instead of crawling all of history — `locate` runs on
/// every Coordinator poll tick, so an exhaustive scan would thrash disk
/// I/O. A match without a `messages.jsonl` beside it is skipped, not
/// returned (→ `NoTranscript` degrade upstream).
pub(crate) fn find_mcode_transcript_in(sessions_root: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty() || !sessions_root.is_dir() {
        return None;
    }
    // Iterative DFS so a deep tree cannot overflow the stack; depth 5
    // covers `<root>/<YYYY>/<MM>/<DD>/<session-dir>/` plus one spare
    // level for legacy layouts.
    let mut stack = vec![(sessions_root.to_path_buf(), 0usize)];
    while let Some((dir, level)) = stack.pop() {
        let manifest = dir.join("manifest.json");
        if manifest.is_file() {
            if manifest_session_id(&manifest).as_deref() == Some(session_id) {
                let messages = dir.join("messages.jsonl");
                if messages.is_file() {
                    return Some(messages);
                }
            }
            continue;
        }
        if level >= 5 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut subdirs: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                std::fs::symlink_metadata(path)
                    .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink())
            })
            .collect();
        // Ascending push + LIFO pop visits the newest dated directories
        // first, so active sessions short-circuit before history is read.
        subdirs.sort();
        for sub in subdirs {
            stack.push((sub, level + 1));
        }
    }
    None
}

/// Read a `manifest.json` and return its `sessionId` when the file parses.
/// Anything else (missing file, bad JSON, non-string id) is `None` — the
/// scan simply moves on to the next manifest.
fn manifest_session_id(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    // The manifest must be an object; `sessionId` must be a string. Both
    // are load-bearing (the CLI validates the same fields before binding
    // history), so anything else degrades to "no match" rather than a
    // best-effort guess.
    value
        .as_object()?
        .get("sessionId")?
        .as_str()
        .map(str::to_string)
}

/// The `message` object of one `messages.jsonl` record, or `None` when the
/// line is not a history record at all.
fn record_message(record: &serde_json::Value) -> Option<&serde_json::Value> {
    record.as_object()?.get("message")
}

fn message_role(message: &serde_json::Value) -> Option<&str> {
    message.as_object()?.get("role")?.as_str()
}

/// Concatenate the `text` of every `{type: "text"}` content item. `content`
/// may also arrive as a bare string (defensive: same convention as the
/// Grok/Claude parsers); `thinking`, `toolCall`, and `image` items are not
/// text and never contribute.
fn message_text(message: &serde_json::Value) -> String {
    match message.as_object().and_then(|m| m.get("content")) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Pull tool calls out of an assistant message's `{type: "toolCall"}`
/// content items. `arguments` is natively an object but may arrive as JSON
/// text; both are honoured under the shared `MAX_TOOL_STRING` truncation so
/// a call carrying a whole file body can't dominate the payload.
fn extract_mcode_tool_calls(message: &serde_json::Value) -> Vec<ToolCall> {
    let Some(serde_json::Value::Array(items)) = message.as_object().and_then(|m| m.get("content"))
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("toolCall"))
        .filter_map(|item| {
            let obj = item.as_object()?;
            let name = obj
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let input = obj
                .get("arguments")
                .map(|value| match value {
                    serde_json::Value::String(text) => {
                        serde_json::from_str(text).unwrap_or_else(|_| value.clone())
                    }
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

/// Parse MiniMax `messages.jsonl` lines into logical turns under the shared
/// [`Parsed`] contract: rolling `keep`-bounded window, whole-stream
/// last-assistant tracking, malformed flag so a renamed shape degrades loudly
/// as `ShapeChanged`. `toolResult` echoes and `compactionSummary`
/// bookkeeping are silently skipped — never flagged, never turns.
#[cfg(test)]
pub(crate) fn parse_mcode_turns(lines: impl Iterator<Item = String>, keep: usize) -> Parsed {
    parse_mcode_turns_with_text_limit(lines, keep, super::super::types::MAX_TURN_TEXT)
}

pub(crate) fn parse_mcode_turns_with_text_limit(
    lines: impl Iterator<Item = String>,
    keep: usize,
    max_text: usize,
) -> Parsed {
    let keep = keep.max(1);
    let mut turns: VecDeque<Turn> = VecDeque::new();
    let mut last_assistant_message: Option<String> = None;
    let mut saw_malformed = false;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(message) = record_message(&record) else {
            // Not a history record (ledger/snapshot envelope, partial write)
            // — skip without flagging; the malformed flag is reserved for
            // records that claim the shape but break it.
            continue;
        };
        let role = match message_role(message) {
            Some("user") => "user",
            Some("assistant") => "assistant",
            // `toolResult` echoes, `compactionSummary` bookkeeping, and
            // every unknown role — silently dropped, never flagged.
            _ => continue,
        };
        if message
            .as_object()
            .is_none_or(|m| !m.contains_key("content"))
        {
            saw_malformed = true;
            continue;
        }
        let text = message_text(message);
        let mut tool_calls = if role == "assistant" {
            extract_mcode_tool_calls(message)
        } else {
            Vec::new()
        };
        // A record with no text and no tool calls is a no-op (a
        // thinking/image-only assistant message, an image-only user
        // submission) — skip without flagging so empty placeholders never
        // consume a slot in the `keep`-bounded turn window.
        if text.trim().is_empty() && tool_calls.is_empty() {
            continue;
        }
        if role == "assistant" {
            cap_tool_calls(&mut tool_calls);
            let turn = Turn {
                role: "assistant".to_string(),
                text: truncate(&text, max_text),
                tool_calls,
            };
            if !turn.text.trim().is_empty() {
                last_assistant_message = Some(turn.text.clone());
            }
            push_bounded(&mut turns, turn, keep);
        } else {
            push_bounded(
                &mut turns,
                Turn {
                    role: "user".to_string(),
                    text: truncate(&text, max_text),
                    tool_calls: Vec::new(),
                },
                keep,
            );
        }
    }
    Parsed {
        turns: turns.into(),
        last_assistant_message,
        saw_malformed,
    }
}

#[cfg(test)]
mod stall_tests {
    use super::*;

    const TASK: &str = "bg_f1045ea6-225e-46cf-bc5f-10249ffa83ca";

    /// Join records into the `messages.jsonl` shape the detector reads.
    fn stream(records: impl IntoIterator<Item = String>) -> String {
        records.into_iter().collect::<Vec<_>>().join("\n")
    }

    fn assistant(timestamp: i64) -> String {
        serde_json::json!({
            "message_id": format!("msg-{timestamp}"),
            "turn_id": "turn_1",
            "message": {
                "role": "assistant",
                "timestamp": timestamp,
                "content": [{"type": "text", "text": "working"}],
            },
        })
        .to_string()
    }

    /// A real `background_task_cadence_reminder` record, reduced to the
    /// fields the detector reads (issue #2105, run 335).
    fn reminder(timestamp: i64, task_id: &str, ended_at_ms: i64, status: &str) -> String {
        serde_json::json!({
            "message_id": format!("msg-reminder-{timestamp}"),
            "turn_id": "turn_1",
            "message": {
                "role": "custom",
                "customType": "background_task_cadence_reminder",
                "content": "<system-reminder>task done</system-reminder>",
                "display": false,
                "details": {
                    "version": 1,
                    "tasks": [{"taskId": task_id, "status": status, "endedAtMs": ended_at_ms}],
                    "undeliveredTotal": 1,
                    "queuedTotal": 0,
                    "terminalTotal": 39,
                    "cadence": {"assistantIterationsBeforeReminder": 0},
                },
                "timestamp": timestamp,
            },
        })
        .to_string()
    }

    #[test]
    fn detects_task_that_finished_after_the_last_assistant_message() {
        let lines = stream([
            assistant(1_791_205_156_271),
            reminder(1_791_206_756_792, TASK, 1_791_205_267_253, "failed"),
        ]);

        let stall = detect_background_task_stall(&lines).expect("stall");
        assert_eq!(stall.task_id, TASK);
        assert_eq!(stall.ended_at_ms, 1_791_205_267_253);
        assert_eq!(stall.last_assistant_at_ms, Some(1_791_205_156_271));
    }

    #[test]
    fn no_stall_when_the_session_spoke_after_the_task_finished() {
        // The agent read the result and kept working: no nudge, no loop.
        let lines = stream([
            assistant(1_791_205_156_271),
            reminder(1_791_206_756_792, TASK, 1_791_205_267_253, "failed"),
            assistant(1_791_210_000_000),
        ]);

        assert_eq!(detect_background_task_stall(&lines), None);
    }

    #[test]
    fn queued_task_without_an_end_time_is_not_a_stall() {
        // A task still running carries no `endedAtMs` at all; nudging would be noise.
        let queued = serde_json::json!({
            "message_id": "msg-queued",
            "turn_id": "turn_1",
            "message": {
                "role": "custom",
                "customType": "background_task_cadence_reminder",
                "details": {
                    "version": 1,
                    "tasks": [{"taskId": TASK, "status": "running"}],
                    "undeliveredTotal": 1,
                    "queuedTotal": 1,
                },
                "timestamp": 1_791_206_756_792i64,
            },
        })
        .to_string();
        let lines = stream([assistant(1_791_205_156_271), queued]);

        assert_eq!(detect_background_task_stall(&lines), None);
    }

    #[test]
    fn newest_finished_task_wins() {
        let lines = stream([
            assistant(1_791_205_156_271),
            reminder(1_791_206_756_792, "bg_older", 1_791_205_267_253, "failed"),
            reminder(1_791_206_756_792, TASK, 1_791_206_000_000, "succeeded"),
        ]);

        let stall = detect_background_task_stall(&lines).expect("stall");
        assert_eq!(stall.task_id, TASK);
        assert_eq!(stall.ended_at_ms, 1_791_206_000_000);
    }

    #[test]
    fn a_reminder_for_a_still_running_session_is_not_a_stall() {
        // An equal timestamp means the turn that received the reminder already
        // produced its message, so there is nothing to wake.
        let lines = stream([
            assistant(1_791_205_267_253),
            reminder(1_791_206_756_792, TASK, 1_791_205_267_253, "failed"),
        ]);
        assert_eq!(detect_background_task_stall(&lines), None);
    }

    #[test]
    fn other_custom_reminders_are_ignored() {
        let todo = serde_json::json!({
            "message_id": "msg-todo",
            "turn_id": "turn_1",
            "message": {
                "role": "custom",
                "customType": "todo_cadence_reminder",
                "details": {"tasks": [{"taskId": TASK, "endedAtMs": 1_791_205_267_253i64}]},
                "timestamp": 1_791_206_756_792i64,
            },
        })
        .to_string();
        let lines = stream([assistant(1_791_205_156_271), todo]);
        assert_eq!(detect_background_task_stall(&lines), None);
    }

    #[test]
    fn malformed_lines_are_skipped_without_panicking() {
        let lines = stream([
            "not json at all".to_string(),
            assistant(1_791_205_156_271),
            "{ broken".to_string(),
            reminder(1_791_206_756_792, TASK, 1_791_205_267_253, "failed"),
        ]);
        assert!(detect_background_task_stall(&lines).is_some());
    }

    #[test]
    fn a_stall_with_no_assistant_message_at_all_is_reported() {
        let lines = reminder(1_791_206_756_792, TASK, 1_791_205_267_253, "failed");
        let stall = detect_background_task_stall(&lines).expect("stall");
        assert_eq!(stall.last_assistant_at_ms, None);
    }

    #[test]
    fn only_minimax_code_declares_the_idle_wake_up_gap() {
        // The nudge is a MiniMax Code behaviour, not a Circuit-wide rule. Every
        // other harness must keep the default "no gap", or an unrelated
        // transcript could be read as a stall and woken.
        let mcode_lines = stream([
            assistant(1_791_205_156_271),
            reminder(1_791_206_756_792, TASK, 1_791_205_267_253, "failed"),
        ]);
        assert_eq!(
            McodeAdapter
                .stalled_background_task(&mcode_lines)
                .as_deref(),
            Some(TASK)
        );
        let readers = crate::services::transcript_reader::adapter::registered();
        assert_eq!(
            readers.len(),
            10,
            "the registry gained a reader; this contract must cover it"
        );
        for adapter in readers {
            let expected = (adapter.id() == "mcode").then_some(TASK);
            assert_eq!(
                adapter.stalled_background_task(&mcode_lines).as_deref(),
                expected,
                "{} must not inherit another harness's transcript rule",
                adapter.id()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(message_id: &str, turn_id: &str, message: serde_json::Value) -> String {
        serde_json::json!({
            "message_id": message_id,
            "turn_id": turn_id,
            "message": message,
        })
        .to_string()
    }

    fn user_msg(text: &str) -> serde_json::Value {
        serde_json::json!({
            "role": "user",
            "timestamp": 1_788_000_000_000u64,
            "content": [{"type": "text", "text": text}],
        })
    }

    fn assistant_msg(text: &str, calls: serde_json::Value) -> serde_json::Value {
        let mut content = vec![serde_json::json!({"type": "text", "text": text})];
        if let serde_json::Value::Array(mut extra) = calls {
            content.append(&mut extra);
        }
        serde_json::json!({
            "role": "assistant",
            "timestamp": 1_788_000_001_000u64,
            "content": content,
        })
    }

    #[test]
    fn parses_user_and_assistant_records_with_typed_content() {
        let lines = [
            record("msg-1", "turn-1", user_msg("Fix the login bug")),
            record(
                "msg-2",
                "turn-1",
                assistant_msg(
                    "Reading the file.",
                    serde_json::json!([{"type": "toolCall", "id": "c1",
                        "name": "read_file", "arguments": {"path": "src/auth.rs"}}]),
                ),
            ),
            record(
                "msg-3",
                "turn-1",
                serde_json::json!({"role": "toolResult", "timestamp": 1_788_000_002_000u64,
                    "content": [{"type": "text", "text": "file contents"}]}),
            ),
        ];
        let parsed = parse_mcode_turns(lines.into_iter(), 10);
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].role, "user");
        assert_eq!(parsed.turns[0].text, "Fix the login bug");
        assert!(parsed.turns[0].tool_calls.is_empty());
        assert_eq!(parsed.turns[1].role, "assistant");
        assert_eq!(parsed.turns[1].text, "Reading the file.");
        assert_eq!(parsed.turns[1].tool_calls.len(), 1);
        assert_eq!(parsed.turns[1].tool_calls[0].name, "read_file");
        assert_eq!(
            parsed.turns[1].tool_calls[0].input,
            serde_json::json!({"path": "src/auth.rs"})
        );
        assert_eq!(
            parsed.last_assistant_message.as_deref(),
            Some("Reading the file.")
        );
        assert!(!parsed.saw_malformed);
    }

    #[test]
    fn string_arguments_decode_and_thinking_only_messages_drop() {
        let lines = [
            record(
                "msg-1",
                "turn-1",
                assistant_msg(
                    "",
                    serde_json::json!([
                        {"type": "thinking", "thinking": "private plan"},
                        {"type": "toolCall", "id": "c1", "name": "run",
                         "arguments": "{\"cmd\":\"cargo test\"}"},
                    ]),
                ),
            ),
            record(
                "msg-2",
                "turn-2",
                serde_json::json!({"role": "compactionSummary",
                    "summary": "older work compacted"}),
            ),
        ];
        let parsed = parse_mcode_turns(lines.into_iter(), 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].text, "");
        assert_eq!(
            parsed.turns[0].tool_calls[0].input,
            serde_json::json!({"cmd": "cargo test"})
        );
        // No assistant text anywhere: no last message, but no malformed flag
        // either — thinking-only and compaction records are known shapes.
        assert_eq!(parsed.last_assistant_message, None);
        assert!(!parsed.saw_malformed);
    }

    #[test]
    fn missing_content_flags_malformed_while_unknown_lines_skip_quietly() {
        let lines = [
            "not json at all".to_string(),
            record(
                "msg-1",
                "turn-1",
                serde_json::json!({"role": "assistant", "timestamp": 1u64}),
            ),
        ];
        let parsed = parse_mcode_turns(lines.into_iter(), 10);
        assert!(parsed.turns.is_empty());
        assert!(parsed.saw_malformed);
    }

    #[test]
    fn locator_finds_messages_via_manifest_scan_not_dir_name() {
        let root = tempfile::tempdir().unwrap();
        // Date-shaped nesting with an opaque (non-id) directory name, exactly
        // like the CLI's `<YYYY>/<MM>/<DD>/<time>-session_<base64url>` layout.
        let session_dir = root.path().join("2026/09/19/10-00-00-000-session_abc");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("manifest.json"),
            serde_json::json!({"schemaVersion": 1, "sessionId": "ses-123",
                "createdAtMs": 1_788_000_000_000u64})
            .to_string(),
        )
        .unwrap();
        let messages = session_dir.join("messages.jsonl");
        std::fs::write(&messages, record("msg-1", "turn-1", user_msg("hi"))).unwrap();
        // A decoy manifest for another session must not shadow the match.
        let decoy = root.path().join("2026/09/18/09-00-00-000-session_zzz");
        std::fs::create_dir_all(&decoy).unwrap();
        std::fs::write(
            decoy.join("manifest.json"),
            serde_json::json!({"schemaVersion": 1, "sessionId": "ses-999",
                "createdAtMs": 1_787_000_000_000u64})
            .to_string(),
        )
        .unwrap();
        std::fs::write(decoy.join("messages.jsonl"), "noise").unwrap();

        assert_eq!(
            find_mcode_transcript_in(root.path(), "ses-123"),
            Some(messages)
        );
        assert_eq!(find_mcode_transcript_in(root.path(), "ses-absent"), None);
        assert_eq!(find_mcode_transcript_in(root.path(), ""), None);
    }

    #[test]
    fn empty_user_records_skip_without_consuming_window() {
        // An image-only user submission carries no text and no tool calls:
        // it must not evict a meaningful turn from a small window.
        let lines = [
            record("msg-1", "turn-1", user_msg("first")),
            record(
                "msg-2",
                "turn-2",
                serde_json::json!({"role": "user", "timestamp": 1u64,
                    "content": [{"type": "image", "data": "…"}]}),
            ),
            record(
                "msg-3",
                "turn-2",
                assistant_msg("answer", serde_json::json!([])),
            ),
        ];
        let parsed = parse_mcode_turns(lines.into_iter(), 2);
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].text, "first");
        assert_eq!(parsed.turns[1].text, "answer");
        assert!(!parsed.saw_malformed);
    }

    #[test]
    fn locator_never_descends_into_session_artifact_dirs() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("2026/09/19/10-00-00-000-session_abc");
        std::fs::create_dir_all(session_dir.join("snapshots")).unwrap();
        std::fs::write(
            session_dir.join("manifest.json"),
            serde_json::json!({"schemaVersion": 1, "sessionId": "ses-123",
                "createdAtMs": 1_788_000_000_000u64})
            .to_string(),
        )
        .unwrap();
        // A manifest buried in snapshots/ is a session artifact, never a
        // session — the walk must not reach it even though it matches.
        std::fs::write(
            session_dir.join("snapshots/manifest.json"),
            serde_json::json!({"schemaVersion": 1, "sessionId": "ses-buried",
                "createdAtMs": 1_788_000_000_000u64})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            session_dir.join("snapshots/messages.jsonl"),
            record("msg-9", "turn-9", user_msg("buried")),
        )
        .unwrap();
        assert_eq!(find_mcode_transcript_in(root.path(), "ses-buried"), None);
    }

    #[test]
    fn locator_prefers_newest_duplicate_manifest() {
        let root = tempfile::tempdir().unwrap();
        for (day, tag) in [("18", "old"), ("19", "new")] {
            let dir = root
                .path()
                .join(format!("2026/09/{day}/10-00-00-000-session_abc"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("manifest.json"),
                serde_json::json!({"schemaVersion": 1, "sessionId": "ses-dup",
                    "createdAtMs": 1_788_000_000_000u64})
                .to_string(),
            )
            .unwrap();
            std::fs::write(
                dir.join("messages.jsonl"),
                record("msg-1", "turn-1", user_msg(tag)),
            )
            .unwrap();
        }
        let found = find_mcode_transcript_in(root.path(), "ses-dup").unwrap();
        let text = std::fs::read_to_string(found).unwrap();
        assert!(
            text.contains("\"new\""),
            "expected newest duplicate, got {text}"
        );
    }

    #[test]
    fn locator_ignores_manifest_without_messages() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("2026/09/19/10-00-00-000-session_abc");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("manifest.json"),
            serde_json::json!({"schemaVersion": 1, "sessionId": "ses-123",
                "createdAtMs": 1_788_000_000_000u64})
            .to_string(),
        )
        .unwrap();
        assert_eq!(find_mcode_transcript_in(root.path(), "ses-123"), None);
    }

    #[test]
    fn line_predicate_matches_assistant_text_only() {
        let text = record(
            "msg-1",
            "turn-1",
            assistant_msg("Blocking question?", serde_json::json!([])),
        );
        assert!(McodeAdapter.line_has_assistant_text(&text));
        let tools_only = record(
            "msg-2",
            "turn-1",
            assistant_msg(
                "",
                serde_json::json!([{"type": "toolCall", "id": "c1",
                "name": "run", "arguments": {}}]),
            ),
        );
        assert!(!McodeAdapter.line_has_assistant_text(&tools_only));
        let user = record("msg-3", "turn-2", user_msg("go on"));
        assert!(!McodeAdapter.line_has_assistant_text(&user));
        assert!(!McodeAdapter.line_has_assistant_text("garbage"));
    }
}

#[cfg(test)]
mod contract_tests {

    use crate::services::transcript_reader::{assistant_report_from_file, TranscriptFormat};

    #[test]
    fn mcode_native_transcript_recovers_circuit_report() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp.path().join("2026/09/19/10-00-00-000-session_abc");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(
            session.join("manifest.json"),
            r#"{"schemaVersion":1,"sessionId":"ses-123","createdAtMs":1788000000000}"#,
        )
        .unwrap();
        let file = session.join("messages.jsonl");
        std::fs::write(&file, concat!(
            "{\"message_id\":\"m1\",\"turn_id\":\"t1\",\"message\":{\"role\":\"user\",\"timestamp\":1788000000000,\"content\":[{\"type\":\"text\",\"text\":\"Fix the parser\"}]}}\n",
            "{\"message_id\":\"m2\",\"turn_id\":\"t1\",\"message\":{\"role\":\"assistant\",\"timestamp\":1788000001000,\"content\":[{\"type\":\"text\",\"text\":\"Parser fixed. Tests pass.\"}]}}\n",
        )).unwrap();

        let path = crate::services::transcript_reader::readers::mcode::find_mcode_transcript_in(
            temp.path(),
            "ses-123",
        )
        .expect("manifest scan must resolve the session transcript");
        assert_eq!(path, file);
        let report = assistant_report_from_file(&path, TranscriptFormat::Mcode)
            .expect("a completed mcode turn must be readable by the circuit");
        assert_eq!(report.text, "Parser fixed. Tests pass.");

        use std::io::Write;
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap();
        writeln!(writer, "{{\"message_id\":\"m3\",\"turn_id\":\"t1\",\"message\":{{\"role\":\"toolResult\",\"timestamp\":1788000002000,\"content\":[{{\"type\":\"text\",\"text\":\"tool echo\"}}]}}}}").unwrap();
        assert_eq!(
            assistant_report_from_file(&path, TranscriptFormat::Mcode)
                .unwrap()
                .revision,
            report.revision,
            "a toolResult echo must not advance the assistant revision"
        );
        writeln!(writer, "{{\"message_id\":\"m4\",\"turn_id\":\"t2\",\"message\":{{\"role\":\"assistant\",\"timestamp\":1788000003000,\"content\":[{{\"type\":\"text\",\"text\":\"Next task done.\"}}]}}}}").unwrap();
        assert_ne!(
            assistant_report_from_file(&path, TranscriptFormat::Mcode)
                .unwrap()
                .revision,
            report.revision,
            "a fresh assistant response must advance the revision"
        );
    }
}

#[cfg(test)]
mod file_contract_tests {
    use super::*;
    use crate::services::transcript_reader::test_support::{assert_jsonl_contract, fixture};

    #[test]
    fn reader_handles_tail_digest_malformed_empty_shape_changed_and_unreadable_files() {
        assert_jsonl_contract(
            &McodeAdapter,
            &fixture("mcode", "transcript.jsonl"),
            &fixture("mcode", "shape_changed.jsonl"),
            "MiniMax blocking question?",
        );
    }
}
