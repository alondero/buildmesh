//! Codex transcript adapter (issue #885, #887).
//!
//! Codex writes `rollout-<timestamp>-<session-id>.jsonl` files under
//! `~/.codex/sessions/YYYY/MM/DD/`. Lines are `{"type": <envelope>,
//! "payload": {...}}` envelopes — `parse_codex_turns` extracts the
//! `message`, `function_call`, and `function_call_output` payload kinds.
//!
//! Issue #1661 step 6: Codex is the **fourth harness migrated end-to-end**.
//! `find_codex_rollout` + the pure walk + `parse_codex_turns` +
//! `is_codex_synthetic` + `codex_concat_text` + `codex_tool_input` all
//! live in this file. The capture poller in `services::codex_session`
//! independently walks the same rollout tree (its `rollout_days_newest_first`);
//! step 6 only collapses the reader's copy — the poller's directory→id
//! mapping is its own concern and stays in `codex_session.rs`.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use crate::env;
use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::{
    cap_tool_calls, merge_into, push_bounded, truncate, truncate_json_strings, Parsed, ToolCall,
    Turn, MAX_TOOL_STRING, MAX_TURN_TEXT,
};

/// Drop-in [`TranscriptAdapter`] for Codex.
pub(crate) struct CodexAdapter;

impl TranscriptAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        // Codex keys sessions globally by id; `node_path` is ignored.
        let _ = ctx.node_path;
        find_codex_rollout(ctx.session_id)
    }

    fn parse(&self, lines: Box<dyn Iterator<Item = String> + '_>, keep: usize) -> Parsed {
        parse_codex_turns(lines, keep)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        matches!(
            value.get("type").and_then(|kind| kind.as_str()),
            Some("response_item") | Some("event_msg")
        ) && value
            .get("payload")
            .is_some_and(|payload| {
                payload.get("type").and_then(|kind| kind.as_str()) == Some("message")
                    && payload.get("role").and_then(|role| role.as_str()) == Some("assistant")
                    && !codex_concat_text(
                        payload.get("content").unwrap_or(&serde_json::Value::Null),
                    )
                    .trim()
                    .is_empty()
            })
    }
}

/// Locate a Codex rollout file `rollout-<timestamp>-<session_id>.jsonl` under
/// `<codex home>/sessions/YYYY/MM/DD/`. Codex cannot relocate its sessions
/// dir per-project (issue #885), so the global one is walked — fixed depth
/// 3, at most a few hundred day dirs, <10ms cold.
fn find_codex_rollout(session_id: &str) -> Option<PathBuf> {
    find_codex_rollout_in(&env::codex_dir().join("sessions"), session_id)
}

/// Pure walk over an explicit sessions root, split from
/// [`find_codex_rollout`] so tests drive it against a temp directory
/// instead of `~/.codex`. Walks newest-first (years, months, days each
/// sorted descending) so the common case — a recent session — terminates
/// after a handful of dirs.
pub(crate) fn find_codex_rollout_in(sessions_dir: &Path, session_id: &str) -> Option<PathBuf> {
    let suffix = format!("-{session_id}.jsonl");
    for year in subdirs_sorted_desc(sessions_dir) {
        for month in subdirs_sorted_desc(&year) {
            for day in subdirs_sorted_desc(&month) {
                let Ok(entries) = fs::read_dir(&day) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    let matches = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(&suffix));
                    if matches {
                        return Some(path);
                    }
                }
            }
        }
    }
    None
}

/// Immediate subdirectories of `dir`, sorted by name descending. Date-named
/// dirs (`2026`, `07`, `18`) sort chronologically, so descending = newest
/// first.
fn subdirs_sorted_desc(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    dirs
}

/// True for Codex's injected context messages (`<user_instructions>` /
/// `<environment_context>` wrappers) — session plumbing, not genuine user
/// turns, mirroring [`super::super::is_synthetic_message`] for Claude Code.
fn is_codex_synthetic(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<user_instructions>") || t.starts_with("<environment_context>")
}

/// Pull the text out of a Codex message `content` array. Codex types its
/// blocks `input_text` (user) / `output_text` (assistant); accept both
/// plus a plain `text` for defensive breadth. Multiple blocks join with
/// newlines.
pub(crate) fn codex_concat_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter(|b| {
                matches!(
                    b.get("type").and_then(|t| t.as_str()),
                    Some("input_text") | Some("output_text") | Some("text")
                )
            })
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A Codex `function_call`'s `arguments` is either a JSON object or a
/// string-encoded JSON blob (the OpenAI wire form). Decode the string
/// form so the Coordinator sees the input's structure, not an escaped
/// blob; a string that isn't valid JSON is delivered as-is.
fn codex_tool_input(arguments: Option<&serde_json::Value>) -> serde_json::Value {
    match arguments {
        Some(serde_json::Value::String(s)) => {
            serde_json::from_str(s).unwrap_or_else(|_| serde_json::Value::String(s.clone()))
        }
        Some(v) => v.clone(),
        None => serde_json::Value::Null,
    }
}

/// Parse Codex rollout JSONL lines into logical turns, honouring the same
/// [`Parsed`] contract as [`super::super::parse_turns`]: rolling
/// `keep`-bounded turn window, whole-stream last-assistant-message
/// tracking, and a malformed flag so a Codex format drift degrades
/// loudly as `ShapeChanged`, never as a quiet `Empty`. Dispatches on
/// the *payload* type rather than the envelope type (`response_item`
/// vs `event_msg`) — Codex has carried `function_call` under both across
/// versions.
pub(crate) fn parse_codex_turns(
    lines: impl Iterator<Item = String>,
    keep: usize,
) -> Parsed {
    let keep = keep.max(1);
    let mut turns: VecDeque<Turn> = VecDeque::new();
    let mut last_assistant_message: Option<String> = None;
    let mut saw_malformed = false;
    // The trailing turn is an assistant turn opened by function_call events;
    // the turn's closing assistant message merges into it.
    let mut assistant_open = false;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let outer = val.get("type").and_then(|t| t.as_str());
        if outer != Some("response_item") && outer != Some("event_msg") {
            // session_meta, turn_context, compaction markers, … — not turns.
            continue;
        }
        let Some(payload) = val.get("payload") else {
            saw_malformed = true;
            continue;
        };
        match payload.get("type").and_then(|t| t.as_str()) {
            Some("message") => {
                let role = payload.get("role").and_then(|r| r.as_str());
                if role != Some("user") && role != Some("assistant") {
                    saw_malformed = true;
                    continue;
                }
                let Some(content) = payload.get("content") else {
                    saw_malformed = true;
                    continue;
                };
                let text = codex_concat_text(content);
                if role == Some("user") {
                    assistant_open = false;
                    if is_codex_synthetic(&text) || text.trim().is_empty() {
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
                } else {
                    if text.trim().is_empty() {
                        assistant_open = false;
                        continue;
                    }
                    if assistant_open {
                        if let Some(last) = turns.back_mut() {
                            merge_into(last, &text, Vec::new());
                            if !last.text.is_empty() {
                                last_assistant_message = Some(last.text.clone());
                            }
                        }
                    } else {
                        let turn = Turn {
                            role: "assistant".to_string(),
                            text: truncate(&text, MAX_TURN_TEXT),
                            tool_calls: Vec::new(),
                        };
                        last_assistant_message = Some(turn.text.clone());
                        push_bounded(&mut turns, turn, keep);
                    }
                    // The assistant's text message closes the turn; later
                    // function_calls belong to the next one.
                    assistant_open = false;
                }
            }
            Some("function_call") => {
                let name = payload
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let call = ToolCall {
                    name,
                    input: truncate_json_strings(codex_tool_input(payload.get("arguments")), MAX_TOOL_STRING),
                };
                if assistant_open {
                    if let Some(last) = turns.back_mut() {
                        last.tool_calls.push(call);
                        cap_tool_calls(&mut last.tool_calls);
                    }
                } else {
                    push_bounded(
                        &mut turns,
                        Turn {
                            role: "assistant".to_string(),
                            text: String::new(),
                            tool_calls: vec![call],
                        },
                        keep,
                    );
                    assistant_open = true;
                }
            }
            // function_call_output (tool results), reasoning, token_count, …
            // — deliberately skipped, like Claude's tool_result echoes.
            _ => {}
        }
    }
    Parsed {
        turns: turns.into(),
        last_assistant_message,
        saw_malformed,
    }
}