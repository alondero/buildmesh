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
use crate::services::transcript_reader::adapter::{
    LocateCtx, TranscriptAdapter,
};
use crate::services::transcript_reader::types::{
    cap_tool_calls, merge_into_with_text_limit, push_bounded, truncate, truncate_json_strings,
    Parsed, ToolCall, Turn, MAX_TOOL_STRING,
};

/// Drop-in [`TranscriptAdapter`] for Claude Code (also the default for any
/// unknown harness id).
pub(crate) struct ClaudeCodeAdapter;

impl TranscriptAdapter for ClaudeCodeAdapter {
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

    fn launch_line(id: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"tool_use_id":"t1","type":"tool_result","content":"Command running in background with ID: {id}. Output is being written to: /tmp/{id}.output. You will be notified when it completes. To check interim output, use Read on that file path.","is_error":false}}]}}}}"#
        )
    }

    fn timeout_launch_line(id: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"tool_use_id":"t2","type":"tool_result","content":"Command did not complete within its 120s timeout and was moved to the background (ID: {id}). Output is being written to: /tmp/{id}.output. You will be notified when it completes. To check interim output, use Read on that file path.","is_error":false}}]}}}}"#
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
            vec![launch_line("byt1iw94s"), timeout_launch_line("b97ep9a8n")].into_iter(),
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
            vec![
                launch_line("aaa"),
                launch_line("bbb"),
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
            vec![launch_line("ccc"), notification_line("ccc", "running")].into_iter(),
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
    fn count_pending_none_on_unreadable_file() {
        // Unknown must never read as "no pending work" — the caller falls back
        // to marking attention.
        assert_eq!(
            count_pending_background_tasks(Path::new("X:\\nope\\missing.jsonl")),
            None
        );
    }

    #[test]
    fn count_pending_reads_real_fixture_shape() {
        let suffix = std::process::id();
        let path =
            std::env::temp_dir().join(format!("buildmesh_test_pending_tasks_{suffix}.jsonl"));
        let body = [
            launch_line("early"),
            notification_line("early", "completed"),
            launch_line("late"),
        ]
        .join("\n");
        std::fs::write(&path, body).unwrap();
        let count = count_pending_background_tasks(&path);
        std::fs::remove_file(&path).ok();
        assert_eq!(count, Some(1), "one launched-but-unnotified task");
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
