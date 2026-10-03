//! Grok Code transcript adapter (issue #1281).
//!
//! Grok writes per-session directories under
//! `~/.grok/sessions/<urlencoded-cwd>/<session-id>/` carrying
//! `chat_history.jsonl` (the per-message conversation log) and
//! `updates.jsonl` (event-level telemetry). The reader uses
//! `chat_history.jsonl`. Native message JSON: `{"type", "content"}` (legacy: `role`) where
//! `content` may be a string or an array of typed blocks.
//!
//! Issue #1661 step 2: Grok is the **first harness migrated end-to-end**
//! behind the seam. All Grok-specific locator + parser + line-predicate
//! logic lives in this file; the reader module has no per-format arms.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::agent::session_lifecycle::LifecycleKind;
use crate::env;
use crate::services::transcript_reader::adapter::{
    HookClassification, HookDecision, LocateCtx, TranscriptAdapter,
};
use crate::services::transcript_reader::types::{
    cap_tool_calls, push_bounded, truncate, truncate_json_strings, Parsed, ToolCall, Turn,
    MAX_TOOL_STRING,
};

/// Drop-in [`TranscriptAdapter`] for Grok Code.
pub(crate) struct GrokAdapter;

impl TranscriptAdapter for GrokAdapter {
    fn id(&self) -> &'static str {
        "grok"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        grok_locator_in(
            &env::cli_dir_for_spawn(env::grok_dir(), ".grok", ctx.node_path)?.join("sessions"),
            ctx.session_id,
            ctx.node_path,
        )
    }

    fn parse(
        &self,
        lines: Box<dyn Iterator<Item = String> + '_>,
        keep: usize,
        max_text: usize,
    ) -> Parsed {
        parse_grok_turns_with_text_limit(lines, keep, max_text)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        message_role(&value) == Some("assistant")
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

    fn classify_hook_value(
        &self,
        payload: &serde_json::Value,
        _provider: &str,
    ) -> Option<HookClassification> {
        // Grok posts `hookEventName: "notification"` with a structured
        // `notificationType` (issue #1282): permission_prompt marks
        // input, task_complete marks ready, question-shaped types
        // mark input with QuestionRequested. Other notification types
        // and unrelated events fall through to the shared
        // post-processing.
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

    fn verify_attention_token(&self, query_string: Option<&str>, minted: Option<&str>) -> bool {
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
        let presented = query_string.and_then(|q| {
            crate::services::transcript_reader::types::extract_query_value(q, "token")
        });
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
    let find = |cwd: &str| {
        let session = sessions_root.join(grok_urlencode_cwd(cwd)).join(session_id);
        ["chat_history.jsonl", "updates.jsonl"]
            .into_iter()
            .map(|name| session.join(name))
            .find(|path| path.is_file())
    };
    find(node_path).or_else(|| {
        // Windows resolves mixed separators and trims trailing separators
        // before Grok encodes its cwd. Keep exact lookup first and never
        // rewrite POSIX guest paths. Avoid a duplicate probe for an already
        // canonical Windows path.
        (env::is_windows_path(node_path) && (node_path.contains('/') || node_path.ends_with('\\')))
            .then(|| find(node_path.replace('/', "\\").trim_end_matches('\\')))
            .flatten()
    })
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

// Both parsing and revision tracking must agree on which records are reports.
fn message_role(value: &serde_json::Value) -> Option<&str> {
    value
        .get("type")
        .or_else(|| value.get("role"))
        .and_then(|role| role.as_str())
}

/// Pull tool calls out of a Grok assistant line's `tool_calls` array. Grok
/// encodes native `arguments` as JSON text (legacy: `args`), and the parser
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
            let input = obj
                .get("arguments")
                .or_else(|| obj.get("args"))
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

/// Parse Grok Code JSONL lines into logical turns, honouring the same
/// [`Parsed`] contract as the other parsers: rolling `keep`-bounded turn
/// window, whole-stream last-assistant-message tracking, malformed flag so a
/// renamed-shape line degrades loudly as `ShapeChanged`. Unknown event
/// types (tool echoes, telemetry, status, …) are silently skipped — never
/// flagged.
#[cfg(test)]
pub(crate) fn parse_grok_turns(lines: impl Iterator<Item = String>, keep: usize) -> Parsed {
    parse_grok_turns_with_text_limit(lines, keep, super::super::types::MAX_TURN_TEXT)
}

pub(crate) fn parse_grok_turns_with_text_limit(
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
        let role = match message_role(&val) {
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
mod tests {
    use super::*;

    #[test]
    fn native_tool_calls_decode_arguments_without_exposing_reasoning() {
        let parsed = parse_grok_turns([
            r#"{"type":"reasoning","content":"private"}"#.to_string(),
            r#"{"type":"assistant","content":"Reading the file.","tool_calls":[{"name":"read_file","arguments":"{\"path\":\"src/lib.rs\"}"}]}"#.to_string(),
            r#"{"type":"tool_result","content":"file contents"}"#.to_string(),
        ].into_iter(), 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].tool_calls[0].name, "read_file");
        assert_eq!(
            parsed.turns[0].tool_calls[0].input,
            serde_json::json!({"path": "src/lib.rs"})
        );
        assert_eq!(
            parsed.last_assistant_message.as_deref(),
            Some("Reading the file.")
        );
    }

    #[test]
    fn grok_urlencode_cwd_matches_rfc3986_unreserved_only() {
        // Reserved `-_.~` stay literal; everything else percent-encoded.
        assert_eq!(grok_urlencode_cwd("C:\\Users\\adam"), "C%3A%5CUsers%5Cadam");
        assert_eq!(grok_urlencode_cwd("/home/user"), "%2Fhome%2Fuser");
        assert_eq!(grok_urlencode_cwd("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(grok_urlencode_cwd("with space"), "with%20space");
    }
}

#[cfg(test)]
mod contract_tests {
    use crate::services::transcript_reader::test_support::fixture;
    use crate::services::transcript_reader::types::empty_or_shape_changed;
    use crate::services::transcript_reader::types::MAX_TOOL_STRING;
    use crate::services::transcript_reader::{
        assistant_report_from_file, read_last_assistant_message_from_file, read_tail_from_file,
        TranscriptFormat, TranscriptTail, UnavailableReason,
    };

    #[test]
    fn grok_contract_parses_tail_and_last_assistant_message() {
        let tail = read_tail_from_file(
            &fixture("grok", "grok_chat_history.jsonl"),
            10,
            TranscriptFormat::Grok,
        );
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = tail
        else {
            panic!("fixture should parse to an available tail, got {tail:?}");
        };
        let roles: Vec<&str> = turns.iter().map(|t| t.role.as_str()).collect();
        assert_eq!(
            roles,
            vec!["user", "assistant", "assistant", "user", "assistant"],
            "tool_result echo and command_status must be skipped; turns: {turns:#?}"
        );
        assert_eq!(turns[0].text, "Fix the login redirect bug");
        // First assistant turn carries a tool call (Read).
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
    fn grok_cheap_digest_reader_matches_full_reader() {
        let cheap = read_last_assistant_message_from_file(
            &fixture("grok", "grok_chat_history.jsonl"),
            TranscriptFormat::Grok,
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
            Some("Found it — the redirect drops the query string. Shall I apply the fix?")
        );
    }

    #[test]
    fn grok_unknown_event_types_are_silently_skipped() {
        let lines = vec![
            r#"{"role":"command_status","status":"completed"}"#.to_string(),
            r#"{"role":"telemetry","latency_ms":42}"#.to_string(),
            r#"{"role":"user","content":"real prompt"}"#.to_string(),
            r#"{"role":"assistant","content":"real reply"}"#.to_string(),
            r#"{"role":"heartbeat","seq":7}"#.to_string(),
        ];
        let parsed = crate::services::transcript_reader::readers::grok::parse_grok_turns(
            lines.into_iter(),
            10,
        );
        assert_eq!(parsed.turns.len(), 2);
        assert!(
            !parsed.saw_malformed,
            "unknown event types must not flag malformed"
        );
        assert_eq!(parsed.last_assistant_message.as_deref(), Some("real reply"));
    }

    #[test]
    fn grok_renamed_role_field_degrades_to_shape_changed() {
        let lines = vec![
            // Recognized role + content shape we don't understand (a nested
            // object instead of string/array/null) — this IS a structural
            // break in the Grok format, so it must degrade loudly.
            r#"{"role":"assistant","content":{"unexpected":"object"}}"#.to_string(),
            r#"{"role":"assistant","content":42}"#.to_string(),
        ];
        let parsed = crate::services::transcript_reader::readers::grok::parse_grok_turns(
            lines.into_iter(),
            10,
        );
        assert!(parsed.turns.is_empty());
        assert!(
            parsed.saw_malformed,
            "recognized role with wrong content type is malformed"
        );
        assert_eq!(
            empty_or_shape_changed(parsed.saw_malformed),
            UnavailableReason::ShapeChanged
        );
    }

    #[test]
    fn grok_missing_role_field_is_skipped_not_flagged() {
        let lines = vec![
            r#"{"author":"assistant","blocks":[{"type":"text","text":"renamed"}]}"#.to_string(),
            r#"{"type":"command_status","status":"running"}"#.to_string(),
            r#"{"latency_ms":42,"transport":"stream"}"#.to_string(),
        ];
        let parsed = crate::services::transcript_reader::readers::grok::parse_grok_turns(
            lines.into_iter(),
            10,
        );
        assert!(
            parsed.turns.is_empty(),
            "no recognized-role lines yields no turns"
        );
        assert!(
            !parsed.saw_malformed,
            "unknown event types must NOT flag malformed"
        );
        assert_eq!(
            empty_or_shape_changed(parsed.saw_malformed),
            UnavailableReason::Empty
        );
    }

    #[test]
    fn grok_only_unknown_events_degrade_to_empty() {
        let tail = read_tail_from_file(
            &fixture("grok", "grok_chat_history_empty.jsonl"),
            10,
            TranscriptFormat::Grok,
        );
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::Empty),
            "a turn-less file of unknown event types is Empty, not ShapeChanged"
        );
    }

    #[test]
    fn grok_rolling_buffer_retains_only_the_last_keep_turns() {
        let mut lines = Vec::new();
        for i in 0..50 {
            lines.push(format!(r#"{{"role":"user","content":"prompt {i}"}}"#));
            lines.push(format!(r#"{{"role":"assistant","content":"reply {i}"}}"#));
        }
        let parsed = crate::services::transcript_reader::readers::grok::parse_grok_turns(
            lines.into_iter(),
            3,
        );
        assert_eq!(parsed.turns.len(), 3, "buffer never exceeds keep");
        assert_eq!(parsed.turns[2].text, "reply 49");
        assert_eq!(
            parsed.last_assistant_message.as_deref(),
            Some("reply 49"),
            "last assistant message survives eviction"
        );
    }

    #[test]
    fn grok_tool_call_args_are_truncated_through_truncate_json_strings() {
        let big = "x".repeat(MAX_TOOL_STRING + 50);
        let lines = vec![format!(
            r#"{{"role":"assistant","content":"with a big tool call","tool_calls":[{{"name":"Read","args":{{"file_path":"a","content":"{big}"}}}}]}}"#
        )];
        let parsed = crate::services::transcript_reader::readers::grok::parse_grok_turns(
            lines.into_iter(),
            10,
        );
        assert_eq!(parsed.turns.len(), 1);
        let call = &parsed.turns[0].tool_calls[0];
        assert_eq!(call.name, "Read");
        let content = call.input["content"].as_str().unwrap();
        assert!(content.ends_with('…'), "large args body must be truncated");
    }

    #[test]
    fn grok_native_transcript_recovers_circuit_report() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp
            .path()
            .join("F%3A%5Csrc%5Crepo%5C.claude%5Cworktrees%5Ctask")
            .join("session-99");
        std::fs::create_dir_all(&session).unwrap();
        let file = session.join("chat_history.jsonl");
        std::fs::write(
            &file,
            concat!(
                "{\"type\":\"user\",\"content\":\"Fix the module\"}\n",
                "{\"type\":\"reasoning\",\"content\":\"Private reasoning\"}\n",
                "{\"type\":\"assistant\",\"content\":\"Module fixed. Tests passed.\"}\n",
                "{\"type\":\"tool_result\",\"content\":\"tool output\"}\n",
            ),
        )
        .unwrap();
        let path = crate::services::transcript_reader::readers::grok::grok_locator_in(
            temp.path(),
            "session-99",
            r"F:\src\repo/.claude/worktrees/task",
        )
        .unwrap();
        let report = assistant_report_from_file(&path, TranscriptFormat::Grok)
            .expect("completed Grok turn must be readable by the circuit");
        assert_eq!(report.text, "Module fixed. Tests passed.");
        use std::io::Write;
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap();
        writeln!(writer, "{{\"type\":\"user\",\"content\":\"Next task\"}}").unwrap();
        writeln!(
            writer,
            "{{\"type\":\"tool_result\",\"content\":\"More tool output\"}}"
        )
        .unwrap();
        assert_eq!(
            assistant_report_from_file(&path, TranscriptFormat::Grok)
                .unwrap()
                .revision,
            report.revision
        );
        writeln!(
            writer,
            "{{\"type\":\"assistant\",\"content\":\"Module fixed. Tests passed.\"}}"
        )
        .unwrap();
        assert_ne!(
            assistant_report_from_file(&path, TranscriptFormat::Grok)
                .unwrap()
                .revision,
            report.revision
        );
    }

    #[test]
    fn grok_locator_recovers_native_windows_cwd_from_mixed_separators() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp
            .path()
            .join("F%3A%5Csrc%5Crepo%5C.claude%5Cworktrees%5Ctask")
            .join("session-99");
        std::fs::create_dir_all(&session).unwrap();
        let file = session.join("chat_history.jsonl");
        std::fs::write(&file, "{}\n").unwrap();
        assert_eq!(
            crate::services::transcript_reader::readers::grok::grok_locator_in(
                temp.path(),
                "session-99",
                r"F:\src\repo/.claude/worktrees/task/"
            ),
            Some(file)
        );
    }

    #[test]
    fn grok_locator_prefers_chat_history_over_updates() {
        let suffix = std::process::id();
        let temp =
            std::env::temp_dir().join(format!("buildmesh_test_grok_locator_prefer_{suffix}"));
        let session = temp.join("session-abc");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("chat_history.jsonl"), "{}").unwrap();
        std::fs::write(session.join("updates.jsonl"), "{}").unwrap();
        let found = crate::services::transcript_reader::readers::grok::grok_locator_in(
            &temp,
            "session-abc",
            "",
        );
        assert_eq!(
            found.as_deref(),
            Some(session.join("chat_history.jsonl").as_path()),
            "chat_history.jsonl wins when both exist"
        );
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn grok_locator_falls_back_to_updates_when_chat_history_missing() {
        let suffix = std::process::id();
        let temp =
            std::env::temp_dir().join(format!("buildmesh_test_grok_locator_fallback_{suffix}"));
        let session = temp.join("session-abc");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("updates.jsonl"), "{}").unwrap();
        let found = crate::services::transcript_reader::readers::grok::grok_locator_in(
            &temp,
            "session-abc",
            "",
        );
        assert_eq!(
            found.as_deref(),
            Some(session.join("updates.jsonl").as_path()),
            "updates.jsonl fallback when chat_history.jsonl missing"
        );
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn grok_locator_returns_none_when_both_files_missing() {
        let suffix = std::process::id();
        let temp = std::env::temp_dir().join(format!("buildmesh_test_grok_locator_none_{suffix}"));
        let session = temp.join("session-abc");
        std::fs::create_dir_all(&session).unwrap();
        assert!(
            crate::services::transcript_reader::readers::grok::grok_locator_in(
                &temp,
                "session-abc",
                ""
            )
            .is_none()
        );
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn grok_urlencode_cwd_test_pin() {
        // RFC 3986 unreserved set: ALPHA / DIGIT / "-" / "." / "_" / "~".
        // Everything else becomes %XX, uppercase hex (the form Grok emits).
        assert_eq!(
            crate::services::transcript_reader::readers::grok::grok_urlencode_cwd(
                r"C:\Users\adam\src\buildmesh"
            ),
            "C%3A%5CUsers%5Cadam%5Csrc%5Cbuildmesh",
            "Windows drive colon and backslashes must be percent-encoded so the \
             session-directory segment is filesystem-safe"
        );
        assert_eq!(
            crate::services::transcript_reader::readers::grok::grok_urlencode_cwd(
                "/home/adam/src/buildmesh"
            ),
            "%2Fhome%2Fadam%2Fsrc%2Fbuildmesh",
            "POSIX slashes also percent-encoded"
        );
        // Unreserved per RFC 3986 stays literal; the locator only encodes
        // non-unreserved bytes.
        assert_eq!(
            crate::services::transcript_reader::readers::grok::grok_urlencode_cwd(
                "project-with_under.dots~tildas"
            ),
            "project-with_under.dots~tildas",
            "RFC 3986 unreserved chars pass through unchanged"
        );
        assert_eq!(
            crate::services::transcript_reader::readers::grok::grok_urlencode_cwd(""),
            ""
        );
        assert_eq!(
            crate::services::transcript_reader::readers::grok::grok_urlencode_cwd("with space"),
            "with%20space",
            "space encodes to %20, not '+' (RFC 3986, not form-style)"
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
            &GrokAdapter,
            &fixture("grok", "grok_chat_history.jsonl"),
            &fixture("grok", "shape_changed.jsonl"),
            "Found it — the redirect drops the query string. Shall I apply the fix?",
        );
    }
}
