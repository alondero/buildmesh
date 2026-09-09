//! Grok Code transcript adapter (issue #1281).
//!
//! Grok writes per-session directories under
//! `~/.grok/sessions/<urlencoded-cwd>/<session-id>/` carrying
//! `chat_history.jsonl` (the per-message conversation log) and
//! `updates.jsonl` (event-level telemetry). The reader uses
//! `chat_history.jsonl`. Per-message JSON: `{"role", "content"}` where
//! `content` may be a string or an array of typed blocks.
//!
//! Issue #1661 step 2: Grok is the **first harness migrated end-to-end**
//! behind the seam. All Grok-specific locator + parser + line-predicate
//! logic lives in this file; the reader module has no per-format arms.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::env;
use crate::agent::session_lifecycle::LifecycleKind;
use crate::services::transcript_reader::adapter::{
    HookClassification, HookDecision, LocateCtx, TranscriptAdapter,
};
use crate::services::transcript_reader::types::{
    cap_tool_calls, push_bounded, truncate, truncate_json_strings, Parsed, ToolCall, Turn,
    MAX_TOOL_STRING, MAX_TURN_TEXT,
};

/// Drop-in [`TranscriptAdapter`] for Grok Code.
pub(crate) struct GrokAdapter;

impl TranscriptAdapter for GrokAdapter {
    fn id(&self) -> &'static str {
        "grok"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        grok_locator_in(
            &env::grok_dir().join("sessions"),
            ctx.session_id,
            ctx.node_path,
        )
    }

    fn parse(&self, lines: Box<dyn Iterator<Item = String> + '_>, keep: usize) -> Parsed {
        parse_grok_turns(lines, keep)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        value.get("role").and_then(|role| role.as_str()) == Some("assistant")
            && value.get("content").is_some_and(|content| match content {
                serde_json::Value::String(text) => !text.trim().is_empty(),
                serde_json::Value::Array(blocks) => blocks
                    .iter()
                    .filter(|block| {
                        block.get("type").and_then(|kind| kind.as_str()) == Some("text")
                    })
                    .filter_map(|block| block.get("text").and_then(|text| text.as_str()))
                    .any(|text| !text.trim().is_empty()),
                _ => false,
            })
    }

    fn classify_hook(
        &self,
        body: &[u8],
        _provider: &str,
    ) -> Option<HookClassification> {
        // Grok posts `hookEventName: "notification"` with a structured
        // `notificationType` (issue #1282): permission_prompt marks
        // input, task_complete marks ready, question-shaped types
        // mark input with QuestionRequested. Other notification types
        // and unrelated events fall through to the shared
        // post-processing.
        let payload: serde_json::Value = serde_json::from_slice(body).ok()?;
        // The HookPayload struct in routes/attention.rs applies serde
        // aliases (`hookEventName`, `notificationType`); we read raw
        // `serde_json::Value` here, so handle both casings explicitly.
        let event = payload
            .get("hook_event_name")
            .or_else(|| payload.get("hookEventName"))
            .and_then(|n| n.as_str())
            .map(str::to_ascii_lowercase);
        if event.as_deref() != Some("notification") {
            return None;
        }
        let nt = payload
            .get("notification_type")
            .or_else(|| payload.get("notificationType"))
            .and_then(|v| v.as_str());
        match nt {
            Some("permission_prompt") => Some(HookClassification {
                decision: HookDecision::MarkInput,
                kind: None,
            }),
            Some("task_complete") => Some(HookClassification {
                decision: HookDecision::Ready,
                kind: None,
            }),
            Some("question") | Some("question_prompt") | Some("ask_user") => {
                Some(HookClassification {
                    decision: HookDecision::MarkInput,
                    kind: Some(LifecycleKind::QuestionRequested),
                })
            }
            _ => None,
        }
    }

    fn verify_attention_token(
        &self,
        query_string: Option<&str>,
        minted: Option<&str>,
    ) -> bool {
        // Issue #1366 round-2 + round-3: Grok's runner cannot bind
        // loopback peer, so the route relies on a minted token
        // attached to the hook command. Default adapters (Claude,
        // Codex, …) accept every callback.
        let Some(minted) = minted else {
            return false;
        };
        // Share the parser from `transcript_reader::types` (not the
        // routes layer) so the services layer doesn't import from
        // http::routes — the seam is meant to decouple, not to bind
        // services back into the routes graph.
        let presented = query_string
            .and_then(|q| crate::services::transcript_reader::types::extract_query_value(q, "token"));
        presented == Some(minted)
    }
}

/// Pure Grok locator. Splits the env lookup out so the layout can be tested
/// against a temp `sessions_root` rather than `~/.grok`. `pub(crate)` so the
/// reader's contract test still drives the resolve without touching the
/// process-global `GROK_HOME` override.
///
/// `sessions_root` is the `~/.grok/sessions` directory (split from the env
/// lookup so the layout can be tested with a temp dir). The result prefers
/// `chat_history.jsonl` (the per-message log, which carries the assistant
/// text the Coordinator reasons over) and falls back to `updates.jsonl`
/// (event-level telemetry is still better than nothing). Neither file
/// present yields `None` (→ `NoTranscript` degrade).
pub(crate) fn grok_locator_in(
    sessions_root: &Path,
    session_id: &str,
    node_path: &str,
) -> Option<PathBuf> {
    let session = sessions_root
        .join(grok_urlencode_cwd(node_path))
        .join(session_id);
    let chat = session.join("chat_history.jsonl");
    if chat.exists() {
        return Some(chat);
    }
    let updates = session.join("updates.jsonl");
    if updates.exists() {
        return Some(updates);
    }
    None
}

/// Percent-encode the harness-cwd path Grok uses as its session-directory
/// segment. Distinct from Claude Code's `encode_path` (which replaces
/// non-alphanumeric with `-`): Grok carries the Windows drive colon and
/// backslashes through as `%3A`/`%5C` etc. so a `C:\Users\…` cwd round-trips
/// deterministically. Reserved characters `-_.~` stay literal (RFC 3986);
/// space becomes `%20`. `pub(crate)` so tests can pin the encoding scheme.
pub(crate) fn grok_urlencode_cwd(node_path: &str) -> String {
    let mut out = String::with_capacity(node_path.len());
    for byte in node_path.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

/// Pull tool calls out of a Grok assistant line's `tool_calls` array. Grok
/// names the input field `args` (not `input` like Claude), and the parser
/// honours the same shared `MAX_TOOL_STRING` truncation so a `Write` carrying
/// a multi-MB body doesn't blow up the payload.
fn extract_grok_tool_calls(value: Option<&serde_json::Value>) -> Vec<ToolCall> {
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
            let input = obj.get("args").cloned().unwrap_or(serde_json::Value::Null);
            Some(ToolCall {
                name,
                input: truncate_json_strings(input, MAX_TOOL_STRING),
            })
        })
        .collect()
}

/// Parse Grok Code JSONL lines into logical turns, honouring the same
/// [`Parsed`] contract as the other parsers: rolling `keep`-bounded turn
/// window, whole-stream last-assistant-message tracking, malformed flag so a
/// renamed-shape line degrades loudly as `ShapeChanged`. Unknown event
/// types (tool echoes, telemetry, status, …) are silently skipped — never
/// flagged.
pub(crate) fn parse_grok_turns(lines: impl Iterator<Item = String>, keep: usize) -> Parsed {
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
        // Grok lines are not gated on an outer `type` discriminator (no
        // Codex-style envelope), so dispatch on the inner `role` field.
        let role = match val.get("role").and_then(|r| r.as_str()) {
            Some("user") => "user",
            Some("assistant") => "assistant",
            // `tool` (tool-result echoes), `system`, plus every unknown event
            // type — silently dropped, never flagged as malformed. This is
            // the "graceful failure on unknown event types" clause of #1281.
            _ => continue,
        };
        let Some(content) = val.get("content") else {
            saw_malformed = true;
            continue;
        };
        let text = match content {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => String::new(),
            // Defensive: if Grok ever switches to a block-array `content`
            // shape, fall back to joining all `text` blocks — same convention
            // as the Claude/Codex parsers.
            serde_json::Value::Array(blocks) => blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => {
                saw_malformed = true;
                continue;
            }
        };
        let mut tool_calls = extract_grok_tool_calls(val.get("tool_calls"));
        // An assistant line with neither text nor tool calls is a no-op
        // (e.g. a heartbeat variant that picked up `role: "assistant"`
        // somehow) — silently skip without flagging malformed.
        if role == "assistant" && text.trim().is_empty() && tool_calls.is_empty() {
            continue;
        }
        if role == "assistant" {
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
        } else {
            push_bounded(
                &mut turns,
                Turn {
                    role: "user".to_string(),
                    text: truncate(&text, MAX_TURN_TEXT),
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
mod tests {
    use super::*;

    #[test]
    fn grok_urlencode_cwd_matches_rfc3986_unreserved_only() {
        // Reserved `-_.~` stay literal; everything else percent-encoded.
        assert_eq!(grok_urlencode_cwd("C:\\Users\\adam"), "C%3A%5CUsers%5Cadam");
        assert_eq!(grok_urlencode_cwd("/home/user"), "%2Fhome%2Fuser");
        assert_eq!(grok_urlencode_cwd("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(grok_urlencode_cwd("with space"), "with%20space");
    }
}