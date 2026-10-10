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
/// MiniMax Code records task completion out of band, and its own
/// `background_task_cadence_reminder` is injected **at the start of the next
/// turn**, never when the task actually ends. An idle session therefore takes
/// no turn, receives no reminder, and never reads the finished result — the
/// stall in issue #2105. A detector driven by that reminder would never fire
/// during the stall it exists to break, so this is built on the CLI's on-disk
/// task store instead, which is written as the task runs and stops growing
/// when it ends. See [`detect_background_task_stall`].
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

/// A background task this session launched, from the `bash_background`
/// acknowledgement the CLI writes **during the turn that starts it**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchedTask {
    pub task_id: String,
    pub started_at_ms: i64,
}

/// What the CLI's own on-disk task store says about a launched task.
///
/// This is the load-bearing input. Unlike the cadence reminder, the store is
/// written when the task actually ends, so it is readable during exactly the
/// idle window the reminder cannot reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskStoreFact {
    pub task_id: String,
    /// When the CLI last wrote this task's output — for a finished task, when
    /// it finished. Measured to the second against the reminder's `endedAtMs`.
    pub settled_at_ms: i64,
    /// The CLI wrote a terminal summary, so the task is definitively over.
    pub finished: bool,
}

/// How long a task's output must have been quiet before a wake-up is spent on
/// it. The CLI flushes a running task's output periodically, so a task that
/// merely wrote recently is still running and must not consume the single
/// wake-up an attempt is allowed.
pub(crate) const TASK_QUIESCENCE_MS: i64 = 3 * 60_000;

/// Pull every background task this session launched out of the transcript.
///
/// Only the launch acknowledgement counts: it carries `details.task_id` and
/// `details.status == "started"`, and it is written while the turn that
/// started the task is still running — so it is present during the stall.
pub(crate) fn launched_background_tasks(lines: &str) -> Vec<LaunchedTask> {
    let mut tasks: Vec<LaunchedTask> = Vec::new();
    for line in lines.lines() {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(message) = record_message(&record) else {
            continue;
        };
        let Some(details) = message.get("details") else {
            continue;
        };
        if details.get("status").and_then(|s| s.as_str()) != Some("started") {
            continue;
        }
        let Some(task_id) = details.get("task_id").and_then(|t| t.as_str()) else {
            continue;
        };
        if tasks.iter().any(|task| task.task_id == task_id) {
            continue;
        }
        tasks.push(LaunchedTask {
            task_id: task_id.to_string(),
            started_at_ms: message
                .get("timestamp")
                .and_then(|t| t.as_i64())
                .unwrap_or_default(),
        });
    }
    tasks
}

/// Pure detector: has this session launched a background task that finished
/// without the session ever reading it?
///
/// A task counts as stalled when the CLI's store says it is over **and** its
/// last write is newer than the session's last assistant message. A turn that
/// began after the task ended would have seen the cadence reminder and could
/// have read the result, so that ordering is what separates "the agent is
/// working on it" from "the agent will never learn of it". Terminal silence on
/// its own is never evidence.
pub(crate) fn detect_background_task_stall(
    launched: &[LaunchedTask],
    facts: &[TaskStoreFact],
    last_assistant_at_ms: Option<i64>,
    now_ms: i64,
) -> Option<BackgroundTaskStall> {
    let mut stall: Option<BackgroundTaskStall> = None;
    for fact in facts {
        if !launched.iter().any(|task| task.task_id == fact.task_id) {
            continue;
        }
        // Still running: a recent write means the CLI may append more.
        let quiet = now_ms - fact.settled_at_ms;
        if !fact.finished && quiet < TASK_QUIESCENCE_MS {
            continue;
        }
        if let Some(last) = last_assistant_at_ms {
            if last >= fact.settled_at_ms {
                // The session has spoken since the task finished.
                continue;
            }
        }
        let newer = stall
            .as_ref()
            .is_none_or(|current| fact.settled_at_ms > current.ended_at_ms);
        if newer {
            stall = Some(BackgroundTaskStall {
                task_id: fact.task_id.clone(),
                ended_at_ms: fact.settled_at_ms,
                last_assistant_at_ms,
            });
        }
    }
    stall
}

/// Read the CLI's own completion facts for these tasks from its task store.
///
/// `<dataDir>/background-tasks/<taskId>/` is written as the task runs and stops
/// growing when it ends, so `output.log`'s modification time is the CLI's
/// record of the task's end — measured to the second against the `endedAtMs`
/// the cadence reminder carries. `summary.txt` appears only once the task has
/// finished, and is the stronger signal when present.
fn task_store_facts(tasks_dir: &Path, launched: &[LaunchedTask]) -> Vec<TaskStoreFact> {
    launched
        .iter()
        .filter_map(|task| {
            let dir = tasks_dir.join(&task.task_id);
            let output = dir.join("output.log");
            // A missing or unreadable store entry is not evidence: another
            // session may own the task, or the CLI may not have written yet.
            let settled_at_ms = mtime_ms(&output)?;
            Some(TaskStoreFact {
                task_id: task.task_id.clone(),
                settled_at_ms,
                finished: dir.join("summary.txt").is_file(),
            })
        })
        .collect()
}

/// Modification time as epoch milliseconds.
fn mtime_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let millis = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    i64::try_from(millis).ok()
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

    fn stalled_background_task(
        &self,
        lines: &str,
        spawn_path: &str,
        now_ms: i64,
    ) -> Option<String> {
        let launched = launched_background_tasks(lines);
        if launched.is_empty() {
            return None;
        }
        let tasks_dir = minimax_data_dir_for_spawn(spawn_path)?.join("background-tasks");
        let facts = task_store_facts(&tasks_dir, &launched);
        detect_background_task_stall(&launched, &facts, newest_assistant_at(lines), now_ms)
            .map(|stall| stall.task_id)
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
#[cfg(test)]
mod stall_tests {
    use super::*;

    const TASK: &str = "bg_f1045ea6-225e-46cf-bc5f-10249ffa83ca";

    /// The `bash_background` acknowledgement the CLI writes when the turn that
    /// starts a task accepts it (issue #2105, run 335, real record).
    fn launch(timestamp: i64, task_id: &str) -> String {
        serde_json::json!({
            "message_id": format!("msg-launch-{timestamp}"),
            "turn_id": "turn_1",
            "message": {
                "role": "toolResult",
                "toolCallId": "call_1",
                "toolName": "bash",
                "content": [{
                    "type": "text",
                    "text": format!("<bash_background task_id=\"{task_id}\">\nBackground Bash task accepted\n</bash_background>"),
                }],
                "details": {
                    "description": "Run clippy on all targets",
                    "timing": {},
                    "status": "started",
                    "task_id": task_id,
                },
                "timestamp": timestamp,
            },
        })
        .to_string()
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

    /// Join records into the `messages.jsonl` shape the detector reads.
    fn stream(records: impl IntoIterator<Item = String>) -> String {
        records.into_iter().collect::<Vec<_>>().join("\n")
    }

    fn launched(lines: &str) -> Vec<LaunchedTask> {
        launched_background_tasks(lines)
    }

    fn fact(task_id: &str, settled_at_ms: i64, finished: bool) -> TaskStoreFact {
        TaskStoreFact {
            task_id: task_id.to_string(),
            settled_at_ms,
            finished,
        }
    }

    const T_END: i64 = 1_791_205_267_253; // task finished
    const T_LAST_TALK: i64 = 1_791_205_156_271; // session's last assistant message
    const NOW: i64 = 1_791_216_000_000; // well after the task ended

    #[test]
    fn reads_the_task_id_from_the_launch_acknowledgement() {
        let lines = stream([assistant(T_LAST_TALK - 60_000), launch(T_LAST_TALK, TASK)]);
        assert_eq!(
            launched(&lines),
            vec![LaunchedTask {
                task_id: TASK.to_string(),
                started_at_ms: T_LAST_TALK,
            }]
        );
    }

    #[test]
    fn detects_a_finished_task_the_session_never_saw() {
        // The stall: the task ended 111s after the session last spoke, and the
        // session said nothing for 3h14m afterwards.
        let lines = stream([launch(T_LAST_TALK - 60_000, TASK), assistant(T_LAST_TALK)]);
        let stall = detect_background_task_stall(
            &launched(&lines),
            &[fact(TASK, T_END, false)],
            newest_assistant_at(&lines),
            NOW,
        )
        .expect("stall");
        assert_eq!(stall.task_id, TASK);
        assert_eq!(stall.ended_at_ms, T_END);
        assert_eq!(stall.last_assistant_at_ms, Some(T_LAST_TALK));
    }

    #[test]
    fn no_stall_when_the_session_spoke_after_the_task_finished() {
        let lines = stream([assistant(T_END + 1_000)]);
        assert_eq!(
            detect_background_task_stall(
                &launched(&lines),
                &[fact(TASK, T_END, true)],
                newest_assistant_at(&lines),
                NOW,
            ),
            None
        );
    }

    #[test]
    fn a_running_task_is_not_a_stall() {
        // Output flushed a second ago: the task is still going, and spending the
        // single wake-up now would strand the real stall later.
        let lines = stream([launch(T_LAST_TALK - 60_000, TASK), assistant(T_LAST_TALK)]);
        let now = T_END + 1_000;
        assert_eq!(
            detect_background_task_stall(
                &launched(&lines),
                &[fact(TASK, T_END, false)],
                newest_assistant_at(&lines),
                now,
            ),
            None
        );
    }

    #[test]
    fn a_finished_summary_settles_a_task_whose_output_is_still_recent() {
        // `summary.txt` is the CLI's own terminal marker, so no waiting for
        // quiescence is needed.
        let lines = stream([launch(T_LAST_TALK - 60_000, TASK), assistant(T_LAST_TALK)]);
        assert!(detect_background_task_stall(
            &launched(&lines),
            &[fact(TASK, T_END, true)],
            newest_assistant_at(&lines),
            T_END + 1_000,
        )
        .is_some());
    }

    #[test]
    fn a_task_this_session_never_launched_is_ignored() {
        let lines = stream([assistant(T_LAST_TALK)]);
        assert_eq!(
            detect_background_task_stall(
                &launched(&lines),
                &[fact("bg_someone_elses", T_END, true)],
                newest_assistant_at(&lines),
                NOW,
            ),
            None
        );
    }

    #[test]
    fn newest_finished_task_wins() {
        let lines = stream([launch(T_LAST_TALK - 60_000, TASK), assistant(T_LAST_TALK)]);
        let facts = [
            fact("bg_older", T_END, true),
            fact(TASK, T_END + 60_000, true),
        ];
        let stall = detect_background_task_stall(
            &launched(&lines),
            &facts,
            newest_assistant_at(&lines),
            NOW,
        )
        .expect("stall");
        assert_eq!(stall.task_id, TASK);
    }

    #[test]
    fn a_duplicate_launch_acknowledgement_is_one_task() {
        let lines = stream([launch(T_LAST_TALK, TASK), launch(T_LAST_TALK + 1, TASK)]);
        assert_eq!(launched(&lines).len(), 1);
    }

    #[test]
    fn a_stall_with_no_assistant_message_at_all_is_reported() {
        let lines = launch(T_LAST_TALK, TASK);
        let stall = detect_background_task_stall(
            &launched(&lines),
            &[fact(TASK, T_END, true)],
            newest_assistant_at(&lines),
            NOW,
        )
        .expect("stall");
        assert_eq!(stall.last_assistant_at_ms, None);
    }

    #[test]
    fn malformed_lines_are_skipped_without_panicking() {
        let lines = stream([
            "not json at all".to_string(),
            launch(T_LAST_TALK - 60_000, TASK),
            "{ broken".to_string(),
            assistant(T_LAST_TALK),
        ]);
        assert_eq!(launched(&lines).len(), 1);
        assert!(detect_background_task_stall(
            &launched(&lines),
            &[fact(TASK, T_END, true)],
            newest_assistant_at(&lines),
            NOW,
        )
        .is_some());
    }

    #[test]
    fn the_quiescence_window_is_longer_than_a_running_tasks_flush_cadence() {
        // Guards the constant against being tuned down to something that would
        // let a still-running task consume the one wake-up an attempt owns.
        assert!(TASK_QUIESCENCE_MS >= 60_000);
    }

    #[test]
    fn only_minimax_code_declares_the_idle_wake_up_gap() {
        // The nudge is a MiniMax Code behaviour, not a Circuit-wide rule: every
        // other reader must keep the default "no gap", or a transcript that
        // happens to look like a launch record could wake an unrelated harness.
        // mcode's own end-to-end path needs its real on-disk task store, so it
        // is covered by the pure detector tests above.
        let lines = stream([launch(T_LAST_TALK - 60_000, TASK), assistant(T_LAST_TALK)]);
        let readers = crate::services::transcript_reader::adapter::registered();
        assert_eq!(
            readers.len(),
            10,
            "the registry gained a reader; this contract must cover it"
        );
        let others: Vec<&str> = readers
            .iter()
            .map(|reader| reader.id())
            .filter(|id| *id != "mcode")
            .collect();
        assert_eq!(others.len(), 9, "every non-mcode reader is covered");
        for id in others {
            let reader =
                crate::services::transcript_reader::adapter::dispatch(id).expect("registered");
            assert_eq!(
                reader.stalled_background_task(&lines, "/repo", NOW),
                None,
                "{id} must not inherit mcode's rule"
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
