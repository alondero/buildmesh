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
    MAX_TOOL_STRING,
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

    fn parse(
        &self,
        lines: Box<dyn Iterator<Item = String> + '_>,
        keep: usize,
        max_text: usize,
    ) -> Parsed {
        parse_agy_turns_with_text_limit(lines, keep, max_text)
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
#[cfg(test)]
pub(crate) fn parse_agy_turns(lines: impl Iterator<Item = String>, keep: usize) -> Parsed {
    parse_agy_turns_with_text_limit(lines, keep, super::super::types::MAX_TURN_TEXT)
}

pub(crate) fn parse_agy_turns_with_text_limit(
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
                        text: truncate(text, max_text),
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
                let text = val.get("content").and_then(|c| c.as_str()).unwrap_or("");
                let mut tool_calls = extract_agy_tool_calls(val.get("tool_calls"));
                if text.trim().is_empty() && tool_calls.is_empty() {
                    // thinking-only turn — nothing the Coordinator can use.
                    continue;
                }
                cap_tool_calls(&mut tool_calls);
                let turn = Turn {
                    role: "assistant".to_string(),
                    text: truncate(text, max_text),
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

#[cfg(test)]
mod contract_tests {
    use super::*;
    use crate::services::transcript_reader::test_support::fixture;
    use crate::services::transcript_reader::types::empty_or_shape_changed;
    use crate::services::transcript_reader::types::MAX_TURN_TOOL_CALLS;
    use crate::services::transcript_reader::{
        read_last_assistant_message_from_file, read_tail_from_file, TranscriptFormat,
        TranscriptTail, UnavailableReason,
    };

    #[test]
    fn agy_contract_parses_tail_and_last_assistant_message() {
        let tail = read_tail_from_file(
            &fixture("agy", "agy_transcript.jsonl"),
            10,
            TranscriptFormat::Agy,
        );
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = tail
        else {
            panic!("AGY fixture should parse to an available tail, got {tail:?}");
        };
        // Two user prompts + three MODEL turns = five surviving turns
        // (the SYSTEM TASK_NOTIFICATION is dropped). The final MODEL turn
        // carries `status: ERROR` because the harness flagged the search
        // replacement as failed, but the parser still surfaces the line —
        // it's a real assistant reply and the Coordinator needs to see it.
        let roles: Vec<&str> = turns.iter().map(|t| t.role.as_str()).collect();
        assert_eq!(
            roles,
            vec!["user", "assistant", "assistant", "user", "assistant"],
            "turns: {turns:#?}"
        );
        // First user prompt is the genuine opening turn.
        assert_eq!(turns[0].text, "Inspect src/login.ts for the redirect bug.");
        // The first MODEL turn opens with text + a single tool call.
        assert_eq!(turns[1].text, "I'll read the file first.");
        assert_eq!(turns[1].tool_calls.len(), 1);
        assert_eq!(turns[1].tool_calls[0].name, "read_file");
        assert_eq!(
            turns[1].tool_calls[0].input["file_path"], "src/login.ts",
            "AGY's `args` field is mapped onto the shared `input` wire shape"
        );
        // The second MODEL turn has text + a different tool call shape.
        assert_eq!(
            turns[2].text,
            "Found it — the redirect drops the query string. Shall I apply the fix?"
        );
        assert_eq!(turns[2].tool_calls[0].name, "search_replace");
        assert_eq!(turns[2].tool_calls[0].input["file_path"], "src/login.ts");
        // The SYSTEM TASK_NOTIFICATION is silently dropped before the
        // user prompt that follows it.
        assert_eq!(turns[3].text, "Yes, apply the fix.");
        // The closing assistant turn (status=ERROR) still surfaces — the
        // Coordinator needs to see the failure, not have the rich layer
        // degrade silently.
        assert!(turns[4].text.contains("Patch applied"));
        assert_eq!(turns[4].tool_calls[0].name, "edit_file");
        // last_assistant_message is the FULL final text, regardless of
        // the bounded turn window — same contract as the Codex / Claude
        // paths.
        assert_eq!(
            last_assistant_message.as_deref(),
            Some("Patch applied. The redirect now preserves the query string."),
        );
    }

    #[test]
    fn agy_cheap_digest_reader_matches_full_reader() {
        let cheap = read_last_assistant_message_from_file(
            &fixture("agy", "agy_transcript.jsonl"),
            TranscriptFormat::Agy,
        );
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = cheap
        else {
            panic!("expected available, got {cheap:?}");
        };
        assert!(turns.is_empty(), "cheap reader must not return turns");
        assert_eq!(
            last_assistant_message.as_deref(),
            Some("Patch applied. The redirect now preserves the query string."),
        );
    }

    #[test]
    fn agy_notification_only_session_degrades_to_empty() {
        let lines = vec![
            r#"{"source":"SYSTEM","type":"TASK_NOTIFICATION","status":"DONE","content":"<task-notification>\n<task-id>t1</task-id>\n<status>completed</status>\n</task-notification>"}"#.to_string(),
            r#"{"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","content":"<local-command-caveat>noise</local-command-caveat>"}"#.to_string(),
        ];
        let parsed = parse_agy_turns(lines.into_iter(), 10);
        assert!(parsed.turns.is_empty());
        assert!(!parsed.saw_malformed);
        assert_eq!(
            empty_or_shape_changed(parsed.saw_malformed),
            UnavailableReason::Empty
        );
    }

    #[test]
    fn agy_renamed_source_field_degrades_to_shape_changed() {
        // A USER_EXPLICIT-style line whose `content` field was renamed
        // `prompt_text` — every line fails to surface, but the missing-
        // content case is the load-bearing one (a future `source: HUMAN`
        // variant would likewise fail the `Some(source) == USER_EXPLICIT`
        // gate and silently degrade to `Empty`; that's intentional — the
        // *role gate* is the shape pin).
        let lines = vec![
            r#"{"source":"USER_EXPLICIT","prompt_text":"the content was renamed"}"#.to_string(),
        ];
        let parsed = parse_agy_turns(lines.into_iter(), 10);
        assert!(parsed.turns.is_empty());
        assert!(
            parsed.saw_malformed,
            "a USER_EXPLICIT line missing `content` is malformed"
        );
        assert_eq!(
            empty_or_shape_changed(parsed.saw_malformed),
            UnavailableReason::ShapeChanged
        );
    }

    #[test]
    fn agy_thinking_only_turn_is_skipped() {
        let lines = vec![
            r#"{"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","content":"hi","tool_calls":[]}"#.to_string(),
            r#"{"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"","thinking":"just thinking","tool_calls":[]}"#.to_string(),
            r#"{"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"Hello!","thinking":"","tool_calls":[]}"#.to_string(),
        ];
        let parsed = parse_agy_turns(lines.into_iter(), 10);
        let roles: Vec<&str> = parsed.turns.iter().map(|t| t.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "assistant"]);
        assert_eq!(parsed.turns[1].text, "Hello!");
        assert!(!parsed.saw_malformed);
    }

    #[test]
    fn agy_tool_calls_per_turn_are_capped() {
        let mut calls = String::new();
        for i in 0..(MAX_TURN_TOOL_CALLS + 10) {
            if i > 0 {
                calls.push(',');
            }
            calls.push_str(&format!(
                r#"{{"name":"run_command","args":{{"line":"{i}"}}}}"#
            ));
        }
        let line = format!(
            r#"{{"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"running","tool_calls":[{calls}]}}"#
        );
        let parsed = parse_agy_turns(std::iter::once(line), 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(
            parsed.turns[0].tool_calls.len(),
            MAX_TURN_TOOL_CALLS,
            "AGY tool calls per turn are bounded by the shared cap"
        );
    }

    #[test]
    fn agy_locator_prefers_short_transcript_then_falls_back_to_full() {
        let suffix = std::process::id();
        let temp = std::env::temp_dir().join(format!("buildmesh_test_agy_locator_{suffix}"));
        let conv = temp.join("conv-123").join(".system_generated").join("logs");
        std::fs::create_dir_all(&conv).unwrap();

        // Neither file yet → None (callers degrade to NoTranscript).
        let resolved = agy_locator_in(&temp, "conv-123");
        assert!(
            resolved.is_none(),
            "no transcript files yet, locator must report missing (None), got {:?}",
            resolved
        );

        // Only the full file present → it wins (the short variant doesn't
        // exist; the issue's fallback ranks `transcript_full.jsonl` second).
        let full_only = conv.join("transcript_full.jsonl");
        std::fs::write(&full_only, "{}\n").unwrap();
        let resolved = agy_locator_in(&temp, "conv-123");
        assert_eq!(resolved.as_deref(), Some(full_only.as_path()));

        // Both present → short wins (AGY keeps `transcript.jsonl` as the
        // primary and `transcript_full.jsonl` as the untruncated fallback).
        let short = conv.join("transcript.jsonl");
        std::fs::write(&short, "{}\n").unwrap();
        let resolved = agy_locator_in(&temp, "conv-123");
        assert_eq!(resolved.as_deref(), Some(short.as_path()));

        std::fs::remove_dir_all(&temp).ok();
    }
}

#[cfg(test)]
mod file_contract_tests {
    use super::*;
    use crate::services::transcript_reader::test_support::{assert_jsonl_contract, fixture};

    #[test]
    fn reader_handles_tail_digest_malformed_empty_shape_changed_and_unreadable_files() {
        assert_jsonl_contract(
            &AgyAdapter,
            &fixture("agy", "agy_transcript.jsonl"),
            &fixture("agy", "shape_changed.jsonl"),
            "Patch applied. The redirect now preserves the query string.",
        );
    }
}
