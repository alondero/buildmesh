//! Command Code transcript adapter (issues #1407, #1500).
//!
//! Command Code writes structured per-session JSONL under
//! `~/.commandcode/projects/<encoded-cwd>/<session-id>.jsonl` — one
//! `{type: "message", message: {role, content}}` event per line.
//!
//! Issue #1661 step 3: Command Code is the **second harness migrated
//! end-to-end**. The reader's `commandcode_sessions_dir` path composition +
//! `commandcode_transcript_path_in` + `find_commandcode_transcript` wrapper
//! + `commandcode_message_activity` (used by `commandcode_watcher.rs`)
//! + `parse_commandcode_turns` all live in this file. The capture poller
//! in `services::commandcode_session` already delegates to
//! `commandcode_sessions_dir`, so the seam-inversion is one import rewrite
//! in step 10.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::env;
use crate::models::EnvType;
use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::{
    cap_tool_calls, merge_into, push_bounded, truncate, Parsed, Turn, MAX_TURN_TEXT,
};
// Command Code's wire shape reuses Claude Code's content
// primitives (same `tool_use` blocks, same `local-command-caveat`
// synthetic wrappers). Reach the shared primitives from
// `transcript_paths` — the legacy indirection through mod.rs ended
// with step 5 of #1661.
use crate::services::transcript_paths::{concat_text_blocks, is_synthetic_message};
use super::claude_code::extract_tool_calls as extract_claude_tool_calls;

/// Drop-in [`TranscriptAdapter`] for Command Code.
pub(crate) struct CommandCodeAdapter;

impl TranscriptAdapter for CommandCodeAdapter {
    fn id(&self) -> &'static str {
        "commandcode"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        let env_type = EnvType::from(env::env_for_path(Path::new(ctx.node_path)));
        let sessions_dir = commandcode_sessions_dir(env_type, ctx.node_path)?;
        let path = commandcode_transcript_path_in(&sessions_dir, ctx.session_id);
        path.exists().then_some(path)
    }

    fn parse(&self, lines: Box<dyn Iterator<Item = String> + '_>, keep: usize) -> Parsed {
        parse_commandcode_turns(lines, keep)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        value.get("type").and_then(|kind| kind.as_str()) == Some("message")
            && value.get("message").is_some_and(|message| {
                message.get("role").and_then(|role| role.as_str()) == Some("assistant")
                    && !concat_text_blocks(message.get("content"))
                        .trim()
                        .is_empty()
            })
    }
}

/// Encode a filesystem path the way Command Code does for its
/// `~/.commandcode/projects/<slug>` directory names: lowercase, replace every
/// non-alphanumeric character with `-`, collapse consecutive `-` runs, and
/// trim leading/trailing `-` (issue #1500).
///
/// For example `F:\src\buildmesh\.claude\worktrees\foo` becomes
/// `f-src-buildmesh-claude-worktrees-foo`, and `/home/user/project` becomes
/// `home-user-project`. This matches the on-disk layout observed in Command
/// Code v1.43.0 and the `c-users-user` / `home-...` slugs reported upstream.
/// Pass the CLI cwd form; for raw paths that may be WSL UNC, normalize with
/// `env::normalize_unc_to_wsl` first.
///
/// Kept `pub(crate)` because `services::commandcode_session::find_*` and
/// `agent_node_discovery` (until step 10 of #1661) both need this; it
/// moves with the adapter.
pub(crate) fn commandcode_project_slug(path: &str) -> String {
    let mut slug = String::with_capacity(path.len());
    let mut last_was_dash = false;
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

/// The host-accessible Command Code session directory for an agent
/// environment: `<home>/.commandcode/projects/<encoded-cwd>/` (issue #1500).
/// Mirrors [`super::super::transcript_path`] (Claude Code): the transcript-
/// format module composes the slug, while `env` owns the host-accessible
/// projects base (including WSL translation). `spawn_path` is the CLI cwd
/// form; raw WSL UNC paths are normalized first via `env::normalize_unc_to_wsl`
/// so the in-WSL CLI's home-user-repo layout resolves to the same slug.
pub(crate) fn commandcode_sessions_dir(
    env_type: EnvType,
    spawn_path: &str,
) -> Option<PathBuf> {
    let normalized = env::normalize_unc_to_wsl(spawn_path);
    let projects = env::commandcode_projects_dir(env_type, &normalized)?;
    let slug = commandcode_project_slug(&normalized);
    if slug.is_empty() {
        return None;
    }
    Some(projects.join(slug))
}

/// Pure Command Code locator used by the contract test and kept separate
/// from process-global home/environment discovery.
pub(crate) fn commandcode_transcript_path_in(
    sessions_root: &Path,
    session_id: &str,
) -> PathBuf {
    sessions_root.join(format!("{session_id}.jsonl"))
}

/// Classify a Command Code message payload using the canonical text and
/// tool-extraction rules. Empty, synthetic, tool-result, thinking, and
/// reasoning records are classified separately so the watcher can clear
/// pending tool calls, while the digest parser still omits them from
/// normalized turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandCodeMessageActivity {
    UserTurn,
    ToolUse,
    ToolResult,
    AssistantResponse,
}

/// Classify a Command Code message payload.
pub(crate) fn commandcode_message_activity(
    message: &serde_json::Value,
) -> Option<CommandCodeMessageActivity> {
    let role = message.get("role")?.as_str()?;
    let content = message.get("content")?;
    let text = concat_text_blocks(Some(content));
    let tool_calls = extract_claude_tool_calls(Some(content));

    match role {
        "user" if contains_tool_result(content) => Some(CommandCodeMessageActivity::ToolResult),
        "user" if is_synthetic_message(&text) || text.trim().is_empty() => None,
        "user" => Some(CommandCodeMessageActivity::UserTurn),
        "assistant" if !tool_calls.is_empty() => Some(CommandCodeMessageActivity::ToolUse),
        "assistant" if !text.trim().is_empty() => {
            Some(CommandCodeMessageActivity::AssistantResponse)
        }
        _ => None,
    }
}

fn contains_tool_result(content: &serde_json::Value) -> bool {
    content.as_array().is_some_and(|blocks| {
        blocks
            .iter()
            .any(|block| block.get("type").and_then(|kind| kind.as_str()) == Some("tool_result"))
    })
}

/// Parse Command Code JSONL lines into normalized turns. The rolling
/// buffer, assistant digest, malformed-shape signal, and assistant-id
/// coalescing all follow the shared transcript-reader contract used by
/// Claude Code/Cursor.
pub(crate) fn parse_commandcode_turns(
    lines: impl Iterator<Item = String>,
    keep: usize,
) -> Parsed {
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

        // Metadata and future event kinds are deliberately ignored. A
        // recognized message envelope with a broken payload is different:
        // it should degrade to ShapeChanged when no usable turns remain.
        if value.get("type").and_then(|kind| kind.as_str()) != Some("message") {
            continue;
        }
        let Some(message) = value.get("message") else {
            saw_malformed = true;
            continue;
        };

        let Some(role) = message.get("role").and_then(|role| role.as_str()) else {
            saw_malformed = true;
            continue;
        };
        if role != "user" && role != "assistant" {
            saw_malformed = true;
            continue;
        }
        let Some(raw_content) = message.get("content") else {
            saw_malformed = true;
            continue;
        };
        if !matches!(
            raw_content,
            serde_json::Value::String(_) | serde_json::Value::Array(_) | serde_json::Value::Null
        ) {
            saw_malformed = true;
            continue;
        }
        let text = concat_text_blocks(Some(raw_content));
        let mut tool_calls = extract_claude_tool_calls(Some(raw_content));

        if role == "user" {
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

        // An assistant message containing only internal blocks has no
        // user-facing normalized content. Keep the open id so a split
        // assistant message can still merge a following continuation.
        if text.trim().is_empty() && tool_calls.is_empty() {
            continue;
        }

        let id = value
            .get("id")
            .or_else(|| message.get("id"))
            .and_then(|id| id.as_str())
            .map(str::to_string);
        if let (Some(id), Some(open)) = (&id, &open_assistant_id) {
            if id == open {
                if let Some(last) = turns.back_mut() {
                    merge_into(last, &text, tool_calls);
                    if !last.text.trim().is_empty() {
                        last_assistant_message = Some(last.text.clone());
                    }
                    continue;
                }
            }
        }

        open_assistant_id = id;
        cap_tool_calls(&mut tool_calls);
        let turn = Turn {
            role: "assistant".to_string(),
            text: truncate(&text, MAX_TURN_TEXT),
            tool_calls,
        };
        if !turn.text.trim().is_empty() {
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