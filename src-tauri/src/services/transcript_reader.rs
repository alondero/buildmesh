//! Transcript reader (ADR-0008) "— the deep module that, given an Agent Node's
//! CLI session id and working-directory path, locates and parses the harness's
//! on-disk JSONL transcript and returns the **raw recent turns** (assistant
//! text and tool calls) plus the last assistant message "— or a typed
//! [`Unavailable`] reason when the provider has no readable transcript or the
//! file fails to parse.
//!
//! Two harness formats are supported, selected by [`TranscriptFormat`]:
//! Claude Code's `~/.claude/projects/<encoded-cwd>/<session>.jsonl` and
//! Codex's `~/.codex/sessions/YYYY/MM/DD/rollout-*-<session>.jsonl` (issue
//! #885). Both map onto the same [`Turn`]/[`ToolCall`] wire shape, so the
//! Coordinator never learns which harness wrote the file.
//!
//! **All transcript-format brittleness is quarantined here.** Both this reader
//! and `services::session_discovery` share the Claude-Code JSONL primitives
//! below (`encode_path`, `is_synthetic_message`, `concat_text_blocks`), so a
//! format change has exactly one place to break "— caught by the contract tests
//! over checked-in fixtures (see `mod tests`).
//!
//! Content is **raw and truncated, not summarised** (ADR-0008 Â§4): the
//! Coordinator is itself an LLM, so it reasons over the real material rather
//! than someone else's lossy summary. Truncation only bounds payload size.
//!
//! [`Unavailable`]: UnavailableReason
use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use serde::Serialize;
use crate::env;
/// Per-turn text cap. Generous (this is the deep drill-in, not the scan) but
/// bounded so a single huge assistant message can't dominate the payload.
const MAX_TURN_TEXT: usize = 4000;
/// Per-turn tool-call cap. Within one `message.id` the count is naturally small
/// (parallel calls usually span separate message ids → separate turns), so this
/// is defensive only — but it honours the same "no single turn dominates the
/// payload" intent as [`MAX_TURN_TEXT`] (issue #335). Generous so a real turn is
/// never clipped; a turn that hits it was already pathological.
const MAX_TURN_TOOL_CALLS: usize = 50;
/// Cap applied to every string leaf inside a tool call's raw `input`, so a
/// `Write` carrying a whole file body doesn't blow up the response while the
/// input's *structure* is still delivered raw.
const MAX_TOOL_STRING: usize = 1000;
/// Default tail length when the caller supplies none.
pub const DEFAULT_TAIL: usize = 20;
/// Hard ceiling on the caller-supplied tail, so a `?tail=100000` can't ask the
/// reader to hold an unbounded transcript in memory.
pub const MAX_TAIL: usize = 200;

/// Which harness's on-disk JSONL shape a transcript uses. Selected once at the
/// enrichment boundary (from the node's resolved harness adapter id) and passed
/// down, so the reader itself never consults provider state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFormat {
    ClaudeCode,
    Codex,
}

impl TranscriptFormat {
    /// Map a resolved harness adapter id to its transcript format. Every
    /// Claude-Code-backed executor (the `anthropic` adapter behind the built-in
    /// subscription and all custom MiniMax/DeepSeek profiles) shares the
    /// Claude Code format; only Codex writes its own rollout format. Kimi Code
    /// (wayfinder #918) writes standard JSONL (`~/.kimi/sessions/wire.jsonl`)
    /// but the path resolver isn't wired yet — tracked as a follow-up.
    pub fn for_harness(harness_id: &str) -> Self {
        if harness_id == "codex" {
            TranscriptFormat::Codex
        } else {
            TranscriptFormat::ClaudeCode
        }
    }
}
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
/// Pull the text of only the **first** `text` block out of a message `content`
/// field (or the whole string for a bare-string content). Unlike
/// [`concat_text_blocks`] this never joins multiple blocks: `session_discovery`
/// wants a single-line session *title* from the opening prompt, and joining all
/// blocks with `\n` (which its `strip_tags` doesn't collapse) would corrupt the
/// title for a multi-text-block user message (issue #335). For the common
/// single-block message the two functions are identical.
pub(crate) fn first_text_block(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .find_map(|b| b.get("text").and_then(|t| t.as_str()))
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}
/// Truncate to at most `max` *bytes* (the right unit for bounding payload
/// size), appending `…` if cut. Respects UTF-8 boundaries so we never split a
/// multi-byte character "— for non-ASCII text the result is therefore fewer than
/// `max` characters. `pub(crate)` so the sibling Claude-Code JSONL consumers
/// (`agent_node_discovery`, formerly `session_discovery`) share one truncation
/// rule "— divergence here is how the two copies of this fn used to drift
/// silently (issue #340).
pub(crate) fn truncate(s: &str, max: usize) -> String {
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
/// Why a transcript could not be read. Typed so the Coordinator can tell a
/// genuinely-quiet node from a degraded rich layer (ADR-0008 Â§3) "— never a
/// panic, never a silent empty result.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    /// The provider doesn't produce a readable transcript at all (capability
    /// flag off "— e.g. OpenCode, Agy, Terminal). Distinct from `NoSession` so a
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
    /// The file was read and its lines were *structurally well-formed*, but it
    /// carried no recognizable turns yet — a genuinely quiet/new session whose
    /// only lines are deliberately-skipped ones (synthetic `local-command-caveat`
    /// injections, tool-result echoes, thinking-only assistant lines) plus
    /// non-message lines (`mode`/`system`/summary). Distinct from `ShapeChanged`
    /// so a Coordinator can tell "nothing has happened yet" from "the rich layer
    /// is broken, page me" (issue #335). Low-probability in practice (a spawned
    /// node's first user prompt is itself a turn) but the two are now distinct.
    Empty,
    /// The file was read but a structurally-malformed `user`/`assistant` line was
    /// seen (renamed/missing `message`/`role`/`content`) and no recognizable
    /// turns could be parsed "— the Claude Code JSONL shape has changed. A busy
    /// node must never look quiet, so this degrades loudly rather than returning
    /// `[]` or the quieter `Empty`.
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
/// Locate and read the tail of a node's transcript. `session_id` is the node's
/// `cli_session_id`; `node_path` is its working directory (used by the Claude
/// Code format to find the `~/.claude/projects/<encoded>` folder; Codex keys
/// sessions globally by id, so it ignores it). Returns at most `tail` turns
/// (clamped to [`MAX_TAIL`]; `0` is treated as [`DEFAULT_TAIL`]).
pub fn read_tail(
    format: TranscriptFormat,
    session_id: Option<&str>,
    node_path: &str,
    tail: usize,
) -> TranscriptTail {
    let Some(session_id) = session_id.filter(|s| !s.is_empty()) else {
        return TranscriptTail::unavailable(UnavailableReason::NoSession);
    };
    let Some(path) = locate_transcript(format, session_id, node_path) else {
        return TranscriptTail::unavailable(UnavailableReason::NoTranscript);
    };
    if !path.exists() {
        return TranscriptTail::unavailable(UnavailableReason::NoTranscript);
    }
    read_tail_from_file(&path, tail, format)
}
/// Resolve the on-disk transcript file for a session in the given format.
/// `None` means "no file exists" (only the Codex walk can conclude that
/// before an `exists()` check).
fn locate_transcript(
    format: TranscriptFormat,
    session_id: &str,
    node_path: &str,
) -> Option<PathBuf> {
    match format {
        TranscriptFormat::ClaudeCode => Some(transcript_path(session_id, node_path)),
        TranscriptFormat::Codex => find_codex_rollout(session_id),
    }
}
/// Build the expected on-disk path of a Claude Code session transcript:
/// `<claude_dir>/projects/<encoded node_path>/<session_id>.jsonl`.
fn transcript_path(session_id: &str, node_path: &str) -> PathBuf {
    env::claude_dir()
        .join("projects")
        .join(encode_path(node_path))
        .join(format!("{session_id}.jsonl"))
}
/// Locate a Codex rollout file `rollout-<timestamp>-<session_id>.jsonl` under
/// `<codex home>/sessions/YYYY/MM/DD/`. Codex cannot relocate its sessions dir
/// per-project (issue #885), so the global one is walked — fixed depth 3,
/// at most a few hundred day dirs, <10ms cold.
fn find_codex_rollout(session_id: &str) -> Option<PathBuf> {
    find_codex_rollout_in(&env::codex_dir().join("sessions"), session_id)
}
/// Pure walk over an explicit sessions root, split from [`find_codex_rollout`]
/// so tests drive it against a temp directory instead of `~/.codex`. Walks
/// newest-first (years, months, days each sorted descending) so the common
/// case — a recent session — terminates after a handful of dirs.
fn find_codex_rollout_in(sessions_dir: &Path, session_id: &str) -> Option<PathBuf> {
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
/// dirs (`2026`, `07`, `18`) sort chronologically, so descending = newest first.
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
/// Parse the tail directly from a JSONL file. Split out from [`read_tail`] so
/// the contract test can point it at a checked-in fixture without touching
/// `~/.claude`. Opens the file, parses turns, and returns the last `tail` of
/// them "— or a typed [`UnavailableReason`] on I/O failure or a shape change.
pub fn read_tail_from_file(path: &Path, tail: usize, format: TranscriptFormat) -> TranscriptTail {
    let Ok(file) = fs::File::open(path) else {
        return TranscriptTail::unavailable(UnavailableReason::Unreadable);
    };
    let reader = BufReader::new(file);
    let lines = reader.lines().map_while(Result::ok);
    // Stream the whole file but retain only the last N turns (issue #335): the
    // on-demand drill-in still scans every line, yet holds O(tail) turns in
    // memory instead of the whole transcript, so a busy long-running node can't
    // make the endpoint allocate a Vec of every turn it ever produced.
    build_tail(parse_transcript(format, lines, effective_tail(tail)))
}
/// Dispatch JSONL lines to the parser for the given harness format. Both
/// parsers share the [`Parsed`] contract (bounded turn window, whole-stream
/// last-assistant tracking, malformed-line flag).
fn parse_transcript(
    format: TranscriptFormat,
    lines: impl Iterator<Item = String>,
    keep: usize,
) -> Parsed {
    match format {
        TranscriptFormat::ClaudeCode => parse_turns(lines, keep),
        TranscriptFormat::Codex => parse_codex_turns(lines, keep),
    }
}
/// Cheap digest reader (issue #341). Returns only the last assistant message
/// from a Claude Code transcript, bounded to a tail byte window so a single
/// `GET /nodes` over many Claude Code nodes with long histories doesn't parse every
/// line in every file. Falls back to a full read if the bounded window
/// contains no assistant text "— rare in practice (the most recent assistant
/// text is by construction near the end of the file) but keeps the reader
/// correct in all cases. Always returns `turns: vec![]` "— the digest consumer
/// only wants `last_assistant_message`, and materialising the full turn list
/// would defeat the optimisation.
pub fn read_last_assistant_message(
    format: TranscriptFormat,
    session_id: Option<&str>,
    node_path: &str,
) -> TranscriptTail {
    let Some(session_id) = session_id.filter(|s| !s.is_empty()) else {
        return TranscriptTail::unavailable(UnavailableReason::NoSession);
    };
    let Some(path) = locate_transcript(format, session_id, node_path) else {
        return TranscriptTail::unavailable(UnavailableReason::NoTranscript);
    };
    if !path.exists() {
        return TranscriptTail::unavailable(UnavailableReason::NoTranscript);
    }
    read_last_assistant_message_from_file(&path, format)
}
/// Cheap file-level reader. See [`read_last_assistant_message`].
pub fn read_last_assistant_message_from_file(
    path: &Path,
    format: TranscriptFormat,
) -> TranscriptTail {
    let Ok(metadata) = fs::metadata(path) else {
        return TranscriptTail::unavailable(UnavailableReason::Unreadable);
    };
    let size = metadata.len();
    // 256 KiB holds several hundred typical JSONL lines, enough to span the
    // last handful of turns for any agent that's been alive for more than a
    // few minutes. Bounded so a 30s Coordinator poll over N Claude Code nodes
    // doesn't parse the entire transcript for each one (issue #341).
    const TAIL_BYTES: u64 = 256 * 1024;
    // The digest only needs the last assistant message, so keep just one turn
    // (enough for a split assistant message to coalesce its text) — the rolling
    // buffer never grows with transcript length.
    let parsed = if size > TAIL_BYTES {
        let Some(window) = parse_byte_window(path, TAIL_BYTES, format) else {
            return TranscriptTail::unavailable(UnavailableReason::Unreadable);
        };
        // Defensive fallback: if the bounded window carried no assistant *text*
        // — the common case is a long window of tool calls — re-parse the whole
        // file so we still surface the actual last assistant message. (Per the
        // contract in `last_assistant_message_is_from_full_transcript_not_window`,
        // a `tail=1` request must not report the blocking question as absent
        // just because the requested window missed it.)
        if window.last_assistant_message.is_none() {
            let Ok(file) = fs::File::open(path) else {
                return TranscriptTail::unavailable(UnavailableReason::Unreadable);
            };
            let reader = BufReader::new(file);
            parse_transcript(format, reader.lines().map_while(Result::ok), 1)
        } else {
            window
        }
    } else {
        // Small file "— parse the whole thing, no point in seeking.
        let Ok(file) = fs::File::open(path) else {
            return TranscriptTail::unavailable(UnavailableReason::Unreadable);
        };
        let reader = BufReader::new(file);
        parse_transcript(format, reader.lines().map_while(Result::ok), 1)
    };
    if parsed.turns.is_empty() {
        return TranscriptTail::unavailable(empty_or_shape_changed(parsed.saw_malformed));
    }
    TranscriptTail::Available {
        turns: Vec::new(),
        last_assistant_message: parsed.last_assistant_message,
    }
}
/// Read and parse only the last `tail_bytes` of a transcript (keeping one turn,
/// for the cheap digest reader). Seeks to the byte window, drops the partial
/// first line the seek landed mid-way through, and parses the remainder. Returns
/// `None` on any I/O failure so the caller can degrade to `Unreadable`.
fn parse_byte_window(path: &Path, tail_bytes: u64, format: TranscriptFormat) -> Option<Parsed> {
    let mut file = fs::File::open(path).ok()?;
    file.seek(SeekFrom::End(-(tail_bytes as i64))).ok()?;
    let mut buf = Vec::with_capacity(tail_bytes as usize);
    file.read_to_end(&mut buf).ok()?;
    let mut buf_reader = BufReader::new(buf.as_slice());
    // The seek landed mid-line; drop everything up to and including the first
    // newline so the parser only sees complete JSONL lines.
    let mut discard = Vec::new();
    let _ = buf_reader.read_until(b'\n', &mut discard);
    Some(parse_transcript(format, buf_reader.lines().map_while(Result::ok), 1))
}
/// Effective tail length: `0` …"™ default, otherwise clamp to the ceiling.
fn effective_tail(tail: usize) -> usize {
    if tail == 0 {
        DEFAULT_TAIL
    } else {
        tail.min(MAX_TAIL)
    }
}
/// The outcome of parsing JSONL lines into turns. Carries the *bounded* tail of
/// turns (the last `keep`, so memory is O(keep) even for a many-MB transcript —
/// issue #335), the last assistant message text seen across the **whole** stream
/// (not just the retained window, so a small tail still surfaces the blocking
/// question), and whether any structurally-malformed `user`/`assistant` line was
/// seen (issue #335: lets [`empty_or_shape_changed`] tell a broken Claude Code
/// shape from a genuinely-quiet session that simply has no turns yet).
struct Parsed {
    turns: Vec<Turn>,
    last_assistant_message: Option<String>,
    saw_malformed: bool,
}
/// Build the wire result from a [`Parsed`]: an `Available` tail, or — for a file
/// that yielded no turns — the typed empty-vs-shape-changed degrade.
fn build_tail(parsed: Parsed) -> TranscriptTail {
    if parsed.turns.is_empty() {
        return TranscriptTail::unavailable(empty_or_shape_changed(parsed.saw_malformed));
    }
    TranscriptTail::Available {
        turns: parsed.turns,
        last_assistant_message: parsed.last_assistant_message,
    }
}
/// The degrade reason for a file that opened but yielded no turns: `ShapeChanged`
/// (loud) when a malformed message line proves the recognizable shape is gone,
/// else `Empty` (quiet) for a genuinely-new/quiet session (issue #335).
fn empty_or_shape_changed(saw_malformed: bool) -> UnavailableReason {
    if saw_malformed {
        UnavailableReason::ShapeChanged
    } else {
        UnavailableReason::Empty
    }
}
/// Parse JSONL lines into logical turns, retaining only the last `keep` of them
/// in a rolling buffer (issue #335: bounds held memory regardless of transcript
/// size). Skips every non-message line type (`mode`, `queue-operation`,
/// `file-history-snapshot`, `system`, summaries, …), synthetic injections, and
/// pure tool-result echoes. Consecutive assistant lines sharing a `message.id`
/// are coalesced into one turn (Claude Code splits one assistant message "—
/// thinking / text / tool_use "— across several lines).
fn parse_turns(lines: impl Iterator<Item = String>, keep: usize) -> Parsed {
    // Always retain at least the open turn so a split assistant message can
    // still coalesce its continuation lines (the open turn is never evicted —
    // eviction only drops the front).
    let keep = keep.max(1);
    let mut turns: VecDeque<Turn> = VecDeque::new();
    let mut last_assistant_message: Option<String> = None;
    let mut saw_malformed = false;
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
        // From here the line claims to be a user/assistant message. A missing or
        // renamed `message`/`role`/`content` is a structural break in the Claude
        // Code shape (issue #335) — flag it so an all-broken file degrades loudly
        // as `ShapeChanged`, while a file whose only non-turn lines were
        // *deliberately* skipped (synthetic/echo/thinking) degrades as `Empty`.
        let Some(message) = val.get("message") else {
            saw_malformed = true;
            continue;
        };
        let role = message.get("role").and_then(|r| r.as_str());
        if role != Some("user") && role != Some("assistant") {
            saw_malformed = true;
            continue;
        }
        let role = role.unwrap();
        let Some(content) = message.get("content") else {
            saw_malformed = true;
            continue;
        };
        let content = Some(content);
        let text = concat_text_blocks(content);
        let mut tool_calls = extract_tool_calls(content);
        // Skip synthetic user injections (local-command-caveat) and pure
        // tool-result echoes (a user line carrying only tool output). These are
        // well-formed lines we choose not to surface — never `ShapeChanged`.
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
                    if let Some(last) = turns.back_mut() {
                        merge_into(last, &text, tool_calls);
                        if !last.text.is_empty() {
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
            if !turn.text.is_empty() {
                last_assistant_message = Some(turn.text.clone());
            }
            push_bounded(&mut turns, turn, keep);
        } else {
            open_assistant_id = None;
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
/// Push a turn into the rolling buffer, evicting the oldest if it now exceeds
/// `keep`. Only the front is dropped, so the most-recent (open) turn always
/// survives for a continuation line to coalesce into.
fn push_bounded(turns: &mut VecDeque<Turn>, turn: Turn, keep: usize) {
    turns.push_back(turn);
    while turns.len() > keep {
        turns.pop_front();
    }
}
/// Bound a turn's tool-call count to [`MAX_TURN_TOOL_CALLS`] (issue #335), so no
/// single turn dominates the payload even if a message carries a pathological
/// number of parallel tool calls.
fn cap_tool_calls(tool_calls: &mut Vec<ToolCall>) {
    tool_calls.truncate(MAX_TURN_TOOL_CALLS);
}
/// Merge a continuation line of the same assistant message into the open turn:
/// append any text (re-truncating the combined result) and add its tool calls
/// (re-capping the combined list so a turn split across many lines still honours
/// [`MAX_TURN_TOOL_CALLS`]).
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
    cap_tool_calls(&mut turn.tool_calls);
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
// --- Codex rollout parser (issue #885 / #887) ---
//
// Codex writes `rollout-<timestamp>-<session-id>.jsonl` files whose lines are
// `{"type": <envelope>, "payload": {...}}` envelopes. The payloads this reader
// cares about:
//
//   message              — {"type":"message","role":"user"|"assistant",
//                           "content":[{"type":"input_text"|"output_text","text":…}]}
//   function_call        — {"type":"function_call","name":…,"arguments":…,"call_id":…}
//   function_call_output — the tool's result; skipped, like Claude tool_result echoes.
//
// Within a turn Codex emits function_calls first and the assistant's text
// message last, so the parser opens an assistant turn on the first
// function_call and closes it when the assistant message (or a user message)
// arrives — mapping onto the same "one turn = text + its tool calls" shape the
// Claude parser produces.

/// True for Codex's injected context messages (`<user_instructions>` /
/// `<environment_context>` wrappers) — session plumbing, not genuine user
/// turns, mirroring [`is_synthetic_message`] for Claude Code.
fn is_codex_synthetic(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<user_instructions>") || t.starts_with("<environment_context>")
}

/// Pull the text out of a Codex message `content` array. Codex types its
/// blocks `input_text` (user) / `output_text` (assistant); accept both plus a
/// plain `text` for defensive breadth. Multiple blocks join with newlines.
fn codex_concat_text(content: &serde_json::Value) -> String {
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
/// string-encoded JSON blob (the OpenAI wire form). Decode the string form so
/// the Coordinator sees the input's structure, not an escaped blob; a string
/// that isn't valid JSON is delivered as-is.
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
/// [`Parsed`] contract as [`parse_turns`]: rolling `keep`-bounded turn window,
/// whole-stream last-assistant-message tracking, and a malformed flag so a
/// Codex format drift degrades loudly as `ShapeChanged`, never as a quiet
/// `Empty`. Dispatches on the *payload* type rather than the envelope type
/// (`response_item` vs `event_msg`) — Codex has carried `function_call`
/// under both across versions.
fn parse_codex_turns(lines: impl Iterator<Item = String>, keep: usize) -> Parsed {
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
                    input: truncate_json_strings(
                        codex_tool_input(payload.get("arguments")),
                        MAX_TOOL_STRING,
                    ),
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
            // function_call_output (tool results), reasoning, token_count, … —
            // deliberately skipped, like Claude's tool_result echoes.
            _ => {}
        }
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
// `run_in_background` Bash call, or a foreground command that outlives its
// timeout and is moved to the background) and auto-resumes itself when the
// task's `<task-notification>` arrives. A Stop hook that fires with such work
// still pending is NOT "the user is needed". The transcript records both ends
// deterministically:
//
//   launch  — a `tool_result` whose text says "…background… (ID: xyz) …
//             You will be notified when it completes."
//   finish  — a line (queue-operation, or the queued_command attachment that
//             re-invokes the agent) carrying `<task-id>xyz</task-id>`.
//
// Pending = launched minus notified.

/// Matches the task id in either launch phrasing:
/// `Command running in background with ID: xyz.` and
/// `…was moved to the background (ID: xyz).`
static LAUNCH_ID: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"\bID: ([A-Za-z0-9_-]+)").unwrap());
/// A task-notification's id paired with its status, non-greedy so several
/// notifications on one line pair correctly. Real transcripts carry
/// `<status>running</status>` notifications too (e.g. a foreground command
/// moved to the background) — only a terminal status means the wait is over.
static NOTIFIED_ID: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(r"<task-id>([A-Za-z0-9_-]+)</task-id>.*?<status>([a-z_]+)</status>").unwrap()
});
/// The phrase that makes a `tool_result` a background-task launch. Both known
/// launch phrasings carry it; matching the promise (rather than the two exact
/// sentences) keeps the scan stable across minor wording changes.
const LAUNCH_MARKER: &str = "You will be notified when it completes";

/// Count background tasks launched but not yet notified in a Claude Code
/// transcript. `None` = the file could not be read — the caller must treat
/// that as "unknown" and fall back to its pre-#878 behaviour, never as "no
/// pending work".
pub fn count_pending_background_tasks(path: &Path) -> Option<usize> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    Some(pending_background_task_ids(reader.lines().map_while(Result::ok)).len())
}

/// Pure scan over JSONL lines: launched-task ids with no matching
/// `<task-id>` notification, in launch order. Split from the I/O wrapper so
/// tests drive it with inline fixtures.
fn pending_background_task_ids(lines: impl Iterator<Item = String>) -> Vec<String> {
    let mut launched: Vec<String> = Vec::new();
    let mut notified: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in lines {
        // The notification marker is matched on the raw line: it appears in
        // `queue-operation` lines and in the queued_command attachment that
        // re-invokes the agent, and caring which one carries it would couple
        // us to more of the shape than we need.
        for cap in NOTIFIED_ID.captures_iter(&line) {
            if &cap[2] != "running" {
                notified.insert(cap[1].to_string());
            }
        }
        // Launches only count inside a tool_result block — free text merely
        // *mentioning* the promise (e.g. an agent quoting these docs) must not
        // register a phantom task.
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
            read_tail(TranscriptFormat::ClaudeCode, None, "X:\\src\\buildmesh", 10),
            TranscriptTail::unavailable(UnavailableReason::NoSession)
        );
        assert_eq!(
            read_tail(TranscriptFormat::ClaudeCode, Some(""), "X:\\src\\buildmesh", 10),
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
        let result = read_tail_from_file(Path::new("X:\\nope\\missing.jsonl"), 10, TranscriptFormat::ClaudeCode);
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
        let full = read_tail_from_file(&path, 1, TranscriptFormat::ClaudeCode);
        let cheap = read_last_assistant_message_from_file(&path, TranscriptFormat::ClaudeCode);
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
        let cheap = read_last_assistant_message_from_file(&path, TranscriptFormat::ClaudeCode);
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
        let tail = read_tail_from_file(&fixture("claude_code_transcript.jsonl"), 10, TranscriptFormat::ClaudeCode);
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
        let two = read_tail_from_file(&fixture("claude_code_transcript.jsonl"), 2, TranscriptFormat::ClaudeCode);
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
        assert_eq!(parsed.last_assistant_message.as_deref(), Some("Shall I apply the fix?"));
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
    // --- Brittleness defence: renamed/missing fields …"™ Unavailable, no panic ---
    #[test]
    fn shape_changed_fixture_degrades_not_panics() {
        let tail = read_tail_from_file(&fixture("claude_code_transcript_shape_changed.jsonl"), 10, TranscriptFormat::ClaudeCode);
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::ShapeChanged),
            "a renamed/missing-field transcript must degrade loudly, never panic"
        );
    }

    // --- Item 1: empty/quiet session is Empty, not ShapeChanged (issue #335) ---
    #[test]
    fn empty_session_degrades_to_empty_not_shape_changed() {
        // A file whose only lines are deliberately-skipped ones (caveat, a
        // tool-result echo, a thinking-only assistant) plus non-message lines
        // (summary/mode/system) is a genuinely-quiet session — `Empty`, the
        // quiet degrade, not the loud `ShapeChanged`.
        let tail = read_tail_from_file(&fixture("claude_code_transcript_empty.jsonl"), 10, TranscriptFormat::ClaudeCode);
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::Empty),
            "a structurally well-formed but turn-less session is Empty, not ShapeChanged"
        );
    }

    #[test]
    fn empty_session_is_empty_through_the_digest_reader_too() {
        // The cheap digest path must make the same empty-vs-shape distinction.
        let tail =
            read_last_assistant_message_from_file(&fixture("claude_code_transcript_empty.jsonl"), TranscriptFormat::ClaudeCode);
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
        assert!(parsed.saw_malformed, "a renamed role/content line is malformed");
        assert_eq!(
            empty_or_shape_changed(parsed.saw_malformed),
            UnavailableReason::ShapeChanged
        );
    }

    // --- Item 3: per-turn tool-call cap (issue #335) ---
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
        let line =
            format!(r#"{{"type":"assistant","message":{{"id":"m1","role":"assistant","content":[{calls}]}}}}"#);
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
            format!(r#"{{"type":"assistant","message":{{"id":"m1","role":"assistant","content":[{calls}]}}}}"#)
        };
        let parsed = parse_turns(vec![mk(0), mk(1000)].into_iter(), 10);
        assert_eq!(parsed.turns.len(), 1, "same message.id coalesces to one turn");
        assert_eq!(parsed.turns[0].tool_calls.len(), MAX_TURN_TOOL_CALLS);
    }

    // --- Item 4: first_text_block is first-block, not join-all (issue #335) ---
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
    // --- Pending background tasks (issue #878) ---

    /// A launch line in the primary phrasing (`run_in_background: true`).
    fn launch_line(id: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"tool_use_id":"t1","type":"tool_result","content":"Command running in background with ID: {id}. Output is being written to: /tmp/{id}.output. You will be notified when it completes. To check interim output, use Read on that file path.","is_error":false}}]}}}}"#
        )
    }

    /// A launch line in the timeout phrasing (foreground command moved to the
    /// background after its timeout).
    fn timeout_launch_line(id: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"tool_use_id":"t2","type":"tool_result","content":"Command did not complete within its 120s timeout and was moved to the background (ID: {id}). Output is being written to: /tmp/{id}.output. You will be notified when it completes. To check interim output, use Read on that file path.","is_error":false}}]}}}}"#
        )
    }

    /// A queue-operation completion notification, the shape that re-invokes
    /// the agent when the task finishes.
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
        assert!(pending.is_empty(), "terminal notifications end the wait, got {pending:?}");
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
        let path = std::env::temp_dir()
            .join(format!("buildmesh_test_pending_tasks_{suffix}.jsonl"));
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

    // --- Codex rollout format (issues #885 / #887) ---

    /// Contract test over the checked-in Codex rollout fixture: noise lines
    /// (session_meta, turn_context, user_instructions / environment_context
    /// injections, function_call_output echoes, token_count) are all dropped;
    /// function_calls attach to the assistant turn they belong to; and the
    /// string-encoded `arguments` form decodes to structure.
    #[test]
    fn codex_contract_parses_tail_and_last_assistant_message() {
        let tail = read_tail_from_file(
            &fixture("codex_rollout_transcript.jsonl"),
            10,
            TranscriptFormat::Codex,
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
            vec!["user", "assistant", "user", "assistant"],
            "turns: {turns:#?}"
        );
        assert_eq!(turns[0].text, "Search the directory for TypeScript files.");
        // The shell function_call opens the assistant turn; the closing
        // assistant message merges into it.
        assert_eq!(turns[1].tool_calls.len(), 1);
        assert_eq!(turns[1].tool_calls[0].name, "shell");
        assert_eq!(turns[1].tool_calls[0].input["command"], "dir /s /b *.ts");
        assert!(turns[1].text.starts_with("I found the following TypeScript files"));
        // call_02's arguments are a string-encoded JSON blob — decoded to
        // structure, not delivered as an escaped string.
        assert_eq!(turns[3].tool_calls[0].name, "read_file");
        assert_eq!(turns[3].tool_calls[0].input["file_path"], "src/login.ts");
        assert_eq!(
            last_assistant_message.as_deref(),
            Some("The file looks good. I don't see any bugs.")
        );
    }

    #[test]
    fn codex_cheap_digest_reader_matches_full_reader() {
        let cheap = read_last_assistant_message_from_file(
            &fixture("codex_rollout_transcript.jsonl"),
            TranscriptFormat::Codex,
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
            Some("The file looks good. I don't see any bugs.")
        );
    }

    /// A rollout whose only message lines are the injected context wrappers is
    /// a genuinely-quiet session — `Empty`, not `ShapeChanged`.
    #[test]
    fn codex_context_only_session_degrades_to_empty() {
        let lines = vec![
            r#"{"type":"session_meta","payload":{"id":"x","cwd":"F:\\src","timestamp":"t","cli_version":"0.144.0"}}"#.to_string(),
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<user_instructions>\nuse tabs\n</user_instructions>"}]}}"#.to_string(),
        ];
        let parsed = parse_codex_turns(lines.into_iter(), 10);
        assert!(parsed.turns.is_empty());
        assert!(!parsed.saw_malformed);
        assert_eq!(
            empty_or_shape_changed(parsed.saw_malformed),
            UnavailableReason::Empty
        );
    }

    /// A renamed role/content field in a message payload is a structural break
    /// in the Codex shape — degrade loudly as `ShapeChanged`, never quietly.
    #[test]
    fn codex_renamed_fields_degrade_to_shape_changed() {
        let lines = vec![
            r#"{"type":"response_item","payload":{"type":"message","author":"assistant","blocks":[{"type":"output_text","text":"renamed"}]}}"#.to_string(),
        ];
        let parsed = parse_codex_turns(lines.into_iter(), 10);
        assert!(parsed.turns.is_empty());
        assert!(parsed.saw_malformed, "renamed role/content is malformed");
        assert_eq!(
            empty_or_shape_changed(parsed.saw_malformed),
            UnavailableReason::ShapeChanged
        );
    }

    /// Consecutive function_calls coalesce into one assistant turn that the
    /// closing assistant message also merges into — one turn, not three.
    #[test]
    fn codex_function_calls_and_closing_message_form_one_turn() {
        let lines = vec![
            r#"{"type":"event_msg","payload":{"type":"function_call","name":"shell","arguments":{"command":"ls"},"call_id":"c1"}}"#.to_string(),
            r#"{"type":"event_msg","payload":{"type":"function_call","name":"read_file","arguments":{"file_path":"a.rs"},"call_id":"c2"}}"#.to_string(),
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}"#.to_string(),
        ];
        let parsed = parse_codex_turns(lines.into_iter(), 10);
        assert_eq!(parsed.turns.len(), 1, "turns: {:#?}", parsed.turns);
        assert_eq!(parsed.turns[0].tool_calls.len(), 2);
        assert_eq!(parsed.turns[0].text, "Done.");
        assert_eq!(parsed.last_assistant_message.as_deref(), Some("Done."));
    }

    /// `function_call` under the `response_item` envelope (the shape newer
    /// Codex versions write) parses identically to the `event_msg` form.
    #[test]
    fn codex_function_call_under_response_item_envelope_parses() {
        let lines = vec![
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":{"command":"ls"},"call_id":"c1"}}"#.to_string(),
        ];
        let parsed = parse_codex_turns(lines.into_iter(), 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].tool_calls[0].name, "shell");
    }

    /// The rollout walk finds `rollout-<ts>-<session>.jsonl` under the
    /// `sessions/YYYY/MM/DD/` layout, and returns `None` (→ NoTranscript) for
    /// an unknown session id or a missing sessions dir.
    #[test]
    fn codex_rollout_walk_finds_session_file() {
        let temp = std::env::temp_dir().join(format!(
            "buildmesh_test_codex_sessions_{}",
            std::process::id()
        ));
        let day = temp.join("2026").join("07").join("18");
        std::fs::create_dir_all(&day).unwrap();
        let file = day.join("rollout-2026-07-18T10-00-00-c1234567-89ab-cdef-0123-456789abcdef.jsonl");
        std::fs::write(&file, "{}").unwrap();

        let found = find_codex_rollout_in(&temp, "c1234567-89ab-cdef-0123-456789abcdef");
        assert_eq!(found.as_deref(), Some(file.as_path()));
        assert!(
            find_codex_rollout_in(&temp, "00000000-dead-beef-0000-000000000000").is_none(),
            "unknown session id must not match"
        );
        assert!(
            find_codex_rollout_in(&temp.join("nope"), "x").is_none(),
            "missing sessions dir degrades to None, not an error"
        );
        std::fs::remove_dir_all(&temp).ok();
    }

    /// `for_harness` routes only the codex harness to the Codex format —
    /// every Claude-backed executor id stays on Claude Code.
    #[test]
    fn transcript_format_for_harness_routes_codex_only() {
        assert_eq!(TranscriptFormat::for_harness("codex"), TranscriptFormat::Codex);
        for id in ["anthropic", "claude", "agy", "opencode", "terminal", ""] {
            assert_eq!(TranscriptFormat::for_harness(id), TranscriptFormat::ClaudeCode);
        }
    }
}