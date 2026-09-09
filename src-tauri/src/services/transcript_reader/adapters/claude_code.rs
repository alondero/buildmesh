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
use crate::services::transcript_reader::adapter::{
    HookClassification, HookDecision, LocateCtx, TranscriptAdapter,
};
use crate::services::transcript_reader::types::{
    build_tail, cap_tool_calls, effective_tail, merge_into, push_bounded, truncate,
    truncate_json_strings, Parsed, ToolCall, Turn,
    MAX_TOOL_STRING, MAX_TURN_TEXT,
};
use crate::services::transcript_paths::{concat_text_blocks, encode_path, is_synthetic_message};

/// Drop-in [`TranscriptAdapter`] for Claude Code (also the default for any
/// unknown harness id).
pub(crate) struct ClaudeCodeAdapter;

impl TranscriptAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        "claude_code"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        Some(transcript_path(ctx.session_id, ctx.node_path))
    }

    fn parse(&self, lines: Box<dyn Iterator<Item = String> + '_>, keep: usize) -> Parsed {
        parse_turns(lines, keep)
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
            && !concat_text_blocks(value.get("message").and_then(|message| message.get("content")))
                .trim()
                .is_empty()
    }

    fn classify_hook(
        &self,
        body: &[u8],
        _provider: &str,
    ) -> Option<HookClassification> {
        // Claude Code's documented Notification envelope is "… needs
        // your permission to use X" — anchored to the verb phrase, not
        // a bare "permission" substring, so prose like "Permission was
        // already granted for Bash" cannot false-positive. Cursor's
        // envelope shape matches Claude Code's, so Cursor delegates
        // here.
        let payload: serde_json::Value = serde_json::from_slice(body).ok()?;
        // The HookPayload struct in routes/attention.rs applies the
        // `hookEventName` alias; we read raw `serde_json::Value` here.
        let event = payload
            .get("hook_event_name")
            .or_else(|| payload.get("hookEventName"))
            .and_then(|n| n.as_str())
            .map(str::to_ascii_lowercase);
        if event.as_deref() != Some("notification") {
            return None;
        }
        payload
            .get("message")
            .and_then(|m| m.as_str())
            .is_some_and(|m| m.to_ascii_lowercase().contains("needs your permission"))
            .then_some(HookClassification {
                decision: HookDecision::MarkInput,
                kind: None,
                notification_type: None,
            })
    }
}

/// Build the expected on-disk path of a Claude Code session transcript:
/// `<claude_dir>/projects/<encoded node_path>/<session_id>.jsonl`.
pub(crate) fn transcript_path(session_id: &str, node_path: &str) -> PathBuf {
    env::claude_dir()
        .join("projects")
        .join(encode_path(node_path))
        .join(format!("{session_id}.jsonl"))
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
pub(crate) fn parse_turns(
    lines: impl Iterator<Item = String>,
    keep: usize,
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
                    text: truncate(&text, MAX_TURN_TEXT),
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
                    merge_into(last, &text, extract_tool_calls(raw_content));
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

// --- Pending background tasks (issue #878) ---
//
// Claude Code ends its turn when it launches background work (a
// `run_in_background` Bash call, or a foreground command that outlives
// its timeout and is moved to the background) and auto-resumes itself
// when the task's `<task-notification>` arrives. A Stop hook that fires
// with such work still pending is NOT "the user is needed". The
// transcript records both ends deterministically:
//
//   launch  — a `tool_result` whose text says "…background… (ID: xyz) …
//             You will be notified when it completes."
//   finish  — a line (queue-operation, or the queued_command attachment
//             that re-invokes the agent) carrying
//             `<task-id>xyz</task-id>`.
//
// Pending = launched minus notified.

/// Matches the task id in either launch phrasing:
/// `Command running in background with ID: xyz.` and
/// `…was moved to the background (ID: xyz)`.
static LAUNCH_ID: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"\bID: ([A-Za-z0-9_-]+)").unwrap());
/// A task-notification's id paired with its status, non-greedy so
/// several notifications on one line pair correctly. Real transcripts
/// carry `<status>running</status>` notifications too (e.g. a foreground
/// command moved to the background) — only a terminal status means the
/// wait is over.
static NOTIFIED_ID: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(r"<task-id>([A-Za-z0-9_-]+)</task-id>.*?<status>([a-z_]+)</status>").unwrap()
});
/// The phrase that makes a `tool_result` a background-task launch. Both
/// known launch phrasings carry it; matching the promise (rather than
/// the two exact sentences) keeps the scan stable across minor wording
/// changes.
const LAUNCH_MARKER: &str = "You will be notified when it completes";

/// Count background tasks launched but not yet notified in a Claude Code
/// transcript. `None` = the file could not be read — the caller must
/// treat that as "unknown" and fall back to its pre-#878 behaviour,
/// never as "no pending work".
pub fn count_pending_background_tasks(path: &Path) -> Option<usize> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    Some(pending_background_task_ids(reader.lines().map_while(Result::ok)).len())
}

/// Pure scan over JSONL lines: launched-task ids with no matching
/// `<task-id>` notification, in launch order. Split from the I/O
/// wrapper so tests drive it with inline fixtures.
pub(crate) fn pending_background_task_ids(lines: impl Iterator<Item = String>) -> Vec<String> {
    let mut launched: Vec<String> = Vec::new();
    let mut notified: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in lines {
        // The notification marker is matched on the raw line: it
        // appears in `queue-operation` lines and in the queued_command
        // attachment that re-invokes the agent, and caring which one
        // carries it would couple us to more of the shape than we
        // need.
        for cap in NOTIFIED_ID.captures_iter(&line) {
            if &cap[2] != "running" {
                notified.insert(cap[1].to_string());
            }
        }
        // Launches only count inside a tool_result block — free text
        // merely *mentioning* the promise (e.g. an agent quoting these
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
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                continue;
            }
            let text = match block.get("content") {
                Some(serde_json::Value::String(s)) => s.clone(),
                other => concat_text_blocks(other),
            };
            if !text.contains(LAUNCH_MARKER) {
                continue;
            }
            if let Some(cap) = LAUNCH_ID.captures(&text) {
                launched.push(cap[1].to_string());
            }
        }
    }
    launched.retain(|id| !notified.contains(id));
    launched
}

#[allow(unused_imports)]
use crate::services::transcript_reader::types::TranscriptTail as _UnusedTranscriptTail; // keep type accessible from tests