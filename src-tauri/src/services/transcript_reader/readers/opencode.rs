use std::collections::VecDeque;
use std::path::PathBuf;

use crate::env;
use crate::models::EnvType;
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

/// OpenCode session IDs start with `ses_` (schema `SessionID`).
fn is_opencode_session_id(id: &str) -> bool {
    id.starts_with("ses_") && id.len() > 4
}

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

/// Resolve a valid session id to its existing env-aware SQLite store.
pub(crate) fn opencode_resolve<'a>(
    session_id: Option<&'a str>,
    node_path: &str,
) -> Result<(PathBuf, &'a str), UnavailableReason> {
    let session_id = session_id
        .filter(|s| !s.is_empty())
        .ok_or(UnavailableReason::NoSession)?;
    if !is_opencode_session_id(session_id) {
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
#[path = "opencode_tests.rs"]
mod opencode_tests;
