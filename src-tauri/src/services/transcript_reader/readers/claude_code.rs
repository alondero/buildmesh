//! Claude Code transcript adapter — the default transcript format for any
//! harness id that doesn't have its own registered adapter (mirrors the
//! legacy `TranscriptFormat::for_harness` default arm). Cursor reuses the
//! same message shape but a workspace-scoped path, so Cursor's adapter
//! shares Claude Code's parser.
//!
//! Issue #1661 step 8: Claude Code is the **last harness migrated
//! end-to-end**. All Claude-Code-specific logic — the `tool_use`
//! content-block extractor, the rolling-buffer `parse_turns` (with
//! `message.id` coalescing), the `~/.claude/projects/<encoded>/<id>.jsonl`
//! path builder, and the background-task-count hook used by the
//! attention route — lives in this file. The shared Claude-Code content
//! primitives (`encode_path`, `is_synthetic_message`, `concat_text_blocks`,
//! `first_text_block`) move to the sibling `transcript_paths` module
//! (shared with `agent_node_discovery`); `truncate` stays in
//! `transcript_reader::types` per issue #340.

use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::env;
use crate::services::transcript_paths::{concat_text_blocks, encode_path, is_synthetic_message};
use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::{
    cap_tool_calls, merge_into_with_text_limit, push_bounded, truncate, truncate_json_strings,
    Parsed, ToolCall, Turn, MAX_TOOL_STRING,
};

/// Drop-in [`TranscriptAdapter`] for Claude Code (also the default for any
/// unknown harness id).
pub(crate) struct ClaudeCodeAdapter;

impl TranscriptAdapter for ClaudeCodeAdapter {
    /// Esc fires no hook; Claude Code records the interruption as the last
    /// user record (`[Request interrupted by user]`, or `… for tool use]`).
    fn interrupted_turn(&self, lines: &str) -> bool {
        let last_user_text = lines
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| {
                matches!(
                    value.get("type").and_then(|kind| kind.as_str()),
                    Some("user" | "assistant")
                ) && value.get("isMeta").and_then(|meta| meta.as_bool()) != Some(true)
            })
            .last()
            .filter(|value| value.get("type").and_then(|kind| kind.as_str()) == Some("user"))
            .and_then(|value| {
                let content = value.pointer("/message/content")?;
                content
                    .as_str()
                    .or_else(|| content.pointer("/0/text").and_then(|text| text.as_str()))
                    .map(str::to_owned)
            });
        last_user_text.is_some_and(|text| text.starts_with("[Request interrupted by user"))
    }

    fn id(&self) -> &'static str {
        "claude_code"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        transcript_path(ctx.session_id, ctx.node_path)
    }

    fn parse(
        &self,
        lines: Box<dyn Iterator<Item = String> + '_>,
        keep: usize,
        max_text: usize,
    ) -> Parsed {
        parse_turns_with_text_limit(lines, keep, max_text)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        // Same predicate the reader's `line_has_assistant_text` matches
        // on the Claude Code / Cursor arms (issue #341). Cursor reuses
        // this exact shape, which is why Cursor's adapter delegates here.
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        value.get("type").and_then(|kind| kind.as_str()) == Some("assistant")
            && value
                .get("message")
                .and_then(|message| message.get("role"))
                .and_then(|role| role.as_str())
                == Some("assistant")
            && !concat_text_blocks(
                value
                    .get("message")
                    .and_then(|message| message.get("content")),
            )
            .trim()
            .is_empty()
    }
}

/// Build the expected on-disk path of a Claude Code session transcript:
/// `<claude_dir>/projects/<encoded node_path>/<session_id>.jsonl`.
pub(crate) fn transcript_path(session_id: &str, node_path: &str) -> Option<PathBuf> {
    Some(
        env::cli_dir_for_spawn(env::claude_dir(), ".claude", node_path)?
            .join("projects")
            .join(encode_path(node_path))
            .join(format!("{session_id}.jsonl")),
    )
}

/// Pull `tool_use` blocks out of a message `content` array into
/// [`ToolCall`]s, truncating string leaves in each raw `input`. Exposed
/// `pub(crate)` so `CommandCodeAdapter` can reuse Claude Code's tool-call
/// shape (its wire shape is identical for `tool_use` blocks).
pub(crate) fn extract_tool_calls(content: Option<&serde_json::Value>) -> Vec<ToolCall> {
    let Some(serde_json::Value::Array(blocks)) = content else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .map(|b| {
            let name = b
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let input = b.get("input").cloned().unwrap_or(serde_json::Value::Null);
            ToolCall {
                name,
                input: truncate_json_strings(input, MAX_TOOL_STRING),
            }
        })
        .collect()
}

/// Parse JSONL lines into logical turns, retaining only the last `keep`
/// of them in a rolling buffer (issue #335: bounds held memory
/// regardless of transcript size). Skips every non-message line type
/// (`mode`, `queue-operation`, `file-history-snapshot`, `system`,
/// summaries, …), synthetic injections, and pure tool-result echoes.
/// Consecutive assistant lines sharing a `message.id` are coalesced into
/// one turn (Claude Code splits one assistant message — thinking / text
/// / tool_use — across several lines).
#[cfg(test)]
pub(crate) fn parse_turns(lines: impl Iterator<Item = String>, keep: usize) -> Parsed {
    parse_turns_with_text_limit(lines, keep, super::super::types::MAX_TURN_TEXT)
}

pub(crate) fn parse_turns_with_text_limit(
    lines: impl Iterator<Item = String>,
    keep: usize,
    max_text: usize,
) -> Parsed {
    // Always retain at least the open turn so a split assistant message
    // can still coalesce its continuation lines (the open turn is never
    // evicted — eviction only drops the front).
    let keep = keep.max(1);
    let mut turns: VecDeque<Turn> = VecDeque::new();
    let mut last_assistant_message: Option<String> = None;
    let mut saw_malformed = false;
    let mut open_assistant_id: Option<String> = None;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let outer = value.get("type").and_then(|t| t.as_str());
        // Skip every envelope that's not a user / assistant message line.
        // The recognized shape is `{"type":"user"|"assistant", "message":{...}}`.
        if outer != Some("user") && outer != Some("assistant") {
            continue;
        }
        let Some(message) = value.get("message") else {
            saw_malformed = true;
            continue;
        };
        let Some(role) = message.get("role").and_then(|r| r.as_str()) else {
            saw_malformed = true;
            continue;
        };
        if role != "user" && role != "assistant" {
            saw_malformed = true;
            continue;
        }
        let raw_content = message.get("content");
        let text = concat_text_blocks(raw_content);
        if role == "user" {
            // A user turn always opens a new turn slot — close the open
            // assistant coalescing window.
            open_assistant_id = None;
            if is_synthetic_message(&text) || text.trim().is_empty() {
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
        // An assistant line with neither text nor tool calls (e.g. a
        // lone `thinking` block) carries nothing the Coordinator can
        // use. Drop before the coalescing window to match the legacy
        // `parse_turns` behaviour: an empty continuation must not
        // merge into the open turn and silently widen its window.
        if text.trim().is_empty() && extract_tool_calls(raw_content).is_empty() {
            continue;
        }
        // Assistant turn: a continuation line (sharing the open
        // message.id) merges into the existing turn; a fresh id opens
        // a new one.
        let id = value
            .get("id")
            .or_else(|| message.get("id"))
            .and_then(|id| id.as_str())
            .map(str::to_string);
        if let (Some(id), Some(open)) = (&id, &open_assistant_id) {
            if id == open {
                if let Some(last) = turns.back_mut() {
                    merge_into_with_text_limit(
                        last,
                        &text,
                        extract_tool_calls(raw_content),
                        max_text,
                    );
                    if !last.text.is_empty() {
                        last_assistant_message = Some(last.text.clone());
                    }
                    continue;
                }
            }
        }
        open_assistant_id = id;
        let mut tool_calls = extract_tool_calls(raw_content);
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

// --- Pending background tasks (issue #878) ---
//
// Claude Code ends its turn when it launches background work (a
// `run_in_background` Bash call, or a foreground command that outlives
// its timeout and is moved to the background) and auto-resumes itself
// when the task's `<task-notification>` arrives. A Stop hook that fires
// with such work still pending is NOT "the user is needed". The
// transcript records both ends deterministically:
//
//   launch  — a `Bash` `tool_use` and its own `tool_result`, whose text
//             opens with the receipt "…background… (ID: xyz) … You will
//             be notified when it completes."
//   finish  — a line (queue-operation, or the queued_command attachment
//             that re-invokes the agent) carrying
//             `<task-id>xyz</task-id>`.
//
// Pending = launched minus terminal notifications and successful stops.

/// The harness-authored launch receipt, anchored to the start of a
/// `tool_result` and carrying the task id it owns: the explicit
/// `run_in_background` form, or the timeout form for a foreground
/// command that was moved to the background. Both observed phrasings
/// begin the result text, because the harness composes the message; a
/// tool returning those sentences as content — `cat`, `Read`, `Grep`,
/// or a JSON payload quoting them — carries them mid-text, where they
/// are quoted material rather than proof of a launch (issue #2264).
/// The trailing prose (output path, completion promise, cwd note) sits
/// outside the match, so rewording it does not by itself cost a real
/// launch.
static LAUNCH_RECEIPT: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(concat!(
        r"^\s*(?:Command running in background with ID: ",
        r"|Command did not complete within (?:its )?\d+s timeout ",
        r"and was moved to the background \(ID: )",
        r"(?P<id>[A-Za-z0-9_-]+)",
    ))
    .unwrap()
});
/// A task-notification's id paired with its status, non-greedy so
/// several notifications on one line pair correctly. Real transcripts
/// carry `<status>running</status>` notifications too (e.g. a foreground
/// command moved to the background) — only a terminal status means the
/// wait is over.
static NOTIFIED_ID: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(r"<task-id>([A-Za-z0-9_-]+)</task-id>.*?<status>([a-z_]+)</status>").unwrap()
});
/// Tools whose result can carry a launch receipt. Claude Code launches
/// background work from `Bash` alone — either `run_in_background: true`
/// or a foreground command that outlived its timeout — so a receipt is
/// only evidence of a launch when the `tool_result` resolves to a
/// `tool_use` naming one of these. Correlation matters as much as the
/// wording: a tool result carries text too, so a receipt echoed back as
/// content would otherwise register a task no notification can settle.
const LAUNCH_TOOLS: [&str; 1] = ["Bash"];

/// Count Claude Code background tasks without a terminal notification or successful
/// TaskStop result. `None` = the file could not be read — the caller must
/// treat that as "unknown" and fall back to its pre-#878 behaviour,
/// never as "no pending work".
pub fn count_pending_background_tasks(path: &Path) -> Option<usize> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    Some(pending_background_task_ids(reader.lines().map_while(Result::ok)).len())
}

// --- Native session continuation (issue #2263) ---
//
// A running Claude Code process can carry on inside a *new* native session and
// leave the transition in the transcript of the session it came from:
//
//   {"type":"continued-in","sessionId":"<old>",
//    "continuedInSessionId":"<new>"}
//
// Every callback after that point carries `<new>`, so this record is the only
// evidence that distinguishes "the same process continued" from "a callback
// from a process that has been replaced" — an unrelated id is never lineage.

/// The session this transcript records itself continuing into. `None` covers
/// both "no continuation" and "unreadable": neither is proof, and the caller
/// must not tell them apart.
pub fn continued_session_id(session_id: &str, path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    declared_continuation(session_id, reader.lines().map_while(Result::ok))
}

// --- `/clear` successor sessions ---
//
// `/clear` moves the running process into a new session without writing a
// `continued-in` record. The new transcript instead opens with the command
// itself: its first non-meta user record is `<command-name>/clear</command-name>`.

/// When this transcript's session was started by `/clear`: the timestamp of
/// its opening `/clear` record. `None` when it opens any other way or is
/// unreadable.
pub fn cleared_at(path: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
    let file = fs::File::open(path).ok()?;
    opening_clear(BufReader::new(file).lines().map_while(Result::ok))
}

/// Pure scan for [`cleared_at`].
pub(crate) fn opening_clear(
    lines: impl Iterator<Item = String>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    for line in lines {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(|kind| kind.as_str()) != Some("user")
            || value.get("isMeta").and_then(|meta| meta.as_bool()) == Some(true)
        {
            continue;
        }
        let content = value.pointer("/message/content")?;
        let text = content
            .as_str()
            .or_else(|| content.pointer("/0/text").and_then(|text| text.as_str()))?;
        if !text
            .trim_start()
            .starts_with("<command-name>/clear</command-name>")
        {
            return None;
        }
        return record_timestamp(&value);
    }
    None
}

/// The newest record timestamp in this transcript. `None` when it has none
/// or is unreadable.
pub fn last_record_at(path: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
    let file = fs::File::open(path).ok()?;
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(&line).ok())
        .filter_map(|value| record_timestamp(&value))
        .max()
}

fn record_timestamp(value: &serde_json::Value) -> Option<chrono::DateTime<chrono::Utc>> {
    let stamp = value.get("timestamp")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|stamp| stamp.with_timezone(&chrono::Utc))
}

/// Pure scan: the last `continued-in` record whose own `sessionId` is this
/// transcript. A record naming a different predecessor proves nothing about
/// *this* session, and the newest record wins if a transcript carries more
/// than one.
pub(crate) fn declared_continuation(
    session_id: &str,
    lines: impl Iterator<Item = String>,
) -> Option<String> {
    let mut continued = None;
    for line in lines {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(|kind| kind.as_str()) != Some("continued-in") {
            continue;
        }
        let declares_own_lineage = value
            .get("sessionId")
            .and_then(|id| id.as_str())
            .is_some_and(|declared| declared == session_id);
        let Some(successor) = value
            .get("continuedInSessionId")
            .and_then(|id| id.as_str())
            .filter(|successor| !successor.is_empty() && *successor != session_id)
        else {
            continue;
        };
        if declares_own_lineage {
            continued = Some(successor.to_string());
        }
    }
    continued
}

/// Pure scan over JSONL lines: launched-task ids with no matching
/// terminal notification or successful TaskStop result, in launch order. Split from
/// the I/O wrapper so tests drive it with inline fixtures.
pub(crate) fn pending_background_task_ids(lines: impl Iterator<Item = String>) -> Vec<String> {
    let mut launched: Vec<String> = Vec::new();
    let mut terminated: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stop_calls = std::collections::HashMap::new();
    // `tool_use` ids of launch-capable calls awaiting their result. Each
    // id is consumed by the `tool_result` it answers, so the set stays
    // bounded by in-flight calls rather than by transcript length.
    let mut launch_calls = std::collections::HashSet::new();
    for line in lines {
        // The notification marker is matched on the raw line: it
        // appears in `queue-operation` lines and in the queued_command
        // attachment that re-invokes the agent, and caring which one
        // carries it would couple us to more of the shape than we
        // need.
        for cap in NOTIFIED_ID.captures_iter(&line) {
            if &cap[2] != "running" {
                terminated.insert(cap[1].to_string());
            }
        }
        // Launches only count inside a tool_result block — free text
        // merely *mentioning* the receipt (e.g. an agent quoting these
        // docs) must not register a phantom task.
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(serde_json::Value::Array(blocks)) =
            val.get("message").and_then(|m| m.get("content"))
        else {
            continue;
        };
        for block in blocks {
            let outer = val.get("type").and_then(|t| t.as_str());
            let kind = block.get("type").and_then(|t| t.as_str());
            if outer == Some("assistant") && kind == Some("tool_use") {
                let name = block.get("name").and_then(|t| t.as_str()).unwrap_or("");
                if let Some(call_id) = block.get("id").and_then(|v| v.as_str()) {
                    if LAUNCH_TOOLS.contains(&name) {
                        launch_calls.insert(call_id.to_owned());
                    }
                    if name == "TaskStop" {
                        if let Some(task_id) = block
                            .get("input")
                            .and_then(|v| v.get("task_id"))
                            .and_then(|v| v.as_str())
                        {
                            stop_calls.insert(call_id.to_owned(), task_id.to_owned());
                        }
                    }
                }
            }
            if kind != Some("tool_result") {
                continue;
            }
            let text = match block.get("content") {
                Some(serde_json::Value::String(s)) => s.clone(),
                other => concat_text_blocks(other),
            };
            // TaskStop can finish without a queued task notification. Require its actual
            // successful result, correlated to the tool call, before ending work.
            if outer == Some("user")
                && block.get("is_error").and_then(|v| v.as_bool()) != Some(true)
            {
                if let Some(task_id) = block
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .and_then(|id| stop_calls.remove(id))
                {
                    if let Ok(result) = serde_json::from_str::<serde_json::Value>(&text) {
                        let prefix = format!("Successfully stopped task: {task_id} (");
                        if result.get("task_id").and_then(|v| v.as_str()) == Some(task_id.as_str())
                            && result
                                .get("message")
                                .and_then(|v| v.as_str())
                                .is_some_and(|message| message.starts_with(&prefix))
                        {
                            terminated.insert(task_id);
                        }
                    }
                }
            }
            // Both halves of the exchange must agree: the result answers
            // a launch-capable call, and its text opens with the receipt.
            // Wording alone is not evidence — a tool result carries text
            // too — and an uncorrelated receipt owns no task, so admitting
            // one would register a wait no notification can settle
            // (issue #2264).
            let owned = block
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| launch_calls.remove(id));
            if !owned {
                continue;
            }
            let Some(cap) = LAUNCH_RECEIPT.captures(&text) else {
                continue;
            };
            let Some(id) = cap.name("id") else {
                continue;
            };
            launched.push(id.as_str().to_string());
        }
    }
    launched.retain(|id| !terminated.contains(id));
    launched
}

#[allow(unused_imports)]
use crate::services::transcript_reader::types::TranscriptTail as _UnusedTranscriptTail; // keep type accessible from tests

#[cfg(test)]
mod interrupt_tests {
    use super::*;

    /// Esc during a turn fires no Claude Code hook (no `Stop`); the
    /// transcript records the interruption as the turn's last user record
    /// (captured live on 2.1.299). Without reading it the node read Running
    /// forever.
    #[test]
    fn an_interrupt_record_ends_the_turn() {
        let prompt = r#"{"type":"user","timestamp":"2026-10-14T12:30:28.772Z","message":{"role":"user","content":"Write a 2000 word essay"}}"#;
        let partial = r##"{"type":"assistant","timestamp":"2026-10-14T12:30:35.034Z","message":{"role":"assistant","content":[{"type":"text","text":"# A Crossing"}]}}"##;
        let interrupted = r#"{"type":"user","timestamp":"2026-10-14T12:30:35.043Z","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#;
        let tool_interrupt = r#"{"type":"user","timestamp":"2026-10-14T12:30:36.000Z","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#;
        let bookkeeping = r#"{"type":"system","timestamp":"2026-10-14T12:31:46.731Z"}"#;
        let adapter = ClaudeCodeAdapter;
        assert!(adapter.interrupted_turn(&format!(
            "{prompt}\n{partial}\n{interrupted}\n{bookkeeping}\n"
        )));
        assert!(adapter.interrupted_turn(&format!("{prompt}\n{tool_interrupt}\n")));
        // Still working, or a new prompt after the interruption.
        assert!(!adapter.interrupted_turn(&format!("{prompt}\n{partial}\n")));
        assert!(!adapter.interrupted_turn(&format!("{interrupted}\n{prompt}\n")));
        assert!(!adapter.interrupted_turn(""));
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;
    use crate::services::transcript_paths::{
        concat_text_blocks, encode_path, first_text_block, is_synthetic_message,
    };
    use crate::services::transcript_reader::test_support::fixture;
    use crate::services::transcript_reader::types::empty_or_shape_changed;
    use crate::services::transcript_reader::types::MAX_TURN_TOOL_CALLS;
    use crate::services::transcript_reader::{
        assistant_report_from_file, read_last_assistant_message_from_file, read_tail,
        read_tail_from_file, TranscriptFormat, TranscriptTail, UnavailableReason,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn circuit_report_revision_tracks_assistant_response_not_user_or_tool_activity() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let assistant = r#"{"type":"assistant","message":{"id":"a","role":"assistant","content":[{"type":"text","text":"Done."}]}}"#;
        writeln!(file, "{assistant}").unwrap();
        let first = assistant_report_from_file(file.path(), TranscriptFormat::ClaudeCode).unwrap();
        let user =
            r#"{"type":"user","message":{"role":"user","content":"Now finish and open the PR"}}"#;
        writeln!(file, "{user}").unwrap();
        let waiting =
            assistant_report_from_file(file.path(), TranscriptFormat::ClaudeCode).unwrap();
        assert_eq!(waiting.revision, first.revision);
        let tool = r#"{"type":"assistant","message":{"id":"tool","role":"assistant","content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"git status"}}]}}"#;
        writeln!(file, "{tool}").unwrap();
        assert_eq!(
            assistant_report_from_file(file.path(), TranscriptFormat::ClaudeCode)
                .unwrap()
                .revision,
            first.revision
        );
        // Even byte-identical responses have distinct positions in the log.
        writeln!(file, "{assistant}").unwrap();
        let second = assistant_report_from_file(file.path(), TranscriptFormat::ClaudeCode).unwrap();
        assert_eq!(second.text, first.text);
        assert_ne!(second.revision, first.revision);
        file.write_all(br#"{"type":"assistant""#).unwrap();
        assert_eq!(
            assistant_report_from_file(file.path(), TranscriptFormat::ClaudeCode)
                .unwrap()
                .revision,
            second.revision
        );
    }

    #[test]
    fn encode_path_matches_claude_code_form() {
        assert_eq!(encode_path("X:\\src\\buildmesh"), "X--src-buildmesh");
        assert_eq!(
            encode_path("X:\\src\\buildmesh\\.claude\\worktrees\\foo"),
            "X--src-buildmesh--claude-worktrees-foo"
        );
    }

    #[test]
    fn is_synthetic_detects_local_command_caveat() {
        assert!(is_synthetic_message("<local-command-caveat>Caveat…"));
        assert!(!is_synthetic_message("Fix the login bug"));
    }

    #[test]
    fn concat_text_blocks_handles_string_and_array() {
        assert_eq!(
            concat_text_blocks(Some(&serde_json::json!("hello"))),
            "hello"
        );
        let arr = serde_json::json!([
            {"type": "thinking", "thinking": "ignored"},
            {"type": "text", "text": "a"},
            {"type": "tool_use", "name": "Read", "input": {}},
            {"type": "text", "text": "b"},
        ]);
        assert_eq!(concat_text_blocks(Some(&arr)), "a\nb");
    }

    #[test]
    fn missing_session_id_is_no_session() {
        assert_eq!(
            read_tail(TranscriptFormat::ClaudeCode, None, "X:\\src\\buildmesh", 10),
            TranscriptTail::unavailable(UnavailableReason::NoSession)
        );
        assert_eq!(
            read_tail(
                TranscriptFormat::ClaudeCode,
                Some(""),
                "X:\\src\\buildmesh",
                10
            ),
            TranscriptTail::unavailable(UnavailableReason::NoSession)
        );
    }

    #[test]
    fn missing_file_is_no_transcript() {
        // A session id that cannot resolve to a real file on disk degrades to
        // NoTranscript rather than erroring.
        let result = read_tail(
            TranscriptFormat::ClaudeCode,
            Some("definitely-not-a-real-session-00000000"),
            "X:\\nowhere\\does\\not\\exist",
            10,
        );
        assert_eq!(
            result,
            TranscriptTail::unavailable(UnavailableReason::NoTranscript)
        );
    }

    #[test]
    fn unreadable_file_path_is_unreadable() {
        let result = read_tail_from_file(
            Path::new("X:\\nope\\missing.jsonl"),
            10,
            TranscriptFormat::ClaudeCode,
        );
        assert_eq!(
            result,
            TranscriptTail::unavailable(UnavailableReason::Unreadable)
        );
    }

    fn write_long_transcript(rounds: usize) -> PathBuf {
        let mut body = String::new();
        for i in 0..rounds {
            body.push_str(&format!(
                r#"{{"type":"user","message":{{"role":"user","content":"prompt {i}"}},"uuid":"u{i}"}}
"#,
            ));
            body.push_str(&format!(
                r#"{{"type":"assistant","message":{{"id":"msg_{i}","role":"assistant","content":[{{"type":"tool_use","name":"Read","input":{{"file_path":"/a/{i}"}}}}]}},"uuid":"a{i}"}}
"#,
            ));
        }
        body.push_str(
            r#"{"type":"user","message":{"role":"user","content":"final question"},"uuid":"u_final"}
"#,
        );
        body.push_str(
            r#"{"type":"assistant","message":{"id":"msg_final","role":"assistant","content":[{"type":"text","text":"The blocking question: shall I proceed?"}]},"uuid":"a_final"}
"#,
        );
        // Suffix by thread id so parallel tests don't trample each other
        // (cargo runs tests in parallel by default; sharing one temp file
        // produces a race that surfaces as a ShapeChanged from the *other*
        // test's larger fixture).
        let suffix = std::process::id();
        let path = std::env::temp_dir().join(format!(
            "buildmesh_test_long_transcript_{suffix}_{rounds}.jsonl"
        ));
        std::fs::write(&path, &body).unwrap();
        path
    }

    #[test]
    fn read_last_assistant_message_matches_full_reader_on_long_transcript() {
        let path = write_long_transcript(2_000);
        let full = read_tail_from_file(&path, 1, TranscriptFormat::ClaudeCode);
        let cheap = read_last_assistant_message_from_file(&path, TranscriptFormat::ClaudeCode);
        std::fs::remove_file(&path).ok();
        let full_last = match full {
            TranscriptTail::Available {
                last_assistant_message,
                ..
            } => last_assistant_message,
            other => panic!("full read should be available, got {other:?}"),
        };
        let cheap_last = match cheap {
            TranscriptTail::Available {
                last_assistant_message,
                turns,
            } => {
                assert!(turns.is_empty(), "cheap reader must not return turns");
                last_assistant_message
            }
            other => panic!("cheap read should be available, got {other:?}"),
        };
        assert_eq!(cheap_last, full_last);
        assert_eq!(
            cheap_last.as_deref(),
            Some("The blocking question: shall I proceed?")
        );
    }

    #[test]
    fn read_last_assistant_message_falls_back_when_window_lacks_assistant_text() {
        // 10,000 rounds of (user, assistant tool call) blows the file well past
        // 256 KiB; the bounded window lands on tool-call turns only, with no
        // assistant text. The defensive fallback must re-parse the whole file
        // so the final assistant text is still recovered — otherwise we would
        // silently return None for a Coordinator that needs the blocking
        // question.
        let path = write_long_transcript(10_000);
        let cheap = read_last_assistant_message_from_file(&path, TranscriptFormat::ClaudeCode);
        std::fs::remove_file(&path).ok();
        let cheap_last = match cheap {
            TranscriptTail::Available {
                last_assistant_message,
                ..
            } => last_assistant_message,
            other => panic!("cheap read should be available, got {other:?}"),
        };
        assert_eq!(
            cheap_last.as_deref(),
            Some("The blocking question: shall I proceed?"),
            "fallback must re-parse the whole file when the bounded window has no assistant text"
        );
    }

    #[test]
    fn contract_parses_tail_and_last_assistant_message() {
        let tail = read_tail_from_file(
            &fixture("claude_code", "claude_code_transcript.jsonl"),
            10,
            TranscriptFormat::ClaudeCode,
        );
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = tail
        else {
            panic!("fixture should parse to an available tail, got {tail:?}");
        };
        // The fixture's noise lines (summary, mode, queue-operation, system,
        // thinking-only, tool_result echo, local-command-caveat) are all
        // dropped; only genuine turns survive.
        let roles: Vec<&str> = turns.iter().map(|t| t.role.as_str()).collect();
        assert_eq!(
            roles,
            vec!["user", "assistant", "assistant", "user", "assistant"],
            "turns: {turns:#?}"
        );
        // First turn is the real user prompt (caveat line skipped before it).
        assert_eq!(turns[0].text, "Fix the login redirect bug");
        // The two split assistant lines (text then tool_use, same message.id)
        // coalesce into one turn carrying both.
        assert_eq!(turns[1].text, "I'll look into the login redirect.");
        assert_eq!(turns[1].tool_calls.len(), 1);
        assert_eq!(turns[1].tool_calls[0].name, "Read");
        assert_eq!(turns[1].tool_calls[0].input["file_path"], "src/login.ts");
        // The blocking question is the most recent assistant text.
        assert_eq!(
            turns[4].text,
            "Found it — the redirect drops the query string. Shall I apply the fix?"
        );
        assert_eq!(
            last_assistant_message.as_deref(),
            Some("Found it — the redirect drops the query string. Shall I apply the fix?")
        );
    }

    #[test]
    fn tail_length_limits_returned_turns() {
        let two = read_tail_from_file(
            &fixture("claude_code", "claude_code_transcript.jsonl"),
            2,
            TranscriptFormat::ClaudeCode,
        );
        let TranscriptTail::Available { turns, .. } = two else {
            panic!("expected available");
        };
        assert_eq!(turns.len(), 2, "tail=2 returns only the last two turns");
        // Last two of [user, asst, asst, user, asst] = [user, asst].
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[1].role, "assistant");
    }

    #[test]
    fn last_assistant_message_is_from_full_transcript_not_window() {
        // The blocking question is "the last assistant message" regardless of
        // how small a tail the caller asks for: a rolling buffer that retains
        // only the trailing user turn must still surface it (issue #335 — the
        // last-message tracking is independent of the bounded turn window).
        let lines = vec![
            r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"Shall I apply the fix?"}]}}"#.to_string(),
            r#"{"type":"user","message":{"role":"user","content":"wait, first explain"}}"#.to_string(),
        ];
        let parsed = parse_turns(lines.into_iter(), 1);
        // The retained window (keep=1) is just the trailing user turn …
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].role, "user");
        // … but the last assistant message is still recovered from the full stream.
        assert_eq!(
            parsed.last_assistant_message.as_deref(),
            Some("Shall I apply the fix?")
        );
    }

    #[test]
    fn rolling_buffer_retains_only_the_last_keep_turns() {
        // Stream more turns than `keep`; the buffer holds the last `keep`, in
        // order, while still tracking the last assistant message (issue #335).
        let mut lines = Vec::new();
        for i in 0..50 {
            lines.push(format!(
                r#"{{"type":"user","message":{{"role":"user","content":"prompt {i}"}}}}"#
            ));
            lines.push(format!(
                r#"{{"type":"assistant","message":{{"id":"m{i}","role":"assistant","content":[{{"type":"text","text":"reply {i}"}}]}}}}"#
            ));
        }
        let parsed = parse_turns(lines.into_iter(), 3);
        assert_eq!(parsed.turns.len(), 3, "buffer never exceeds keep");
        // The last three of [… user 49, assistant 49] are user49, asst49 — wait,
        // order is user,assistant per round, so the tail is asst48? Build it out:
        let roles: Vec<&str> = parsed.turns.iter().map(|t| t.role.as_str()).collect();
        assert_eq!(roles, vec!["assistant", "user", "assistant"]);
        assert_eq!(parsed.turns[2].text, "reply 49");
        assert_eq!(
            parsed.last_assistant_message.as_deref(),
            Some("reply 49"),
            "last assistant message survives eviction of its turn from the window"
        );
    }

    #[test]
    fn shape_changed_fixture_degrades_not_panics() {
        let tail = read_tail_from_file(
            &fixture("claude_code", "claude_code_transcript_shape_changed.jsonl"),
            10,
            TranscriptFormat::ClaudeCode,
        );
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::ShapeChanged),
            "a renamed/missing-field transcript must degrade loudly, never panic"
        );
    }

    #[test]
    fn empty_session_degrades_to_empty_not_shape_changed() {
        // A file whose only lines are deliberately-skipped ones (caveat, a
        // tool-result echo, a thinking-only assistant) plus non-message lines
        // (summary/mode/system) is a genuinely-quiet session — `Empty`, the
        // quiet degrade, not the loud `ShapeChanged`.
        let tail = read_tail_from_file(
            &fixture("claude_code", "claude_code_transcript_empty.jsonl"),
            10,
            TranscriptFormat::ClaudeCode,
        );
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::Empty),
            "a structurally well-formed but turn-less session is Empty, not ShapeChanged"
        );
    }

    #[test]
    fn empty_session_is_empty_through_the_digest_reader_too() {
        // The cheap digest path must make the same empty-vs-shape distinction.
        let tail = read_last_assistant_message_from_file(
            &fixture("claude_code", "claude_code_transcript_empty.jsonl"),
            TranscriptFormat::ClaudeCode,
        );
        assert_eq!(tail, TranscriptTail::unavailable(UnavailableReason::Empty));
    }

    #[test]
    fn one_malformed_line_tips_an_otherwise_empty_file_to_shape_changed() {
        // The discriminator is "did we see a malformed user/assistant line",
        // not "is the file empty": a single renamed-field line among skipped
        // ones still means the shape broke.
        let lines = vec![
            r#"{"type":"user","message":{"role":"user","content":"<local-command-caveat>noise</local-command-caveat>"}}"#.to_string(),
            r#"{"type":"assistant","message":{"id":"m1","author":"assistant","blocks":[{"type":"text","text":"renamed role+content"}]}}"#.to_string(),
        ];
        let parsed = parse_turns(lines.into_iter(), 10);
        assert!(parsed.turns.is_empty());
        assert!(
            parsed.saw_malformed,
            "a renamed role/content line is malformed"
        );
        assert_eq!(
            empty_or_shape_changed(parsed.saw_malformed),
            UnavailableReason::ShapeChanged
        );
    }

    #[test]
    fn tool_calls_per_turn_are_capped() {
        let mut calls = String::new();
        for i in 0..(MAX_TURN_TOOL_CALLS + 10) {
            if i > 0 {
                calls.push(',');
            }
            calls.push_str(&format!(
                r#"{{"type":"tool_use","id":"t{i}","name":"Read","input":{{"file_path":"/a/{i}"}}}}"#
            ));
        }
        let line = format!(
            r#"{{"type":"assistant","message":{{"id":"m1","role":"assistant","content":[{calls}]}}}}"#
        );
        let parsed = parse_turns(std::iter::once(line), 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(
            parsed.turns[0].tool_calls.len(),
            MAX_TURN_TOOL_CALLS,
            "a single turn's tool calls are bounded so no turn dominates the payload"
        );
    }

    #[test]
    fn coalesced_turn_tool_calls_are_capped_across_lines() {
        // Two lines of the same message.id, each already at the cap, must still
        // coalesce to a single capped turn — the merge re-caps, not just appends.
        let mk = |start: usize| {
            let mut calls = String::new();
            for i in 0..MAX_TURN_TOOL_CALLS {
                if i > 0 {
                    calls.push(',');
                }
                calls.push_str(&format!(
                    r#"{{"type":"tool_use","id":"t{}","name":"Read","input":{{}}}}"#,
                    start + i
                ));
            }
            format!(
                r#"{{"type":"assistant","message":{{"id":"m1","role":"assistant","content":[{calls}]}}}}"#
            )
        };
        let parsed = parse_turns(vec![mk(0), mk(1000)].into_iter(), 10);
        assert_eq!(
            parsed.turns.len(),
            1,
            "same message.id coalesces to one turn"
        );
        assert_eq!(parsed.turns[0].tool_calls.len(), MAX_TURN_TOOL_CALLS);
    }

    #[test]
    fn first_text_block_takes_only_the_first_block() {
        assert_eq!(first_text_block(Some(&serde_json::json!("hello"))), "hello");
        let arr = serde_json::json!([
            {"type": "thinking", "thinking": "ignored"},
            {"type": "text", "text": "first"},
            {"type": "text", "text": "second"},
        ]);
        // concat_text_blocks would join to "first\nsecond"; first_text_block
        // returns just "first" with no interior newline.
        assert_eq!(first_text_block(Some(&arr)), "first");
        assert_eq!(concat_text_blocks(Some(&arr)), "first\nsecond");
        assert_eq!(first_text_block(None), "");
    }

    /// The native `Bash` call that owns a launch receipt: an explicit
    /// `run_in_background`, or a foreground command whose own receipt is
    /// the timeout phrasing.
    fn bash_call_line(call_id: &str, run_in_background: bool) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {
                "id": format!("m-{call_id}"),
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": call_id,
                    "name": "Bash",
                    "input": {"command": "npm run dev", "run_in_background": run_in_background},
                }],
            },
        })
        .to_string()
    }

    /// A tool call plus its result, for any tool name.
    fn tool_call_and_result(call_id: &str, tool: &str, content: &str) -> [String; 2] {
        [
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "id": format!("m-{call_id}"),
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": call_id,
                        "name": tool,
                        "input": {"pattern": "background"},
                    }],
                },
            })
            .to_string(),
            serde_json::json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [{
                        "tool_use_id": call_id,
                        "type": "tool_result",
                        "content": content,
                        "is_error": false,
                    }],
                },
            })
            .to_string(),
        ]
    }

    /// This module's own source, as an agent reading it sees it: both
    /// launch phrasings quoted as prose (issue #2264).
    const READ_QUOTING_LAUNCH: &str = concat!(
        "659 // Command running in background with ID: xyz.\n",
        "660 //\n",
        "681 const LAUNCH_MARKER: &str = \"You will be notified when it completes\";",
    );

    /// The explicit-background launch receipt, as the `tool_result` of
    /// `call_id`.
    fn launch_line(call_id: &str, id: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"tool_use_id":"{call_id}","type":"tool_result","content":"Command running in background with ID: {id}. Output is being written to: /tmp/{id}.output. You will be notified when it completes. To check interim output, use Read on that file path.","is_error":false}}]}}}}"#
        )
    }

    /// The timeout phrasing: a foreground `Bash` call that outlived its
    /// limit and was moved to the background.
    fn timeout_launch_line(call_id: &str, id: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"tool_use_id":"{call_id}","type":"tool_result","content":"Command did not complete within its 120s timeout and was moved to the background (ID: {id}). Output is being written to: /tmp/{id}.output. You will be notified when it completes. To check interim output, use Read on that file path.","is_error":false}}]}}}}"#
        )
    }

    fn notification_line(id: &str, status: &str) -> String {
        format!(
            r#"{{"type":"queue-operation","operation":"enqueue","timestamp":"2026-07-18T10:00:00.000Z","sessionId":"s","content":"<task-notification>\n<task-id>{id}</task-id>\n<tool-use-id>t1</tool-use-id>\n<output-file>/tmp/{id}.output</output-file>\n<status>{status}</status>\n</task-notification>"}}"#
        )
    }

    #[test]
    fn launched_without_notification_is_pending() {
        let pending = pending_background_task_ids(
            [
                bash_call_line("t1", true),
                launch_line("t1", "byt1iw94s"),
                bash_call_line("t2", false),
                timeout_launch_line("t2", "b97ep9a8n"),
            ]
            .into_iter(),
        );
        assert_eq!(
            pending,
            vec!["byt1iw94s".to_string(), "b97ep9a8n".to_string()],
            "both launch phrasings must register a pending task"
        );
    }

    #[test]
    fn terminal_notification_clears_pending() {
        // `completed` and `failed` both mean the wait is over — the harness
        // re-invokes the agent either way.
        let pending = pending_background_task_ids(
            [
                bash_call_line("t1", true),
                launch_line("t1", "aaa"),
                bash_call_line("t2", true),
                launch_line("t2", "bbb"),
                notification_line("aaa", "completed"),
                notification_line("bbb", "failed"),
            ]
            .into_iter(),
        );
        assert!(
            pending.is_empty(),
            "terminal notifications end the wait, got {pending:?}"
        );
    }

    #[test]
    fn running_status_notification_does_not_clear_pending() {
        // Real transcripts carry `<status>running</status>` notifications; the
        // task is still in flight, so the Stop is still a false yield.
        let pending = pending_background_task_ids(
            [
                bash_call_line("t1", true),
                launch_line("t1", "ccc"),
                notification_line("ccc", "running"),
            ]
            .into_iter(),
        );
        assert_eq!(pending, vec!["ccc".to_string()]);
    }

    #[test]
    fn free_text_mentioning_the_promise_is_not_a_launch() {
        // An assistant merely *quoting* the launch text (e.g. discussing these
        // docs) must not register a phantom pending task — only a tool_result
        // block counts.
        let assistant = r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"The tool says: moved to the background (ID: zzz). You will be notified when it completes."}]}}"#.to_string();
        assert!(pending_background_task_ids(std::iter::once(assistant)).is_empty());
    }

    #[test]
    fn read_result_quoting_this_files_launch_examples_is_not_a_launch() {
        // Issue #2264: an agent reading the scanner's own source sees both
        // launch phrasings as prose. No notification would arrive for
        // `xyz`, so admitting it registers a wait no notification can end.
        let lines = tool_call_and_result("t-read", "Read", READ_QUOTING_LAUNCH);
        assert!(
            pending_background_task_ids(lines.into_iter()).is_empty(),
            "a Read result quoting the receipt owns no task"
        );
    }

    #[test]
    fn a_read_result_that_opens_with_the_receipt_owns_no_task() {
        // The tool correlation, isolated from the wording: this result
        // carries a pristine receipt, so only the answer to "which call
        // produced it" can reject it. Without that correlation, reading a
        // saved transcript would register a task each time it is scanned.
        let lines = tool_call_and_result(
            "t-read",
            "Read",
            "Command running in background with ID: xyz. Output is being written to: /tmp/xyz.output. You will be notified when it completes.",
        );
        assert!(
            pending_background_task_ids(lines.into_iter()).is_empty(),
            "only a launch-capable call can own a receipt"
        );
    }

    #[test]
    fn a_receipt_quoted_inside_result_content_is_not_a_launch() {
        // The wording guard, isolated from the tool: even a Bash result
        // quotes launch text back as content (a `cat`, a JSON payload).
        // The harness writes the receipt itself at the start of its
        // result; anywhere else it is quoted material.
        let receipt =
            "Command running in background with ID: xyz. You will be notified when it completes.";
        let quoted = format!("{{\"note\":\"{receipt}\"}}");
        let cat = format!("$ cat notes.txt\n{receipt}\n");
        for (case, content) in [("json", quoted.as_str()), ("cat", cat.as_str())] {
            let lines = tool_call_and_result("t-x", "Bash", content);
            assert!(
                pending_background_task_ids(lines.into_iter()).is_empty(),
                "{case} content must not register a launch"
            );
        }
    }

    #[test]
    fn launch_receipt_without_its_own_bash_call_is_not_a_launch() {
        // Missing correlation is not permission to guess: a receipt whose
        // `Bash` call is absent, or whose result slot another tool owns,
        // claims no task.
        assert!(
            pending_background_task_ids(std::iter::once(launch_line("t1", "xyz"))).is_empty(),
            "a receipt with no observed tool call owns nothing"
        );
        let mut lines = tool_call_and_result("t1", "Read", "no launch here").to_vec();
        // The receipt claims result slot `t1`, which the Read already owns.
        lines.push(launch_line("t1", "xyz"));
        assert!(
            pending_background_task_ids(lines.into_iter()).is_empty(),
            "a receipt correlated to a non-launch call owns nothing"
        );
    }

    #[test]
    fn real_launch_survives_quoted_examples_in_the_same_transcript() {
        // The phantom must not be admitted, and admitting it must not cost
        // the genuine task sitting beside it.
        let mut lines = tool_call_and_result("t-read", "Read", READ_QUOTING_LAUNCH).to_vec();
        lines.extend([bash_call_line("t1", true), launch_line("t1", "byt1iw94s")]);
        assert_eq!(
            pending_background_task_ids(lines.into_iter()),
            vec!["byt1iw94s".to_string()],
            "only the correlated Bash launch is pending"
        );
    }

    #[test]
    fn count_pending_none_on_unreadable_file() {
        // Unknown must never read as "no pending work" — the caller falls back
        // to marking attention.
        assert_eq!(
            count_pending_background_tasks(Path::new("X:\\nope\\missing.jsonl")),
            None
        );
    }

    #[test]
    fn stopped_task_requires_a_successful_matching_task_stop_result() {
        let records: Vec<serde_json::Value> =
            std::fs::read_to_string(fixture("claude_code", "stopped_background_task.jsonl"))
                .unwrap()
                .lines()
                .take(4)
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
        for case in [
            "success",
            "failed",
            "wrong_call",
            "wrong_task",
            "different_tool",
            "missing_call",
            "quoted_success",
        ] {
            let mut replay = records.clone();
            match case {
                "failed" => replay[3]["message"]["content"][0]["is_error"] = true.into(),
                "wrong_call" => {
                    replay[3]["message"]["content"][0]["tool_use_id"] = "unrelated".into()
                }
                "wrong_task" => {
                    let result = serde_json::json!({"task_id":"other", "message":"Successfully stopped task: other (vite)"});
                    replay[3]["message"]["content"][0]["content"] = result.to_string().into();
                }
                "different_tool" => replay[2]["message"]["content"][0]["name"] = "Read".into(),
                "missing_call" => {
                    replay.remove(2);
                }
                "quoted_success" => {
                    replay[3]["message"]["content"][0]["content"] =
                        "The tool says: Successfully stopped task: bziuux8nz (vite)".into()
                }
                _ => {}
            }
            let pending =
                pending_background_task_ids(replay.iter().map(|record| record.to_string()));
            let expected = if case == "success" {
                vec![]
            } else {
                vec!["bziuux8nz".to_owned()]
            };
            assert_eq!(pending, expected, "{case}");
        }
    }

    #[test]
    fn count_pending_reads_real_fixture_shape() {
        let suffix = std::process::id();
        let path =
            std::env::temp_dir().join(format!("buildmesh_test_pending_tasks_{suffix}.jsonl"));
        let body = [
            bash_call_line("t1", true),
            launch_line("t1", "early"),
            notification_line("early", "completed"),
            bash_call_line("t2", true),
            launch_line("t2", "late"),
        ]
        .join("\n");
        std::fs::write(&path, body).unwrap();
        let count = count_pending_background_tasks(&path);
        std::fs::remove_file(&path).ok();
        assert_eq!(count, Some(1), "one launched-but-unnotified task");
    }

    // Issue #2263 — the recorded transition between two native sessions.
    const PREVIOUS: &str = "d1b48df8-eb97-4867-b365-a34cdbdc5900";
    const CONTINUED: &str = "a5acfe57-a409-4aaa-a644-4f7621934663";

    fn continued_record(session_id: &str, successor: serde_json::Value) -> String {
        serde_json::json!({
            "type": "continued-in",
            "sessionId": session_id,
            "continuedInSessionId": successor,
            "timestamp": "2026-10-12T18:57:48.345Z",
        })
        .to_string()
    }

    fn scan(lines: &[String], session_id: &str) -> Option<String> {
        declared_continuation(session_id, lines.iter().cloned())
    }

    #[test]
    fn a_recorded_transition_is_read_back_verbatim() {
        // The run-455 shape, as Claude Code wrote it.
        assert_eq!(
            scan(
                &[continued_record(PREVIOUS, serde_json::json!(CONTINUED))],
                PREVIOUS
            )
            .as_deref(),
            Some(CONTINUED)
        );
    }

    #[test]
    fn only_own_well_formed_records_count() {
        // A record naming another predecessor proves nothing about this file.
        assert_eq!(
            scan(
                &[continued_record(CONTINUED, serde_json::json!(PREVIOUS))],
                PREVIOUS
            ),
            None
        );
        // No successor, empty successor, or a self-continuation.
        for successor in [
            serde_json::json!(null),
            serde_json::json!(""),
            serde_json::json!(PREVIOUS),
        ] {
            assert_eq!(
                scan(&[continued_record(PREVIOUS, successor.clone())], PREVIOUS),
                None,
                "{successor}"
            );
        }
        // Right fields, wrong record type; and unparseable lines.
        assert_eq!(
            scan(
                &[serde_json::json!({
                    "type": "summary",
                    "sessionId": PREVIOUS,
                    "continuedInSessionId": CONTINUED,
                })
                .to_string()],
                PREVIOUS
            ),
            None
        );
        assert_eq!(
            scan(&["{\"type\":\"continued-in\",".to_string()], PREVIOUS),
            None
        );
    }

    #[test]
    fn the_newest_own_record_wins() {
        let later = "0c9e6d55-2f18-4a7b-b6c2-51f0a3d97c48";
        assert_eq!(
            scan(
                &[
                    continued_record(PREVIOUS, serde_json::json!(CONTINUED)),
                    continued_record(PREVIOUS, serde_json::json!(later)),
                ],
                PREVIOUS
            )
            .as_deref(),
            Some(later)
        );
    }

    #[test]
    fn an_unreadable_transcript_proves_no_continuation() {
        let missing = std::env::temp_dir().join(format!(
            "buildmesh_test_no_such_session_{}.jsonl",
            std::process::id()
        ));
        assert_eq!(continued_session_id(PREVIOUS, &missing), None);
    }
}

#[cfg(test)]
mod file_contract_tests {
    use super::*;
    use crate::services::transcript_reader::test_support::{assert_jsonl_contract, fixture};

    #[test]
    fn reader_handles_tail_digest_malformed_empty_shape_changed_and_unreadable_files() {
        assert_jsonl_contract(
            &ClaudeCodeAdapter,
            &fixture("claude_code", "claude_code_transcript.jsonl"),
            &fixture("claude_code", "shape_changed.jsonl"),
            "Found it — the redirect drops the query string. Shall I apply the fix?",
        );
    }
}
