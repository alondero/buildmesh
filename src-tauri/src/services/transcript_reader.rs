//! Transcript reader (ADR-0008) "— the deep module that, given an Agent Node's
//! CLI session id and working-directory path, locates and parses Claude Code's
//! on-disk JSONL transcript and returns the **raw recent turns** (assistant
//! text and tool calls) plus the last assistant message "— or a typed
//! [`Unavailable`] reason when the provider has no readable transcript or the
//! file fails to parse.
//!
//! **All Claude-Code-format brittleness is quarantined here.** Both this reader
//! and `services::session_discovery` share the Claude-Code JSONL primitives
//! below (`encode_path`, `is_synthetic_message`, `concat_text_blocks`), so a
//! format change has exactly one place to break "— caught by the contract test
//! over a checked-in fixture (see `mod tests`).
//!
//! Content is **raw and truncated, not summarised** (ADR-0008 Â§4): the
//! Coordinator is itself an LLM, so it reasons over the real material rather
//! than someone else's lossy summary. Truncation only bounds payload size.
//!
//! [`Unavailable`]: UnavailableReason
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use serde::Serialize;
use crate::env;
/// Per-turn text cap. Generous (this is the deep drill-in, not the scan) but
/// bounded so a single huge assistant message can't dominate the payload.
const MAX_TURN_TEXT: usize = 4000;
/// Cap applied to every string leaf inside a tool call's raw `input`, so a
/// `Write` carrying a whole file body doesn't blow up the response while the
/// input's *structure* is still delivered raw.
const MAX_TOOL_STRING: usize = 1000;
/// Default tail length when the caller supplies none.
pub const DEFAULT_TAIL: usize = 20;
/// Hard ceiling on the caller-supplied tail, so a `?tail=100000` can't ask the
/// reader to hold an unbounded transcript in memory.
pub const MAX_TAIL: usize = 200;
// --- Shared Claude-Code JSONL primitives (also used by session_discovery) ---
/// Encode a filesystem path the same way Claude Code does for its
/// `~/.claude/projects/<encoded>` directory names: replace every
/// non-alphanumeric character with `-`. On Windows this collapses the drive
/// colon and `\` separators (and `.` in `.claude`); on Unix it covers `/`.
/// So `X:\src\buildmesh\.claude\worktrees\foo` round-trips to
/// `X--src-buildmesh--claude-worktrees-foo`.
pub(crate) fn encode_path(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}
/// True when raw message text is a synthetic Claude Code injection rather than
/// genuine user input (e.g. the `local-command-caveat` wrapper). Such lines are
/// not real turns and must be skipped.
pub(crate) fn is_synthetic_message(text: &str) -> bool {
    text.trim_start().starts_with("<local-command-caveat>")
}
/// Pull the text out of a message `content` field, which Claude Code writes
/// either as a bare string (user prompts) or as an array of typed blocks
/// (assistant output, tool results). Only `text` blocks contribute; `thinking`,
/// `tool_use`, `tool_result`, `image`, etc. are not text. Multiple text blocks
/// are joined with newlines.
pub(crate) fn concat_text_blocks(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}
/// Truncate to at most `max` *bytes* (the right unit for bounding payload
/// size), appending `…` if cut. Respects UTF-8 boundaries so we never split a
/// multi-byte character "— for non-ASCII text the result is therefore fewer than
/// `max` characters.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}
/// Recursively truncate every string leaf in a JSON value to `max` chars. Keeps
/// the value's *shape* (so the Coordinator sees the real structure of a tool's
/// input) while bounding the bytes any single string contributes.
fn truncate_json_strings(value: serde_json::Value, max: usize) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) => Value::String(truncate(&s, max)),
        Value::Array(arr) => Value::Array(
            arr.into_iter()
                .map(|v| truncate_json_strings(v, max))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, truncate_json_strings(v, max)))
                .collect(),
        ),
        other => other,
    }
}
// --- Wire types ---
/// A single tool invocation the agent made, delivered raw (input structure
/// preserved, individual string leaves truncated to [`MAX_TOOL_STRING`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolCall {
    pub name: String,
    pub input: serde_json::Value,
}
/// One logical transcript turn: a genuine user prompt, or one assistant message
/// (Claude Code splits an assistant message across several JSONL lines that
/// share a `message.id`; the reader coalesces them so a turn is the whole
/// message "— text plus any tool calls it made).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Turn {
    /// `"user"` or `"assistant"`.
    pub role: String,
    /// Concatenated text content, truncated to [`MAX_TURN_TEXT`]. May be empty
    /// for an assistant turn that only made tool calls.
    pub text: String,
    /// Tool calls made in this turn (assistant turns only).
    pub tool_calls: Vec<ToolCall>,
}
impl Turn {
    fn is_assistant(&self) -> bool {
        self.role == "assistant"
    }
}
/// Why a transcript could not be read. Typed so the Coordinator can tell a
/// genuinely-quiet node from a degraded rich layer (ADR-0008 Â§3) "— never a
/// panic, never a silent empty result.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    /// The provider doesn't produce a readable transcript at all (capability
    /// flag off "— e.g. OpenCode, Codex). Distinct from `NoSession` so a
    /// Coordinator can tell "this provider never has a transcript" from "this
    /// supported provider hasn't captured a session yet". The reader never
    /// emits this itself; a route gates on the provider capability and returns
    /// it before reading.
    Unsupported,
    /// The node has no captured CLI session id "— e.g. a supported provider that
    /// never spawned or whose session id wasn't captured yet.
    NoSession,
    /// No transcript file exists at the expected on-disk location.
    NoTranscript,
    /// The transcript file exists but could not be opened or read (I/O error).
    Unreadable,
    /// The file was read but no recognizable turns could be parsed "— the Claude
    /// Code JSONL shape has changed (renamed/missing fields). A busy node must
    /// never look quiet, so this degrades loudly rather than returning `[]`.
    ShapeChanged,
}
/// The reader's result: either an available tail, or a typed unavailable
/// reason. Serializes to a `{"status": "available" | "unavailable", ...}`
/// envelope so it is `curl`-inspectable and shaped for a later MCP wrap.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TranscriptTail {
    Available {
        /// The last N turns, oldest first.
        turns: Vec<Turn>,
        /// The most recent assistant text. When the node is `awaiting_input`
        /// this *is the question it is blocked on* (ADR-0008 Â§4). `None` if no
        /// assistant turn in the tail carried text.
        last_assistant_message: Option<String>,
    },
    Unavailable {
        reason: UnavailableReason,
    },
}
impl TranscriptTail {
    fn unavailable(reason: UnavailableReason) -> Self {
        TranscriptTail::Unavailable { reason }
    }
}
// --- Public entry points ---
/// Locate and read the tail of a node's Claude Code transcript. `session_id` is
/// the node's `cli_session_id`; `node_path` is its working directory (encoded
/// to find the `~/.claude/projects/<encoded>` folder). Returns at most `tail`
/// turns (clamped to [`MAX_TAIL`]; `0` is treated as [`DEFAULT_TAIL`]).
pub fn read_tail(session_id: Option<&str>, node_path: &str, tail: usize) -> TranscriptTail {
    let Some(session_id) = session_id.filter(|s| !s.is_empty()) else {
        return TranscriptTail::unavailable(UnavailableReason::NoSession);
    };
    let path = transcript_path(session_id, node_path);
    if !path.exists() {
        return TranscriptTail::unavailable(UnavailableReason::NoTranscript);
    }
    read_tail_from_file(&path, tail)
}
/// Build the expected on-disk path of a session transcript:
/// `<claude_dir>/projects/<encoded node_path>/<session_id>.jsonl`.
fn transcript_path(session_id: &str, node_path: &str) -> PathBuf {
    env::claude_dir()
        .join("projects")
        .join(encode_path(node_path))
        .join(format!("{session_id}.jsonl"))
}
/// Parse the tail directly from a JSONL file. Split out from [`read_tail`] so
/// the contract test can point it at a checked-in fixture without touching
/// `~/.claude`. Opens the file, parses turns, and returns the last `tail` of
/// them "— or a typed [`UnavailableReason`] on I/O failure or a shape change.
pub fn read_tail_from_file(path: &Path, tail: usize) -> TranscriptTail {
    let Ok(file) = fs::File::open(path) else {
        return TranscriptTail::unavailable(UnavailableReason::Unreadable);
    };
    let reader = BufReader::new(file);
    let lines = reader.lines().map_while(Result::ok);
    tail_from_turns(parse_turns(lines), tail)
}
/// Cheap digest reader (issue #341). Returns only the last assistant message
/// from a Claude Code transcript, bounded to a tail byte window so a single
/// `GET /nodes` over many cwrap nodes with long histories doesn't parse every
/// line in every file. Falls back to a full read if the bounded window
/// contains no assistant text "— rare in practice (the most recent assistant
/// text is by construction near the end of the file) but keeps the reader
/// correct in all cases. Always returns `turns: vec![]` "— the digest consumer
/// only wants `last_assistant_message`, and materialising the full turn list
/// would defeat the optimisation.
pub fn read_last_assistant_message(
    session_id: Option<&str>,
    node_path: &str,
) -> TranscriptTail {
    let Some(session_id) = session_id.filter(|s| !s.is_empty()) else {
        return TranscriptTail::unavailable(UnavailableReason::NoSession);
    };
    let path = transcript_path(session_id, node_path);
    if !path.exists() {
        return TranscriptTail::unavailable(UnavailableReason::NoTranscript);
    }
    read_last_assistant_message_from_file(&path)
}
/// Cheap file-level reader. See [`read_last_assistant_message`].
pub fn read_last_assistant_message_from_file(path: &Path) -> TranscriptTail {
    let Ok(metadata) = fs::metadata(path) else {
        return TranscriptTail::unavailable(UnavailableReason::Unreadable);
    };
    let size = metadata.len();
    // 256 KiB holds several hundred typical JSONL lines, enough to span the
    // last handful of turns for any agent that's been alive for more than a
    // few minutes. Bounded so a 30s Coordinator poll over N cwrap nodes
    // doesn't parse the entire transcript for each one (issue #341).
    const TAIL_BYTES: u64 = 256 * 1024;
    let turns = if size > TAIL_BYTES {
        let mut file = match fs::File::open(path) {
            Ok(f) => f,
            Err(_) => return TranscriptTail::unavailable(UnavailableReason::Unreadable),
        };
        if file.seek(SeekFrom::End(-(TAIL_BYTES as i64))).is_err() {
            return TranscriptTail::unavailable(UnavailableReason::Unreadable);
        }
        let mut buf = Vec::with_capacity(TAIL_BYTES as usize);
        if file.read_to_end(&mut buf).is_err() {
            return TranscriptTail::unavailable(UnavailableReason::Unreadable);
        }
        let mut buf_reader = BufReader::new(buf.as_slice());
        // The seek landed mid-line; drop everything up to and including the
        // first newline so the parser only sees complete JSONL lines.
        let mut discard = Vec::new();
        let _ = buf_reader.read_until(b'\n', &mut discard);
        let lines = buf_reader.lines().map_while(Result::ok);
        let parsed = parse_turns(lines);
        // Defensive fallback: if the bounded window yielded no assistant
        // *text* — the common case is a long window of tool calls — re-parse
        // the whole file so we still surface the actual last assistant
        // message. (Per the contract in `last_assistant_message_is_from_full_transcript_not_window`,
        // a `tail=1` request must not report the blocking question as
        // absent just because the requested window missed it.)
        let have_assistant_text = parsed
            .iter()
            .any(|t| t.is_assistant() && !t.text.is_empty());
        if !have_assistant_text {
            let Ok(file) = fs::File::open(path) else {
                return TranscriptTail::unavailable(UnavailableReason::Unreadable);
            };
            let reader = BufReader::new(file);
            let lines = reader.lines().map_while(Result::ok);
            parse_turns(lines)
        } else {
            parsed
        }
    } else {
        // Small file "— parse the whole thing, no point in seeking.
        let Ok(file) = fs::File::open(path) else {
            return TranscriptTail::unavailable(UnavailableReason::Unreadable);
        };
        let reader = BufReader::new(file);
        let lines = reader.lines().map_while(Result::ok);
        parse_turns(lines)
    };
    if turns.is_empty() {
        return TranscriptTail::unavailable(UnavailableReason::ShapeChanged);
    }
    let last_assistant_message = turns
        .iter()
        .rev()
        .find(|t| t.is_assistant() && !t.text.is_empty())
        .map(|t| t.text.clone());
    TranscriptTail::Available {
        turns: Vec::new(),
        last_assistant_message,
    }
}
/// Effective tail length: `0` …"™ default, otherwise clamp to the ceiling.
fn effective_tail(tail: usize) -> usize {
    if tail == 0 {
        DEFAULT_TAIL
    } else {
        tail.min(MAX_TAIL)
    }
}
/// Turn a parsed turn list into the wire result: keep the last N, derive the
/// last assistant message, or flag `ShapeChanged` if a non-empty file yielded
/// no turns at all.
fn tail_from_turns(turns: Vec<Turn>, tail: usize) -> TranscriptTail {
    if turns.is_empty() {
        // The caller only reaches here for a file that opened; zero turns from
        // it means the recognizable shape is gone (or, rarely, a brand-new
        // empty session). Degrade loudly so a busy node never reads as quiet.
        return TranscriptTail::unavailable(UnavailableReason::ShapeChanged);
    }
    // Derive the last assistant message from the WHOLE transcript, not just the
    // returned window: it is "the last assistant message" (the blocking question
    // when awaiting input) regardless of how small a `tail` the caller asked
    // for, so a `tail=2` request must not report it as absent.
    let last_assistant_message = turns
        .iter()
        .rev()
        .find(|t| t.is_assistant() && !t.text.is_empty())
        .map(|t| t.text.clone());
    let n = effective_tail(tail);
    let start = turns.len().saturating_sub(n);
    let tail_turns = turns[start..].to_vec();
    TranscriptTail::Available {
        turns: tail_turns,
        last_assistant_message,
    }
}
/// Parse JSONL lines into logical turns. Skips every non-message line type
/// (`mode`, `queue-operation`, `file-history-snapshot`, `system`, summaries,
/// …), synthetic injections, and pure tool-result echoes. Consecutive assistant
/// lines sharing a `message.id` are coalesced into one turn (Claude Code splits
/// one assistant message "— thinking / text / tool_use "— across several lines).
fn parse_turns(lines: impl Iterator<Item = String>) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    // Tracks the message.id of the in-progress assistant turn so the next line
    // of the same message merges instead of starting a new turn.
    let mut open_assistant_id: Option<String> = None;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let entry_type = val.get("type").and_then(|t| t.as_str());
        if entry_type != Some("user") && entry_type != Some("assistant") {
            continue;
        }
        let Some(message) = val.get("message") else {
            continue;
        };
        let role = message.get("role").and_then(|r| r.as_str());
        if role != Some("user") && role != Some("assistant") {
            continue;
        }
        let role = role.unwrap();
        let content = message.get("content");
        let text = concat_text_blocks(content);
        let tool_calls = extract_tool_calls(content);
        // Skip synthetic user injections (local-command-caveat) and pure
        // tool-result echoes (a user line carrying only tool output).
        if role == "user" {
            if is_synthetic_message(&text) {
                open_assistant_id = None;
                continue;
            }
            if text.trim().is_empty() {
                // No real text and (user lines never carry tool_use) …"™ echo.
                open_assistant_id = None;
                continue;
            }
        }
        // An assistant line with neither text nor tool calls (e.g. a lone
        // `thinking` block) carries nothing the Coordinator can use.
        if role == "assistant" && text.trim().is_empty() && tool_calls.is_empty() {
            continue;
        }
        if role == "assistant" {
            let id = message
                .get("id")
                .and_then(|i| i.as_str())
                .map(|s| s.to_string());
            // Coalesce with the open assistant turn iff the ids match and are
            // non-empty; otherwise this starts a fresh turn.
            if let (Some(id), Some(open)) = (&id, &open_assistant_id) {
                if id == open {
                    if let Some(last) = turns.last_mut() {
                        merge_into(last, &text, tool_calls);
                        continue;
                    }
                }
            }
            open_assistant_id = id;
            turns.push(Turn {
                role: "assistant".to_string(),
                text: truncate(&text, MAX_TURN_TEXT),
                tool_calls,
            });
        } else {
            open_assistant_id = None;
            turns.push(Turn {
                role: "user".to_string(),
                text: truncate(&text, MAX_TURN_TEXT),
                tool_calls: Vec::new(),
            });
        }
    }
    turns
}
/// Merge a continuation line of the same assistant message into the open turn:
/// append any text (re-truncating the combined result) and add its tool calls.
fn merge_into(turn: &mut Turn, more_text: &str, mut more_tools: Vec<ToolCall>) {
    if !more_text.trim().is_empty() {
        let combined = if turn.text.is_empty() {
            more_text.to_string()
        } else {
            format!("{}\n{}", turn.text, more_text)
        };
        turn.text = truncate(&combined, MAX_TURN_TEXT);
    }
    turn.tool_calls.append(&mut more_tools);
}
/// Pull `tool_use` blocks out of a message `content` array into [`ToolCall`]s,
/// truncating string leaves in each raw `input`.
fn extract_tool_calls(content: Option<&serde_json::Value>) -> Vec<ToolCall> {
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
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }
    // --- Shared primitive tests ---
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
    fn truncate_json_strings_bounds_leaves_but_keeps_shape() {
        let big = "x".repeat(MAX_TOOL_STRING + 50);
        let v = serde_json::json!({"file_path": "/a", "content": big, "n": 7});
        let out = truncate_json_strings(v, MAX_TOOL_STRING);
        assert_eq!(out["file_path"], "/a");
        assert_eq!(out["n"], 7);
        // Truncated leaf gains the ellipsis and is bounded.
        let content = out["content"].as_str().unwrap();
        assert!(content.ends_with('…'));
        assert!(content.chars().count() <= MAX_TOOL_STRING + 1);
    }
    // --- read_tail (locator) ---
    #[test]
    fn missing_session_id_is_no_session() {
        assert_eq!(
            read_tail(None, "X:\\src\\buildmesh", 10),
            TranscriptTail::unavailable(UnavailableReason::NoSession)
        );
        assert_eq!(
            read_tail(Some(""), "X:\\src\\buildmesh", 10),
            TranscriptTail::unavailable(UnavailableReason::NoSession)
        );
    }
    #[test]
    fn missing_file_is_no_transcript() {
        // A session id that cannot resolve to a real file on disk degrades to
        // NoTranscript rather than erroring.
        let result = read_tail(
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
        let result = read_tail_from_file(Path::new("X:\\nope\\missing.jsonl"), 10);
        assert_eq!(
            result,
            TranscriptTail::unavailable(UnavailableReason::Unreadable)
        );
    }
    // --- read_last_assistant_message (issue #341 cheap digest reader) ---
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
        let path = std::env::temp_dir()
            .join(format!("buildmesh_test_long_transcript_{suffix}_{rounds}.jsonl"));
        std::fs::write(&path, &body).unwrap();
        path
    }
    #[test]
    fn read_last_assistant_message_matches_full_reader_on_long_transcript() {
        let path = write_long_transcript(2_000);
        let full = read_tail_from_file(&path, 1);
        let cheap = read_last_assistant_message_from_file(&path);
        std::fs::remove_file(&path).ok();
        let full_last = match full {
            TranscriptTail::Available { last_assistant_message, .. } => last_assistant_message,
            other => panic!("full read should be available, got {other:?}"),
        };
        let cheap_last = match cheap {
            TranscriptTail::Available { last_assistant_message, turns } => {
                assert!(turns.is_empty(), "cheap reader must not return turns");
                last_assistant_message
            }
            other => panic!("cheap read should be available, got {other:?}"),
        };
        assert_eq!(cheap_last, full_last);
        assert_eq!(cheap_last.as_deref(), Some("The blocking question: shall I proceed?"));
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
        let cheap = read_last_assistant_message_from_file(&path);
        std::fs::remove_file(&path).ok();
        let cheap_last = match cheap {
            TranscriptTail::Available { last_assistant_message, .. } => last_assistant_message,
            other => panic!("cheap read should be available, got {other:?}"),
        };
        assert_eq!(
            cheap_last.as_deref(),
            Some("The blocking question: shall I proceed?"),
            "fallback must re-parse the whole file when the bounded window has no assistant text"
        );
    }
    // --- Contract test over a checked-in real-shape fixture ---
    #[test]
    fn contract_parses_tail_and_last_assistant_message() {
        let tail = read_tail_from_file(&fixture("claude_code_transcript.jsonl"), 10);
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
        assert_eq!(turns[4].text, "Found it — the redirect drops the query string. Shall I apply the fix?");
        assert_eq!(
            last_assistant_message.as_deref(),
            Some("Found it — the redirect drops the query string. Shall I apply the fix?")
        );
    }
    #[test]
    fn tail_length_limits_returned_turns() {
        let two = read_tail_from_file(&fixture("claude_code_transcript.jsonl"), 2);
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
        // how small a tail the caller asks for: a window that ends on a user
        // turn must still surface it.
        let turns = vec![
            Turn { role: "assistant".to_string(), text: "Shall I apply the fix?".to_string(), tool_calls: vec![] },
            Turn { role: "user".to_string(), text: "wait, first explain".to_string(), tool_calls: vec![] },
        ];
        let TranscriptTail::Available { turns: window, last_assistant_message } =
            tail_from_turns(turns, 1)
        else {
            panic!("expected available");
        };
        // Window is just the trailing user turn …
        assert_eq!(window.len(), 1);
        assert_eq!(window[0].role, "user");
        // … but the last assistant message is still recovered from the full list.
        assert_eq!(last_assistant_message.as_deref(), Some("Shall I apply the fix?"));
    }
    // --- Brittleness defence: renamed/missing fields …"™ Unavailable, no panic ---
    #[test]
    fn shape_changed_fixture_degrades_not_panics() {
        let tail = read_tail_from_file(&fixture("claude_code_transcript_shape_changed.jsonl"), 10);
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::ShapeChanged),
            "a renamed/missing-field transcript must degrade loudly, never panic"
        );
    }
    // --- Serialization shape (the wire contract a later MCP wrap depends on) ---
    #[test]
    fn available_serializes_with_status_envelope() {
        let tail = TranscriptTail::Available {
            turns: vec![Turn {
                role: "assistant".to_string(),
                text: "hi".to_string(),
                tool_calls: vec![ToolCall {
                    name: "Read".to_string(),
                    input: serde_json::json!({"file_path": "a"}),
                }],
            }],
            last_assistant_message: Some("hi".to_string()),
        };
        let json: serde_json::Value = serde_json::to_value(&tail).unwrap();
        assert_eq!(json["status"], "available");
        assert_eq!(json["turns"][0]["role"], "assistant");
        assert_eq!(json["turns"][0]["tool_calls"][0]["name"], "Read");
        assert_eq!(json["last_assistant_message"], "hi");
    }
    #[test]
    fn unavailable_serializes_reason_in_snake_case() {
        let json: serde_json::Value =
            serde_json::to_value(TranscriptTail::unavailable(UnavailableReason::NoTranscript))
                .unwrap();
        assert_eq!(json["status"], "unavailable");
        assert_eq!(json["reason"], "no_transcript");
    }
}