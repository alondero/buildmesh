//! Antigravity (`agy`) transcript adapter (issue #1283, #1367).
//!
//! Per-conversation JSONL under `~/.gemini/antigravity-cli/brain/<conv>/
//! .system_generated/logs/transcript.jsonl` (token-efficient form), with
//! `transcript_full.jsonl` as the untruncated fallback. One JSON object per
//! turn — flat shape, no `message.id` coalescing.
//!
//! Issue #1661 step 5: AgY is the **third harness migrated end-to-end**.
//! `agy_locator_in` + `find_agy_transcript` + `parse_agy_turns` +
//! `is_agy_synthetic` + `extract_agy_tool_calls` all live in this file.
//! The capture poller in `services::agy_session` already delegates to
//! `agy_locator_in`, so the seam-inversion is one import rewrite in
//! step 10.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::env;
use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::{
    cap_tool_calls, push_bounded, truncate, truncate_json_strings, Parsed, ToolCall, Turn,
    MAX_TOOL_STRING, MAX_TURN_TEXT,
};

/// Drop-in [`TranscriptAdapter`] for Antigravity.
pub(crate) struct AgyAdapter;

impl TranscriptAdapter for AgyAdapter {
    fn id(&self) -> &'static str {
        "agy"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        let home = env::agy_dir_for_env(env::runtime_for_spawn_path(ctx.node_path), ctx.node_path)?;
        let home = PathBuf::from(env::to_host_path(&home.to_string_lossy()));
        agy_locator_in(&home.join("brain"), ctx.session_id)
    }

    fn parse(&self, lines: Box<dyn Iterator<Item = String> + '_>, keep: usize) -> Parsed {
        parse_agy_turns(lines, keep)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        value.get("source").and_then(|source| source.as_str()) == Some("MODEL")
            && value
                .get("content")
                .and_then(|content| content.as_str())
                .is_some_and(|text| !text.trim().is_empty())
    }
}

/// Pure AgY locator — split from the env-aware wrapper so the contract
/// test drives the resolve against a temp brain root instead of touching
/// `~/.gemini`. `pub(crate)` so the AGY capture poller
/// (`services::agy_session`) resolves the same path rather than duplicating
/// the layout. The path it returns (when both files exist) is
/// `transcript.jsonl` first, falling back to `transcript_full.jsonl` when
/// the short variant is missing — issue #1283 acceptance criterion #2.
pub(crate) fn agy_locator_in(brain_root: &Path, session_id: &str) -> Option<PathBuf> {
    let logs = brain_root
        .join(session_id)
        .join(".system_generated")
        .join("logs");
    let short = logs.join("transcript.jsonl");
    if short.exists() {
        return Some(short);
    }
    let full = logs.join("transcript_full.jsonl");
    if full.exists() {
        return Some(full);
    }
    None
}

/// Parse AgY JSONL lines into logical turns, honouring the same
/// [`Parsed`] contract as [`super::super::parse_turns`] (rolling
/// `keep`-bounded turn window, whole-stream last-assistant-message
/// tracking, malformed flag). Each line is one self-contained turn — no
/// message-id coalescing is needed for AGY because its emission shape
/// never splits a single assistant message across multiple JSONL lines.
pub(crate) fn parse_agy_turns(
    lines: impl Iterator<Item = String>,
    keep: usize,
) -> Parsed {
    let keep = keep.max(1);
    let mut turns: VecDeque<Turn> = VecDeque::new();
    let mut last_assistant_message: Option<String> = None;
    let mut saw_malformed = false;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        // SYSTEM-side TASK_NOTIFICATION injections are harness plumbing, not
        // a turn — skip before the role gate, so a session whose only lines
        // are notifications degrades as `Empty`, not `ShapeChanged`.
        match val.get("source").and_then(|s| s.as_str()) {
            Some("USER_EXPLICIT") => {
                let Some(text) = val.get("content").and_then(|c| c.as_str()) else {
                    saw_malformed = true;
                    continue;
                };
                if is_agy_synthetic(text) || text.trim().is_empty() {
                    continue;
                }
                push_bounded(
                    &mut turns,
                    Turn {
                        role: "user".to_string(),
                        text: truncate(text, MAX_TURN_TEXT),
                        tool_calls: Vec::new(),
                    },
                    keep,
                );
            }
            Some("MODEL") => {
                // AGY emits one assistant line per turn (the `type` field is
                // typically `PLANNER_RESPONSE`; we don't gate on it — a
                // renamed type would still be a recognized assistant turn,
                // only the role-by-source gate flags the shape break).
                let text = val
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                let mut tool_calls = extract_agy_tool_calls(val.get("tool_calls"));
                if text.trim().is_empty() && tool_calls.is_empty() {
                    // thinking-only turn — nothing the Coordinator can use.
                    continue;
                }
                cap_tool_calls(&mut tool_calls);
                let turn = Turn {
                    role: "assistant".to_string(),
                    text: truncate(text, MAX_TURN_TEXT),
                    tool_calls,
                };
                if !turn.text.is_empty() {
                    last_assistant_message = Some(turn.text.clone());
                }
                push_bounded(&mut turns, turn, keep);
            }
            // SYSTEM (TASK_NOTIFICATION) and unknown sources are silently
            // skipped — the source gate is the only place we recognize a
            // turn, so a missing `source` on a line that would otherwise be
            // one falls through here without flagging malformed. Real shape
            // breaks (renamed USER_EXPLICIT, etc.) are detected via the
            // explicit guards above.
            _ => {}
        }
    }
    Parsed {
        turns: turns.into(),
        last_assistant_message,
        saw_malformed,
    }
}

/// Synthetic AgY user injections — the AGY equivalent of Claude Code's
/// `<local-command-caveat>` wrapper. Today's transcripts don't carry any
/// of these (issue #1283 research); the predicate is a forward-compat
/// shim so an environment-injected row never masquerades as a real user
/// prompt.
fn is_agy_synthetic(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<local-command-caveat>")
        || t.starts_with("<system>")
        || t.starts_with("<environment_context>")
        || t.starts_with("<task-notification>")
}

/// Pull AgY `tool_calls` (`{name, args}`) into the same [`ToolCall`] wire
/// shape Claude / Codex emit: `{name, input}`. Each `args` object's string
/// leaves are truncated via the shared [`truncate_json_strings`] helper so
/// a `run_command` carrying a multi-megabyte body doesn't blow up the
/// payload while the args *structure* is still delivered raw.
fn extract_agy_tool_calls(value: Option<&serde_json::Value>) -> Vec<ToolCall> {
    let Some(serde_json::Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let name = obj.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let input = obj.get("args").cloned().unwrap_or(serde_json::Value::Null);
            Some(ToolCall {
                name: name.to_string(),
                input: truncate_json_strings(input, MAX_TOOL_STRING),
            })
        })
        .collect()
}