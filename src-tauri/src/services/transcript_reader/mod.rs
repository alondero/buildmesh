//! Transcript reader (ADR-0008) "— the deep module that, given an Agent Node's
//! CLI session id and working-directory path, locates and parses the harness's
//! on-disk JSONL transcript and returns the **raw recent turns** (assistant
//! text and tool calls) plus the last assistant message "— or a typed
//! [`Unavailable`] reason when the provider has no readable transcript or the
//! file fails to parse.
//!
//! Seven harness formats are supported, selected by [`TranscriptFormat`]:
//! Claude Code's `~/.claude/projects/<encoded-cwd>/<session>.jsonl`, Cursor's
//! `~/.cursor/projects/<workspace>/agent-transcripts/<session>/<session>.jsonl`,
//! Codex's `~/.codex/sessions/YYYY/MM/DD/rollout-*-<session>.jsonl` (issue
//! #885), Antigravity's per-conversation JSONL, Grok Code's
//! `~/.grok/sessions/<urlencoded-cwd>/<id>/{chat_history.jsonl, updates.jsonl}`
//! (issue #1281), Command Code's
//! `~/.commandcode/projects/<encoded-cwd>/<session>.jsonl` (issues #1407,
//! #1500), and OpenCode's local `opencode.db` SQLite store (issue #1296).
//! All map onto the same [`Turn`]/[`ToolCall`] wire shape, so the Coordinator
//! never learns which harness wrote the file.
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
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub(crate) mod types;
// `TranscriptTail` and `UnavailableReason` are used by the reader's
// own entry points below (return types of the public fns); re-exported
// at `pub` so the public surface `crate::services::transcript_reader::TranscriptTail`
// resolves for downstream IPC consumers (tauri commands, generated
// TS bindings).
// `ToolCall` / `Turn` are used externally by `coordinator::enrichment`
// (test fixtures + `scrub_tail_masks_secrets_in_all_content_surfaces`);
// `TranscriptTail` / `UnavailableReason` are the public return types of
// the reader's `pub fn`s. Suppress the local unused-import warning —
// Rust's lint doesn't track cross-module use of a `pub use` re-export.
#[allow(unused_imports)]
pub use types::{ToolCall, TranscriptTail, Turn, UnavailableReason};
// `AssistantReport` is `pub(crate)` in `types` (it's a circuit-side
// wire type, not part of the public IPC surface) — re-export at the
// same `pub(crate)` visibility so `coordinator::enrichment::assistant_report`
// can return it via `crate::services::transcript_reader::AssistantReport`.
pub(crate) use types::AssistantReport;
// Internal helpers used by the reader's own entry points below (NOT
// re-exported — external callers reach the seam directly via the
// adapter modules, issue #1661 step 10).
use types::{Parsed, build_tail, effective_tail, empty_or_shape_changed};
use adapters::claude_code::parse_turns;
use adapters::opencode::{
    opencode_resolve, parse_opencode_messages, read_opencode_digest,
    read_opencode_message_rows, read_opencode_tail, OPENCODE_DIGEST_WINDOW,
};

// Per-harness adapters + the registry seam. Issue #1661 step 1: Claude Code
// is the first real adapter; the other six are thin wrappers around the
// existing free functions in this module. Each harness migrates end-to-end
// in its own commit (steps 2-8 of #1661).
pub(crate) mod adapter;
pub(crate) mod adapters;

/// Which harness's on-disk JSONL shape a transcript uses. Selected once at the
/// enrichment boundary (from the node's resolved harness adapter id) and passed
/// down, so the reader itself never consults provider state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFormat {
    ClaudeCode,
    Cursor,
    Codex,
    /// Command Code writes one structured message event per JSONL line under
    /// `~/.commandcode/projects/<encoded-cwd>/<session-id>.jsonl` (issue #1500).
    CommandCode,
    /// Antigravity CLI (issue #1283). Persisted at
    /// `~/.gemini/antigravity-cli/brain/<conversation-id>/.system_generated/
    /// logs/transcript.jsonl` (with `transcript_full.jsonl` as the untruncated
    /// fallback). One JSON object per turn — flat shape, no `message.id`
    /// coalescing needed.
    Agy,
    /// Grok Code writes per-session directories under
    /// `~/.grok/sessions/<urlencoded-cwd>/<session-id>/` carrying
    /// `chat_history.jsonl` (the per-message conversation log) and
    /// `updates.jsonl` (event-level telemetry). Issue #1281.
    Grok,
    /// OpenCode (issue #1296) stores all session messages in a single local
    /// SQLite database (`~/.local/share/opencode/opencode.db`), not in
    /// per-session JSONL files. The reader pulls messages by `session_id`
    /// and normalises each row to the public `opencode export <id>` JSON
    /// shape (`{info, parts, …}` per message) before parsing. Dispatch is
    /// short-circuited in [`read_tail`] / [`read_last_assistant_message`] —
    /// this variant does not flow through the file-based `locate_transcript`
    /// chain because OpenCode has no per-session transcript file.
    OpenCode,
}

impl TranscriptFormat {
    /// Map a resolved harness adapter id to its transcript format. Every
    /// Claude-Code-backed executors (the `anthropic` adapter behind the built-in
    /// subscription and all custom MiniMax/DeepSeek profiles) share the Claude
    /// Code format. Cursor has the same message shape but a workspace-scoped
    /// path, while Codex writes its own rollout format. Antigravity's
    /// per-conversation JSONL is parsed via `TranscriptFormat::Agy`
    /// (issue #1283); Grok writes its own flat per-role JSONL (issue #1281);
    /// Command Code writes structured message events (issue #1407).
    /// Kimi Code (wayfinder #918) writes standard JSONL
    /// (`~/.kimi/sessions/wire.jsonl`) but the path resolver isn't wired yet
    /// — tracked as a follow-up. OpenCode (#1296) stores messages in SQLite
    /// and routes through its own read entry point; the variant is included
    /// here so `for_harness` agrees with the dispatch table in
    /// [`read_tail`] / [`read_last_assistant_message`].
    pub fn for_harness(harness_id: &str) -> Self {
        match harness_id {
            "codex" => TranscriptFormat::Codex,
            "commandcode" => TranscriptFormat::CommandCode,
            "cursor" => TranscriptFormat::Cursor,
            "agy" => TranscriptFormat::Agy,
            "grok" => TranscriptFormat::Grok,
            "opencode" => TranscriptFormat::OpenCode,
            _ => TranscriptFormat::ClaudeCode,
        }
    }
}
// --- Shared Claude-Code JSONL primitives (also used by session_discovery) ---
// `encode_path` / `is_synthetic_message` / `concat_text_blocks` /
// `first_text_block` live in `services::transcript_paths` (issue #1661
// step 5); `commandcode_project_slug` lives in
// `adapters::commandcode`. All external callers have been updated
// to import from those locations directly.

// --- Wire types ---
// Wire types (`ToolCall`, `Turn`, `UnavailableReason`, `TranscriptTail`,
// `AssistantReport`) live in `super::types` and are re-exported at the top of
// this file so existing call sites continue to resolve.

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
    // OpenCode stores all sessions in a single SQLite DB — there is no
    // per-session transcript file. Short-circuit before the file-based chain
    // (issue #1296).
    if format == TranscriptFormat::OpenCode {
        return read_opencode_tail(session_id, node_path, tail);
    }
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
    // Issue #1661: per-harness dispatch via the registry seam. Each
    // adapter owns its own locator; the reader never holds per-format
    // path knowledge. OpenCode's adapter returns `None` because its
    // data lives in a shared SQLite DB (issue #1296); the reader's
    // OpenCode path short-circuits before this is called.
    let adapter = adapter::dispatch(adapter_id_for_format(format))
        .unwrap_or_else(adapter::default_adapter);
    adapter.locate(adapter::LocateCtx {
        session_id,
        node_path,
    })
}

// `agy_locator_in` lives in `adapters::agy`; `commandcode_*` in
// `adapters::commandcode`; `cursor_*` in `adapters::cursor`;
// `claude_code::*` in `adapters::claude_code`; `codex::*` in
// `adapters::codex`; `opencode::*` in `adapters::opencode`. All
// external callers have been updated to import directly from the
// adapter modules (issue #1661 step 10).

// --- OpenCode transcript reader (issue #1296) ---
//
// OpenCode stores every session's messages in a single local SQLite database
// (`~/.local/share/opencode/opencode.db`), not in per-session JSONL files
// (issue #1296). The reader bypasses the file-based `locate_transcript`
// chain in [`read_tail`] / [`read_last_assistant_message`] and queries the DB
// directly through `read_opencode_messages`. The parser takes a slice of
// `serde_json::Value` (no synthetic envelope roundtrip — issue #1296 review
// surfaced the cost of wrapping rows in `{info, messages}` just to unwrap
// `messages` on the next stack frame).
//
// **Two read paths, two row budgets.**
// - `read_opencode_tail` (full /log endpoint): budget = `tail * factor` rows
//   because OpenCode rows are *message events*, not turns — an assistant
//   turn can span multiple rows (`msg-004` + `msg-005` in our fixture) and
//   reasoning-only / tool-only messages drop, so bounding by `tail` rows
//   guarantees the caller gets fewer than `tail` turns.
// - `read_opencode_digest` (`GET /nodes` digest): budget = `DIGEST_WINDOW`
//   so a single user reply at the latest message does not wipe the
//   blocking question — the parser must see the assistant turn before
//   it. `DIGEST_WINDOW` is large enough to span a typical user->
//   assistant exchange but bounded so a multi-thousand-row session
//   doesn't full-scan on every Coordinator poll (default 50).
//
// **`SQLITE_OPEN_READ_ONLY` + `busy_timeout`.** OpenCode writes to
// `opencode.db` while the agent is alive, and a long-running query from
// the Coordinator's Tokio worker (issue #1380) is the silent-degrade risk
// if we trip SQLITE_BUSY. A 500 ms busy timeout lets SQLite wait out
// brief concurrent writes instead of returning `None` → `Unreadable`.
//
// **Defensive parsing.** The parser accepts the documented part types
// (`text`, `reasoning`, `tool`, `step-start`, `step-finish`, …) and silently
// drops unknown ones — same "graceful failure on unknown event types" rule
// Grok (#1281) follows. A renamed `info.role` or `parts` is a structural
// break and degrades loudly as `ShapeChanged`, not the quieter `Empty`.
//
// **Schema assumption (verify against the live CLI):** `message(id PK,
// session_id, time_created, data TEXT)`. The `data` blob is the full
// MessageV2 record (`role`, `time`, `parts`, ...). If OpenCode ever splits
// `parts` into a separate `part` table, `read_opencode_messages` gains a
// second query and the contract test
// (`opencode_locator_reads_messages_from_file_backed_db`) is the pin.

// `find_codex_rollout`, `find_codex_rollout_in`, `subdirs_sorted_desc`
// moved to `adapters::codex` (issue #1661 step 6). Test-module
// references below import directly from the adapter.
// `read_opencode_*` / `parse_opencode_*` / OpenCode constants moved to
// `adapters::opencode` (issue #1661 step 7). The reader's OpenCode
// short-circuits in `read_tail` / `read_last_assistant_message` /
// `read_assistant_report` import directly from the adapter.

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
    // JSONL pipeline only. OpenCode never reaches this dispatch — see
    // Issue #1661: parse dispatch goes through the registry seam. Every
    // registered adapter (claude_code, agy, codex, cursor, commandcode,
    // grok) handles its own parser; OpenCode short-circuits before this
    // is reached so its adapter's `parse` is unreachable in practice.
    let adapter = adapter::dispatch(adapter_id_for_format(format))
        .unwrap_or_else(adapter::default_adapter);
    adapter.parse(Box::new(lines), keep)
}

/// Map a [`TranscriptFormat`] to its adapter's harness id (issue #1661).
/// Will collapse to a direct `&'static str` once the enum is replaced in
/// a later step; for now this is the bridge from the public `TranscriptFormat`
/// API to the registry's `&str` keys.
fn adapter_id_for_format(format: TranscriptFormat) -> &'static str {
    match format {
        TranscriptFormat::ClaudeCode => "claude_code",
        TranscriptFormat::Agy => "agy",
        TranscriptFormat::Codex => "codex",
        TranscriptFormat::Cursor => "cursor",
        TranscriptFormat::CommandCode => "commandcode",
        TranscriptFormat::Grok => "grok",
        // OpenCode's adapter's `parse` is unreachable (the reader
        // short-circuits to `read_opencode_*` before `parse_transcript`
        // runs); routing through the registry still resolves correctly.
        TranscriptFormat::OpenCode => "opencode",
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
    // OpenCode short-circuit (issue #1296) — `read_last_assistant_message`
    // for OpenCode uses a fixed window (`OPENCODE_DIGEST_WINDOW`), NOT the
    // caller-supplied `tail` (which is ignored on the digest path for
    // every other adapter). A user reply at the latest row would otherwise
    // wipe the blocking question — the digest must always have enough
    // context to surface the most recent assistant message.
    if format == TranscriptFormat::OpenCode {
        return read_opencode_digest(session_id, node_path);
    }
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
// `AssistantReport` moved to `super::types` (a wire type used by the reader's
// circuit-report path; not harness-specific).

/// A circuit needs the identity of the assistant response, not the file's
/// mtime: a new user prompt or tool event also changes the transcript file.
/// Unsupported/non-JSONL stores fall back to live per-turn PTY observation.
pub(crate) fn read_assistant_report(
    format: TranscriptFormat,
    session_id: Option<&str>,
    node_path: &str,
) -> Option<AssistantReport> {
    if format == TranscriptFormat::OpenCode {
        let (path, session_id) = opencode_resolve(session_id, node_path).ok()?;
        return opencode_assistant_report(&path, session_id);
    }
    let path = locate_transcript(format, session_id?, node_path)?;
    assistant_report_from_file(&path, format)
}

fn opencode_assistant_report(path: &Path, session_id: &str) -> Option<AssistantReport> {
    use sha2::{Digest, Sha256};
    read_opencode_message_rows(path, session_id, OPENCODE_DIGEST_WINDOW)?
        .into_iter().rev().find_map(|(id, message)| {
            let text = parse_opencode_messages(&[message], 1).last_assistant_message?;
            Some(AssistantReport {
                revision: format!("{id}:{:x}", Sha256::digest(text.as_bytes())),
                text,
            })
        })
}

fn assistant_report_from_file(path: &Path, format: TranscriptFormat) -> Option<AssistantReport> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let start = size.saturating_sub(256 * 1024);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut reader = BufReader::new(file.take(size - start));
    let mut offset = start;
    let mut line = String::new();
    if start > 0 {
        // The window may begin in the middle of a UTF-8 character.
        offset += reader.read_until(b'\n', &mut Vec::new()).ok()? as u64;
    }
    let mut lines = Vec::new();
    let mut assistant_line_offset = None;
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line).ok()?;
        if bytes == 0 { break; }
        offset += bytes as u64;
        // Ignore a record the writer has not finished publishing yet.
        if !line.ends_with('\n') { break; }
        if line_has_assistant_text(format, &line) {
            assistant_line_offset = Some(offset);
        }
        lines.push(std::mem::take(&mut line));
    }
    let text = parse_transcript(format, lines.into_iter(), 1).last_assistant_message?;
    let offset = assistant_line_offset?;
    Some(AssistantReport {
        // Revisions identify the assistant content plus its position. Hashing
        // the normalized text keeps file-backed providers consistent with the
        // OpenCode report reader; the offset still distinguishes identical
        // responses emitted at different points in one transcript.
        revision: format!("{offset}:{:x}", Sha256::digest(text.as_bytes())),
        text,
    })
}

/// Identify a line that can advance `Parsed::last_assistant_message` without
/// invoking one of the full rolling transcript parsers. This is deliberately
/// a single structural JSON inspection per line; the complete parser runs once
/// over the collected window below, avoiding the old O(lines × full-parser)
/// loop on large transcript tails.
fn line_has_assistant_text(format: TranscriptFormat, line: &str) -> bool {
    // Issue #1661: per-harness dispatch via the registry seam. Each adapter
    // owns its own line predicate; the reader never holds per-format logic.
    let adapter = adapter::dispatch(adapter_id_for_format(format))
        .unwrap_or_else(adapter::default_adapter);
    adapter.line_has_assistant_text(line)
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
// `effective_tail`, `Parsed`, `build_tail`, `empty_or_shape_changed` moved to
// `super::types` (format-agnostic, shared by every TranscriptAdapter).
// `parse_turns` + `extract_tool_calls` moved to
// `adapters::claude_code` (issue #1661 step 8). The Claude Code
// adapter is the sole owner of its message-id-coalescing parser; the
// test module imports each parser directly.

// `push_bounded`, `cap_tool_calls`, `merge_into` moved to `super::types`
// (format-agnostic rolling-buffer helpers).

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

// `is_codex_synthetic`, `codex_concat_text`, `codex_tool_input`,
// `parse_codex_turns` moved to `adapters::codex` (issue #1661 step 6).

// --- Command Code transcript parser (issue #1407) ---
//
// Command Code emits an event stream in which session/model metadata is mixed
// with message records. The current message shape is:
//
//   {"type":"message", "id":"...", "message": {
//       "role":"user"|"assistant", "content":[...]}}
//
// Thinking, reasoning, and tool-result blocks are transport details rather
// than Coordinator dialogue. They are deliberately omitted from `Turn.text`;
// tool invocations remain available through the shared `ToolCall` shape.

/// The meaningful activity in one Command Code `message` envelope.
///
/// This narrow classifier is shared with the passive lifecycle watcher so its
/// definition of a real user/assistant turn cannot drift from the transcript
/// reader's. In particular, thinking/reasoning-only and tool-result records
/// are deliberately absent.
// `CommandCodeMessageActivity`, `commandcode_message_activity`,
// `contains_tool_result`, `parse_commandcode_turns` moved to
// `adapters::commandcode` (issue #1661 step 3).

// --- Antigravity transcript parser (issue #1283) ---
//
// Antigravity's `transcript.jsonl` is a flat one-line-per-turn shape (no
// `message.id` coalescing across lines). Each line carries:
//
//   - `source`: USER_EXPLICIT | MODEL | SYSTEM  — who wrote this turn
//   - `type`:   USER_INPUT | PLANNER_RESPONSE | TASK_NOTIFICATION — coarse kind
//   - `content`: the prompt or reply text (always a bare string)
//   - `thinking`: optional chain-of-thought (not surfaced as text)
//   - `tool_calls`: [{name, args}]  — args is the raw input shape
//
// Mapping onto [`Turn`]:
//
//   USER_EXPLICIT     → user turn with `role: "user"`
//   MODEL             → assistant turn with `role: "assistant"`, content + tools
//   SYSTEM            → skipped (TASK_NOTIFICATION is harness plumbing, like
//                       Claude's `<task-notification>` injection)
//
// A MODEL line with neither content nor tool_calls is a `thinking`-only turn —
// skip it (nothing for the Coordinator to use), don't flag malformed. A
// USER_EXPLICIT line missing `content` is flagged malformed so a renamed-shape
// drift degrades loudly as `ShapeChanged` instead of silently surfacing
// nothing.

// `parse_agy_turns`, `is_agy_synthetic`, `extract_agy_tool_calls` moved
// to `adapters::agy` (issue #1661 step 5).

// --- Grok Code parser (issue #1281) ---
//
// Grok Code writes per-session directories at
//   ~/.grok/sessions/<urlencoded-cwd>/<session-id>/
// containing `chat_history.jsonl` (the per-message conversation log — primary
// transcript) and `updates.jsonl` (event-level telemetry). The wire shape per
// line is flat JSONL (no envelope), so unlike Codex the per-harness parser
// doesn't need to dispatch on an outer envelope; it dispatches on the `role`
// field instead. Grok stores each tool call inline on the assistant turn
// itself (`{role:"assistant", content:"...", tool_calls:[{name, args}]}`) —
// not as a separate tool-result event line — so the parser only needs to read
// per-line `role` + `content` + `tool_calls`. Tool-result echoes arrive as
// `{role:"tool", content:"..."}` lines; the parser drops them, like Claude's
// tool_result.
//
// Unknown event types (`command_status`, `telemetry`, `heartbeat`, …) are
// silently skipped — issue #1281 acceptance criterion: "graceful failure on
// unknown event types". They are never flagged as malformed.

// --- Pending background tasks (issue #878) ---
//
// `count_pending_background_tasks`, `pending_background_task_ids`,
// `LAUNCH_ID` / `NOTIFIED_ID` regex statics, and `LAUNCH_MARKER`
// moved to `adapters::claude_code` (issue #1661 step 8). Re-exported
// from this module so the reader's test module's references keep
// resolving.

#[cfg(test)]
mod tests {
    use super::*;
    // The reader module no longer re-exports per-harness helpers from
    // the adapter modules (issue #1661 step 10) — every external
    // caller now imports from the adapter directly. The test module
    // pulls each parser through its own path so the contract tests
    // exercise the seam, not a legacy facade.
    use crate::services::transcript_paths::{
        concat_text_blocks, encode_path, first_text_block, is_synthetic_message,
    };
    use crate::services::transcript_reader::types::{
        MAX_TOOL_STRING, MAX_TURN_TOOL_CALLS, truncate_json_strings,
    };
    use crate::services::transcript_reader::adapters::agy::{agy_locator_in, parse_agy_turns};
    use crate::services::transcript_reader::adapters::claude_code::{
        count_pending_background_tasks, parse_turns, pending_background_task_ids,
    };
    use crate::services::transcript_reader::adapters::codex::{find_codex_rollout_in, parse_codex_turns};
    use crate::services::transcript_reader::adapters::commandcode::{
        commandcode_project_slug, commandcode_sessions_dir, commandcode_transcript_path_in,
        parse_commandcode_turns,
    };
    use crate::services::transcript_reader::adapters::cursor::{
        cursor_transcript_path_in, cursor_workspace_slug,
    };
    use crate::services::transcript_reader::adapters::opencode::{
        parse_opencode_export, parse_opencode_messages, read_opencode_digest_from_messages,
        read_opencode_messages, read_opencode_tail_from_messages,
    };
    // Reader-internal EnvType (used by the commandcode_sessions_dir test
    // and the cursor_workspace_slug test). The reader's own entry
    // points no longer need it after the migrations, but the tests
    // do.
    use crate::models::EnvType;
    use std::path::Path;

    #[test]
    fn circuit_recovers_a_delayed_codex_parent_report_without_a_new_pty_turn() {
        let root = tempfile::TempDir::new().unwrap();
        let directory = "F:/repo/.claude/worktrees/source";
        let anchor = 1_788_701_729_172;
        let id = "01a076ee-6c95-7c82-9e5f-928e9f43ad7a";
        let recover = || crate::services::codex_session::find_historic_id_for_directory_in(root.path(), directory, anchor, true);
        assert!(recover().is_none(), "initial startup capture sees no rollout yet");
        assert!(read_assistant_report(TranscriptFormat::Codex, None, directory).is_none());
        let day = root.path().join("2026/09/06");
        fs::create_dir_all(&day).unwrap();
        let path = day.join(format!("rollout-{id}.jsonl"));
        let meta = serde_json::json!({"type":"session_meta", "payload":{
            "id":id, "cwd":directory, "timestamp":"2026-09-06T13:35:32.115Z", "source":"cli", "thread_source":"user"}});
        let report = serde_json::json!({"type":"response_item", "payload":{
            "type":"message", "role":"assistant", "phase":"final_answer", "content":[{"type":"output_text", "text":"Implementation and verification finished; changes are uncommitted."}]}});
        fs::write(&path, format!("{meta}\n{report}\n")).unwrap();
        let child = serde_json::json!({"type":"session_meta", "payload":{
            "id":"01a076f5-cae9-7db2-b159-7c4682cd2b7f", "cwd":directory,
            "timestamp":"2026-09-06T13:36:32.115Z", "source":{"subagent":{}}}});
        fs::write(day.join("child.jsonl"), format!("{child}\n")).unwrap();
        assert_eq!(recover().as_deref(), Some(id));
        let actual = assistant_report_from_file(&path, TranscriptFormat::Codex).unwrap();
        assert_eq!(actual.text, "Implementation and verification finished; changes are uncommitted.");
        assert!(!actual.revision.is_empty());
    }
    fn write_fixture(name: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "buildmesh_transcript_{name}_{}.jsonl",
            std::process::id()
        ));
        std::fs::write(&path, body).unwrap();
        path
    }
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn circuit_report_revision_tracks_assistant_response_not_user_or_tool_activity() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let assistant = r#"{"type":"assistant","message":{"id":"a","role":"assistant","content":[{"type":"text","text":"Done."}]}}"#;
        writeln!(file, "{assistant}").unwrap();
        let first = assistant_report_from_file(file.path(), TranscriptFormat::ClaudeCode).unwrap();
        let user = r#"{"type":"user","message":{"role":"user","content":"Now finish and open the PR"}}"#;
        writeln!(file, "{user}").unwrap();
        let waiting = assistant_report_from_file(file.path(), TranscriptFormat::ClaudeCode).unwrap();
        assert_eq!(waiting.revision, first.revision);
        let tool = r#"{"type":"assistant","message":{"id":"tool","role":"assistant","content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"git status"}}]}}"#;
        writeln!(file, "{tool}").unwrap();
        assert_eq!(assistant_report_from_file(file.path(), TranscriptFormat::ClaudeCode).unwrap().revision, first.revision);
        // Even byte-identical responses have distinct positions in the log.
        writeln!(file, "{assistant}").unwrap();
        let second = assistant_report_from_file(file.path(), TranscriptFormat::ClaudeCode).unwrap();
        assert_eq!(second.text, first.text);
        assert_ne!(second.revision, first.revision);
        file.write_all(br#"{"type":"assistant""#).unwrap();
        assert_eq!(assistant_report_from_file(file.path(), TranscriptFormat::ClaudeCode).unwrap().revision, second.revision);
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

    /// `for_harness` routes each harness to its native format — codex to
    /// Codex, cursor to Cursor, agy to a dedicated AGY shape (#1283),
    /// grok to its own Grok shape (#1281), opencode to OpenCode (#1296).
    /// Every Claude-backed executor id stays on Claude Code. The
    /// Claude-routed list deliberately excludes "agy", "grok", and
    /// "opencode" so the catch-all ClaudeCode assertion can't mask a
    /// future routing regression.
    #[test]
    fn transcript_format_for_harness_routes_each_format() {
        assert_eq!(TranscriptFormat::for_harness("codex"), TranscriptFormat::Codex);
        assert_eq!(
            TranscriptFormat::for_harness("commandcode"),
            TranscriptFormat::CommandCode
        );
        assert_eq!(TranscriptFormat::for_harness("cursor"), TranscriptFormat::Cursor);
        assert_eq!(TranscriptFormat::for_harness("agy"), TranscriptFormat::Agy);
        assert_eq!(TranscriptFormat::for_harness("grok"), TranscriptFormat::Grok);
        assert_eq!(TranscriptFormat::for_harness("opencode"), TranscriptFormat::OpenCode);
        for id in ["anthropic", "claude", "terminal", ""] {
            assert_eq!(TranscriptFormat::for_harness(id), TranscriptFormat::ClaudeCode);
        }
    }

    #[test]
    fn commandcode_slug_matches_v143_layout() {
        // Observed on-disk layout in Command Code v1.43.0 (issue #1500).
        assert_eq!(
            commandcode_project_slug(
                r"F:\src\buildmesh\.claude\worktrees\saucy-thunderous-cove"
            ),
            "f-src-buildmesh-claude-worktrees-saucy-thunderous-cove"
        );
        assert_eq!(
            commandcode_project_slug(
                r"F:\src\buildmesh\.claude\worktrees\gh1377-mobile-mobile-companion-quick-action-triage"
            ),
            "f-src-buildmesh-claude-worktrees-gh1377-mobile-mobile-companion-quick-action-triage"
        );
        assert_eq!(commandcode_project_slug(r"C:\Users\User"), "c-users-user");
        assert_eq!(
            commandcode_project_slug("/home/user/project"),
            "home-user-project"
        );
        assert_eq!(
            commandcode_project_slug(
                r"F:\src\buildmesh\.claude\worktrees\gh1376-ui-design-system--surface-elevation-typogra"
            ),
            "f-src-buildmesh-claude-worktrees-gh1376-ui-design-system-surface-elevation-typogra"
        );
        assert_eq!(commandcode_project_slug(""), "");
        assert_eq!(commandcode_project_slug("///"), "");
    }

    #[test]
    fn commandcode_sessions_dir_resolves_under_projects() {
        let dir = commandcode_sessions_dir(
            EnvType::Windows,
            r"F:\src\buildmesh\.claude\worktrees\saucy-thunderous-cove",
        )
        .expect("windows sessions dir should resolve");
        let dir_str = dir.to_string_lossy().replace('\\', "/");
        assert!(
            dir_str.ends_with(
                "projects/f-src-buildmesh-claude-worktrees-saucy-thunderous-cove"
            ),
            "sessions dir should be projects/<slug>, got {dir_str}"
        );
        assert!(
            !dir_str.contains("sessions"),
            "must not use the legacy sessions dir, got {dir_str}"
        );
        assert!(
            commandcode_sessions_dir(EnvType::Windows, "").is_none(),
            "empty slug must not resolve"
        );
    }

    #[test]
    fn commandcode_transcript_path_uses_session_id_under_sessions_root() {
        // Issue #1500: the sessions root is the per-project
        // `projects/<encoded-cwd>/` dir; the pure locator just joins the id.
        let path = commandcode_transcript_path_in(
            Path::new(
                r"C:\Users\adam\.commandcode\projects\f-src-buildmesh-claude-worktrees-saucy-thunderous-cove",
            ),
            "3fadada6-e0a3-44a2-ab68-ce1ecf7207a9",
        );
        assert_eq!(
            path,
            PathBuf::from(
                r"C:\Users\adam\.commandcode\projects\f-src-buildmesh-claude-worktrees-saucy-thunderous-cove\3fadada6-e0a3-44a2-ab68-ce1ecf7207a9.jsonl"
            )
        );
    }

    #[test]
    fn commandcode_contract_parses_nested_messages_and_drops_internal_blocks_and_tool_results() {
        let tail = read_tail_from_file(
            &fixture("commandcode_transcript.jsonl"),
            10,
            TranscriptFormat::CommandCode,
        );
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = tail
        else {
            panic!("Command Code fixture should parse to an available tail, got {tail:?}");
        };

        let roles: Vec<&str> = turns.iter().map(|turn| turn.role.as_str()).collect();
        assert_eq!(
            roles,
            vec!["user", "assistant", "assistant", "user", "assistant"],
            "ordinary tool_result echoes and non-message events should be skipped; turns: {turns:#?}"
        );
        assert_eq!(turns[0].text, "Inspect src/login.ts for the redirect bug.");
        assert_eq!(turns[1].text, "I'll inspect the file first.");
        assert_eq!(turns[1].tool_calls.len(), 1);
        assert_eq!(turns[1].tool_calls[0].name, "read_file");
        assert_eq!(turns[1].tool_calls[0].input["file_path"], "src/login.ts");
        assert_eq!(turns[2].text, "I have prepared the redirect fix.");
        assert_eq!(
            turns[2].tool_calls[0].input["diff"],
            "@@ -1 +1 @@\n-const redirect = nextUrl;\n+const redirect = new URL(nextUrl, window.location.origin);"
        );
        assert_eq!(
            turns[4].text,
            "The redirect now preserves the query string."
        );
        assert_eq!(
            last_assistant_message.as_deref(),
            Some("The redirect now preserves the query string.")
        );
    }

    #[test]
    fn commandcode_cheap_digest_reader_matches_full_reader() {
        let digest = read_last_assistant_message_from_file(
            &fixture("commandcode_transcript.jsonl"),
            TranscriptFormat::CommandCode,
        );
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = digest
        else {
            panic!("expected Command Code digest to be available");
        };
        assert!(turns.is_empty(), "cheap reader must not return turns");
        assert_eq!(
            last_assistant_message.as_deref(),
            Some("The redirect now preserves the query string.")
        );
    }

    #[test]
    fn commandcode_thinking_only_turn_is_skipped() {
        let lines = [
            r#"{"type":"message","id":"user-1","message":{"role":"user","content":"Inspect the redirect."}}"#,
            r#"{"type":"message","id":"assistant-1","message":{"role":"assistant","content":[{"type":"thinking","thinking":"I am still inspecting the redirect."}]}}"#,
        ];
        let parsed = parse_commandcode_turns(lines.into_iter().map(str::to_string), 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].text, "Inspect the redirect.");
        assert_eq!(parsed.last_assistant_message, None);
    }

    #[test]
    fn commandcode_renamed_fields_degrade_to_shape_changed() {
        let cases = [
            r#"{"type":"message","id":"assistant-1","message":{"author":"assistant","content":[{"type":"text","text":"renamed"}]}}"#,
            r#"{"type":"message","id":"assistant-1","message":{"role":"assistant","blocks":[{"type":"text","text":"renamed"}]}}"#,
        ];

        for line in cases {
            let parsed = parse_commandcode_turns(std::iter::once(line.to_string()), 10);
            assert!(
                parsed.turns.is_empty(),
                "renamed shape should not produce turns"
            );
            assert!(
                parsed.saw_malformed,
                "renamed fields must be marked malformed"
            );
            assert_eq!(
                empty_or_shape_changed(parsed.saw_malformed),
                UnavailableReason::ShapeChanged
            );
        }
    }

    #[test]
    fn commandcode_non_message_stream_degrades_to_empty() {
        let path = write_fixture(
            "commandcode_empty",
            r#"{"type":"session","id":"sess-commandcode-empty"}
{"type":"model_change","model":"commandcode-default"}
{"type":"telemetry","event":"heartbeat"}
"#,
        );
        let tail = read_tail_from_file(&path, 10, TranscriptFormat::CommandCode);
        std::fs::remove_file(path).ok();

        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::Empty),
            "metadata-only streams are quiet, not malformed"
        );
    }

    #[test]
    fn commandcode_rolling_buffer_evicts_old_turns_but_keeps_digest() {
        let mut lines = Vec::new();
        for i in 0..50 {
            lines.push(format!(
                r#"{{"type":"message","id":"user-{i}","message":{{"role":"user","content":"prompt {i}"}}}}"#
            ));
            lines.push(format!(
                r#"{{"type":"message","id":"assistant-{i}","message":{{"role":"assistant","content":[{{"type":"text","text":"reply {i}"}}]}}}}"#
            ));
        }

        let parsed = parse_commandcode_turns(lines.into_iter(), 3);
        assert_eq!(parsed.turns.len(), 3, "the rolling buffer must honor keep");
        assert_eq!(
            parsed.turns.last().map(|turn| turn.text.as_str()),
            Some("reply 49")
        );
        assert_eq!(parsed.last_assistant_message.as_deref(), Some("reply 49"));
    }

    #[test]
    fn commandcode_tool_call_input_truncates_large_string_leaves() {
        let big = "x".repeat(100 * 1024);
        let line = serde_json::json!({
            "type": "message",
            "id": "assistant-big-input",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "name": "write_file",
                    "input": {"path": "src/main.rs", "content": big}
                }]
            }
        })
        .to_string();

        let parsed = parse_commandcode_turns(std::iter::once(line), 10);
        let content = parsed.turns[0].tool_calls[0].input["content"]
            .as_str()
            .expect("tool input content should remain a string");
        assert!(content.ends_with('…'));
        assert!(content.chars().count() <= MAX_TOOL_STRING + 1);
    }

    #[test]
    fn commandcode_whitespace_tool_turn_does_not_update_digest() {
        let line = serde_json::json!({
            "type": "message",
            "id": "assistant-whitespace",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "   "},
                    {"type": "tool_use", "name": "Read", "input": {}}
                ]
            }
        })
        .to_string();

        let parsed = parse_commandcode_turns(std::iter::once(line), 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.last_assistant_message, None);
    }

    #[test]
    fn cursor_workspace_slug_matches_cursor_storage_names() {
        assert_eq!(
            cursor_workspace_slug("/Users/adam/src/buildmesh"),
            "Users-adam-src-buildmesh"
        );
        assert_eq!(
            cursor_workspace_slug("C:\\Users\\adam\\src\\buildmesh"),
            "c-Users-adam-src-buildmesh"
        );
        assert_eq!(
            cursor_workspace_slug("C:\\Users\\adam\\src\\buildmesh\\.claude\\worktrees\\fancy-name"),
            "c-Users-adam-src-buildmesh--claude-worktrees-fancy-name"
        );
    }

    #[test]
    fn cursor_transcript_path_uses_workspace_scoped_session_directory() {
        let path = cursor_transcript_path_in(
            Path::new("/home/adam/.cursor"),
            "session-123",
            "C:\\Users\\adam\\src\\buildmesh",
        );
        assert_eq!(
            path,
            PathBuf::from(
                r#"/home/adam/.cursor/projects/c-Users-adam-src-buildmesh/agent-transcripts/session-123/session-123.jsonl"#
            )
        );
    }

    #[test]
    fn cursor_jsonl_reuses_the_shared_message_parser() {
        let path = write_fixture(
            "cursor_transcript",
            r#"{"type":"user","message":{"role":"user","content":"Inspect the cursor path"}}
{"type":"assistant","message":{"role":"assistant","id":"msg-1","content":[{"type":"tool_use","name":"Read","input":{"file":"src/main.rs"}}]}}
{"type":"assistant","message":{"role":"assistant","id":"msg-1","content":[{"type":"text","text":"The path is wired."}]}}
"#,
        );
        let tail = read_tail_from_file(&path, 10, TranscriptFormat::Cursor);
        std::fs::remove_file(path).ok();

        let TranscriptTail::Available { turns, .. } = tail else {
            panic!("Cursor's compatible JSONL should be readable");
        };
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[1].text, "The path is wired.");
        assert_eq!(turns[1].tool_calls[0].name, "Read");
    }

    // Antigravity transcript parser (issue #1283)
    //
    // The contract test exercises the *real* AGY shape with one of each
    // line type: USER_INPUT (user prompt), two MODEL turns (one with
    // a tool call, the next with text + a tool call), a SYSTEM
    // TASK_NOTIFICATION (harness plumbing — must be dropped), a follow-up
    // USER_INPUT, and a final MODEL turn flagged with `status: ERROR`.
    // The cheap digest reader must agree with the full reader on the
    // last assistant message — same contract as the Codex / Claude cases.
    // ---------------------------------------------------------------

    /// Contract test over the checked-in AGY fixture: noise lines
    /// (SYSTEM `TASK_NOTIFICATION`) are dropped; MODEL turns keep both
    /// their text and their tool calls (mapped from `args` to the shared
    /// `input` wire shape); the last assistant text is recovered from
    /// the full stream regardless of how small a tail the caller asks
    /// for.
    #[test]
    fn agy_contract_parses_tail_and_last_assistant_message() {
        let tail = read_tail_from_file(
            &fixture("agy_transcript.jsonl"),
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
            turns[1].tool_calls[0].input["file_path"],
            "src/login.ts",
            "AGY's `args` field is mapped onto the shared `input` wire shape"
        );
        // The second MODEL turn has text + a different tool call shape.
        assert_eq!(
            turns[2].text,
            "Found it — the redirect drops the query string. Shall I apply the fix?"
        );
        assert_eq!(turns[2].tool_calls[0].name, "search_replace");
        assert_eq!(
            turns[2].tool_calls[0].input["file_path"],
            "src/login.ts"
        );
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

    /// The cheap digest path must agree with the full reader for AGY —
    /// when an `AwaitingInput` AGY node is reading its blocking question,
    /// the digest endpoint can land on either path.
    #[test]
    fn agy_cheap_digest_reader_matches_full_reader() {
        let cheap = read_last_assistant_message_from_file(
            &fixture("agy_transcript.jsonl"),
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

    /// SYSTEM `TASK_NOTIFICATION` lines are harness plumbing — same as
    /// Claude's `<task-notification>` injection. A session whose only
    /// lines are notifications (plus a stray user turn whose text happens
    /// to be wrapped in a synthetic tag) is a genuinely-quiet session,
    /// `Empty` not `ShapeChanged`.
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

    /// A renamed `source` value (e.g. `HUMAN_EXPLICIT` after an AGY
    /// upgrade, or `source` removed entirely) on a line that should
    /// carry a turn is malformed: the parser can't classify it, so the
    /// degraded result must be `ShapeChanged` rather than `Empty`.
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

    /// A MODEL line with no content AND no tool calls is a `thinking`-
    /// only turn — silently dropped so a session whose MODEL replies are
    /// just chain-of-thought doesn't false-positive `ShapeChanged`.
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

    /// A single MODEL turn carrying `MAX_TURN_TOOL_CALLS + extra` tool
    /// calls is bounded at the cap — same defensive rule as the Claude
    /// / Codex parsers — so no single turn dominates the payload.
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

    /// Pure path builder split from the env-lookup wrapper so tests
    /// drive the resolve with a synthetic brain root instead of touching
    /// `~/.gemini`. Mirrors the cursor/codex split — the locator can
    /// validate `transcript.jsonl` → `transcript_full.jsonl` fallback
    /// under a tempdir without depending on the process-global
    /// `ANTIGRAVITY_HOME`.
    #[test]
    fn agy_locator_prefers_short_transcript_then_falls_back_to_full() {
        let suffix = std::process::id();
        let temp = std::env::temp_dir().join(format!(
            "buildmesh_test_agy_locator_{suffix}"
        ));
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

    // --- Grok transcript format (issue #1281) ---
    //
    // Grok Code stores per-session directories at
    //   ~/.grok/sessions/<percent-encoded-cwd>/<session-id>/
    // containing `summary.json`, `chat_history.jsonl`, and `updates.jsonl`.
    // `chat_history.jsonl` is the per-message conversation log (primary
    // transcript); `updates.jsonl` carries event-level telemetry (which the
    // issue pins as "graceful failure on unknown event types").

    /// A Grok fixture spanning user prompt → assistant tool call → tool result
    /// (skipped, like Claude tool_result echoes) → user follow-up → assistant
    /// final answer that names the blocking question. Plus an unknown event
    /// type (`command_status`) that must be silently dropped, never flagged
    /// as malformed.
    #[test]
    fn grok_contract_parses_tail_and_last_assistant_message() {
        let tail = read_tail_from_file(
            &fixture("grok_chat_history.jsonl"),
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
            &fixture("grok_chat_history.jsonl"),
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

    /// Unknown event types (e.g. `command_status`, `telemetry`) must be
    /// silently skipped — never flagged as malformed. Issue #1281 acceptance
    /// criterion: "graceful failure on unknown event types".
    #[test]
    fn grok_unknown_event_types_are_silently_skipped() {
        let lines = vec![
            r#"{"role":"command_status","status":"completed"}"#.to_string(),
            r#"{"role":"telemetry","latency_ms":42}"#.to_string(),
            r#"{"role":"user","content":"real prompt"}"#.to_string(),
            r#"{"role":"assistant","content":"real reply"}"#.to_string(),
            r#"{"role":"heartbeat","seq":7}"#.to_string(),
        ];
        let parsed = super::adapters::grok::parse_grok_turns(lines.into_iter(), 10);
        assert_eq!(parsed.turns.len(), 2);
        assert!(!parsed.saw_malformed, "unknown event types must not flag malformed");
        assert_eq!(parsed.last_assistant_message.as_deref(), Some("real reply"));
    }

    /// A recognized `role` with a malformed `content` field (the Claude code
    /// breakage analogue) degrades loudly as `ShapeChanged`, never as the
    /// quiet `Empty`. A missing `role` field is treated as an unknown event
    /// type per issue #1281 ("graceful failure on unknown event types") and
    /// is therefore silently skipped, not flagged.
    #[test]
    fn grok_renamed_role_field_degrades_to_shape_changed() {
        let lines = vec![
            // Recognized role + content shape we don't understand (a nested
            // object instead of string/array/null) — this IS a structural
            // break in the Grok format, so it must degrade loudly.
            r#"{"role":"assistant","content":{"unexpected":"object"}}"#.to_string(),
            r#"{"role":"assistant","content":42}"#.to_string(),
        ];
        let parsed = super::adapters::grok::parse_grok_turns(lines.into_iter(), 10);
        assert!(parsed.turns.is_empty());
        assert!(parsed.saw_malformed, "recognized role with wrong content type is malformed");
        assert_eq!(
            empty_or_shape_changed(parsed.saw_malformed),
            UnavailableReason::ShapeChanged
        );
    }

    /// Conversely, lines *without* a `role` field (or with an unrecognized
    /// `role`) are treated as unknown event types and silently skipped —
    /// the issue #1281 acceptance "graceful failure on unknown event types".
    #[test]
    fn grok_missing_role_field_is_skipped_not_flagged() {
        let lines = vec![
            r#"{"author":"assistant","blocks":[{"type":"text","text":"renamed"}]}"#.to_string(),
            r#"{"type":"command_status","status":"running"}"#.to_string(),
            r#"{"latency_ms":42,"transport":"stream"}"#.to_string(),
        ];
        let parsed = super::adapters::grok::parse_grok_turns(lines.into_iter(), 10);
        assert!(parsed.turns.is_empty(), "no recognized-role lines yields no turns");
        assert!(
            !parsed.saw_malformed,
            "unknown event types must NOT flag malformed"
        );
        assert_eq!(
            empty_or_shape_changed(parsed.saw_malformed),
            UnavailableReason::Empty
        );
    }

    /// A file whose only lines are non-message events is a genuinely-quiet
    /// session — `Empty`, not `ShapeChanged`.
    #[test]
    fn grok_only_unknown_events_degrade_to_empty() {
        let tail = read_tail_from_file(
            &fixture("grok_chat_history_empty.jsonl"),
            10,
            TranscriptFormat::Grok,
        );
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::Empty),
            "a turn-less file of unknown event types is Empty, not ShapeChanged"
        );
    }

    /// `parse_grok_turns` honours the contract: a `tail=1` request retains
    /// only the last turn but still tracks the last assistant message across
    /// the whole stream (issue #335 invariant).
    #[test]
    fn grok_rolling_buffer_retains_only_the_last_keep_turns() {
        let mut lines = Vec::new();
        for i in 0..50 {
            lines.push(format!(r#"{{"role":"user","content":"prompt {i}"}}"#));
            lines.push(format!(r#"{{"role":"assistant","content":"reply {i}"}}"#));
        }
        let parsed = super::adapters::grok::parse_grok_turns(lines.into_iter(), 3);
        assert_eq!(parsed.turns.len(), 3, "buffer never exceeds keep");
        assert_eq!(parsed.turns[2].text, "reply 49");
        assert_eq!(
            parsed.last_assistant_message.as_deref(),
            Some("reply 49"),
            "last assistant message survives eviction"
        );
    }

    /// Tool call `args` are run through `truncate_json_strings` so a single
    /// huge `args` body doesn't blow up the payload.
    #[test]
    fn grok_tool_call_args_are_truncated_through_truncate_json_strings() {
        let big = "x".repeat(MAX_TOOL_STRING + 50);
        let lines = vec![format!(
            r#"{{"role":"assistant","content":"with a big tool call","tool_calls":[{{"name":"Read","args":{{"file_path":"a","content":"{big}"}}}}]}}"#
        )];
        let parsed = super::adapters::grok::parse_grok_turns(lines.into_iter(), 10);
        assert_eq!(parsed.turns.len(), 1);
        let call = &parsed.turns[0].tool_calls[0];
        assert_eq!(call.name, "Read");
        let content = call.input["content"].as_str().unwrap();
        assert!(content.ends_with('…'), "large args body must be truncated");
    }

    /// `grok_locator_in` prefers `chat_history.jsonl` over `updates.jsonl`
    /// when both exist (chat_history is the primary conversation log).
    /// Layout: `<sessions_root>/<urlencoded-cwd>/<id>/{chat_history.jsonl,
    /// updates.jsonl}`. Passing an empty `node_path` leaves the cwd segment
    /// empty so the session id sits directly under `sessions_root`.
    #[test]
    fn grok_native_transcript_recovers_circuit_report() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp.path().join("F%3A%5Csrc%5Crepo%5C.claude%5Cworktrees%5Ctask").join("session-99");
        std::fs::create_dir_all(&session).unwrap();
        let file = session.join("chat_history.jsonl");
        std::fs::write(&file, concat!(
            "{\"type\":\"user\",\"content\":\"Fix the module\"}\n",
            "{\"type\":\"reasoning\",\"content\":\"Private reasoning\"}\n",
            "{\"type\":\"assistant\",\"content\":\"Module fixed. Tests passed.\"}\n",
            "{\"type\":\"tool_result\",\"content\":\"tool output\"}\n",
        )).unwrap();
        let path = super::adapters::grok::grok_locator_in(temp.path(), "session-99", r"F:\src\repo/.claude/worktrees/task").unwrap();
        let report = assistant_report_from_file(&path, TranscriptFormat::Grok).expect("completed Grok turn must be readable by the circuit");
        assert_eq!(report.text, "Module fixed. Tests passed.");
        use std::io::Write;
        let mut writer = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(writer, "{{\"type\":\"user\",\"content\":\"Next task\"}}").unwrap();
        writeln!(writer, "{{\"type\":\"tool_result\",\"content\":\"More tool output\"}}").unwrap();
        assert_eq!(assistant_report_from_file(&path, TranscriptFormat::Grok).unwrap().revision, report.revision);
        writeln!(writer, "{{\"type\":\"assistant\",\"content\":\"Module fixed. Tests passed.\"}}").unwrap();
        assert_ne!(assistant_report_from_file(&path, TranscriptFormat::Grok).unwrap().revision, report.revision);
    }

    #[test]
    fn grok_locator_recovers_native_windows_cwd_from_mixed_separators() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp.path().join("F%3A%5Csrc%5Crepo%5C.claude%5Cworktrees%5Ctask").join("session-99");
        std::fs::create_dir_all(&session).unwrap();
        let file = session.join("chat_history.jsonl");
        std::fs::write(&file, "{}\n").unwrap();
        assert_eq!(super::adapters::grok::grok_locator_in(temp.path(), "session-99", r"F:\src\repo/.claude/worktrees/task"), Some(file));
    }

    #[test]
    fn grok_locator_prefers_chat_history_over_updates() {
        let suffix = std::process::id();
        let temp = std::env::temp_dir().join(format!(
            "buildmesh_test_grok_locator_prefer_{suffix}"
        ));
        let session = temp.join("session-abc");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("chat_history.jsonl"), "{}").unwrap();
        std::fs::write(session.join("updates.jsonl"), "{}").unwrap();
        let found = super::adapters::grok::grok_locator_in(&temp, "session-abc", "");
        assert_eq!(
            found.as_deref(),
            Some(session.join("chat_history.jsonl").as_path()),
            "chat_history.jsonl wins when both exist"
        );
        std::fs::remove_dir_all(&temp).ok();
    }

    /// When `chat_history.jsonl` is absent, `grok_locator_in` falls back to
    /// `updates.jsonl` (event-level telemetry is still better than nothing).
    #[test]
    fn grok_locator_falls_back_to_updates_when_chat_history_missing() {
        let suffix = std::process::id();
        let temp = std::env::temp_dir().join(format!(
            "buildmesh_test_grok_locator_fallback_{suffix}"
        ));
        let session = temp.join("session-abc");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("updates.jsonl"), "{}").unwrap();
        let found = super::adapters::grok::grok_locator_in(&temp, "session-abc", "");
        assert_eq!(
            found.as_deref(),
            Some(session.join("updates.jsonl").as_path()),
            "updates.jsonl fallback when chat_history.jsonl missing"
        );
        std::fs::remove_dir_all(&temp).ok();
    }

    /// When neither file exists, the locator returns `None` so the reader
    /// degrades to `NoTranscript` — not an I/O error.
    #[test]
    fn grok_locator_returns_none_when_both_files_missing() {
        let suffix = std::process::id();
        let temp = std::env::temp_dir()
            .join(format!("buildmesh_test_grok_locator_none_{suffix}"));
        let session = temp.join("session-abc");
        std::fs::create_dir_all(&session).unwrap();
        assert!(super::adapters::grok::grok_locator_in(&temp, "session-abc", "").is_none());
        std::fs::remove_dir_all(&temp).ok();
    }

    // `for_harness` routes "grok" to the Grok format (issue #1281 acceptance
    // criterion: `TranscriptFormat::for_harness("grok")` returns the Grok
    // variant). The Claude-routed loop above excludes "grok" so the catch-all
    // `ClaudeCode` assertion cannot mask a future routing regression — the
    // explicit `for_harness("grok") == Grok` assertion in `routes_each_format`
    // pins that.

    /// `grok_urlencode_cwd` percent-encodes the cwd segment Grok uses as its
    /// session-directory name. Pin the scheme so a future refactor that
    /// silently drops (say) the colon encoding produces a compile-time test
    /// failure rather than silently misrouting sessions on Windows drives.
    #[test]
    fn grok_urlencode_cwd_test_pin() {
        // RFC 3986 unreserved set: ALPHA / DIGIT / "-" / "." / "_" / "~".
        // Everything else becomes %XX, uppercase hex (the form Grok emits).
        assert_eq!(
            super::adapters::grok::grok_urlencode_cwd(r"C:\Users\adam\src\buildmesh"),
            "C%3A%5CUsers%5Cadam%5Csrc%5Cbuildmesh",
            "Windows drive colon and backslashes must be percent-encoded so the \
             session-directory segment is filesystem-safe"
        );
        assert_eq!(
            super::adapters::grok::grok_urlencode_cwd("/home/adam/src/buildmesh"),
            "%2Fhome%2Fadam%2Fsrc%2Fbuildmesh",
            "POSIX slashes also percent-encoded"
        );
        // Unreserved per RFC 3986 stays literal; the locator only encodes
        // non-unreserved bytes.
        assert_eq!(
            super::adapters::grok::grok_urlencode_cwd("project-with_under.dots~tildas"),
            "project-with_under.dots~tildas",
            "RFC 3986 unreserved chars pass through unchanged"
        );
        assert_eq!(super::adapters::grok::grok_urlencode_cwd(""), "");
        assert_eq!(
            super::adapters::grok::grok_urlencode_cwd("with space"),
            "with%20space",
            "space encodes to %20, not '+' (RFC 3986, not form-style)"
        );
    }

    // --- OpenCode transcript format (issue #1296) ---
    //
    // The parser is tested directly over the public `opencode export <id>`
    // JSON shape (the checked-in fixture). The locator is tested separately
    // over a file-backed SQLite database whose schema matches the assumed
    // `message(id, session_id, time_created, data)` layout. Each test
    // uses a `tempfile::NamedTempFile` for RAII cleanup so a panic mid-test
    // leaves nothing behind in the OS temp dir. If OpenCode ever splits
    // parts into a separate table, the locator tests fail first.

    /// Parse the checked-in `opencode_export.json` fixture: every message's
    /// `text` parts surface on the matching turn, `tool` parts map to the
    /// shared `ToolCall` wire shape, `reasoning` parts are excluded from
    /// `Turn.text`, and unknown event types (`step-finish`) are silently
    /// dropped. The blocking question is the most recent assistant text —
    /// exactly the contract the Coordinator digest relies on.
    #[test]
    fn opencode_contract_parses_export_fixture() {
        let raw = std::fs::read_to_string(fixture("opencode_export.json"))
            .expect("opencode fixture should be readable");
        let value: serde_json::Value =
            serde_json::from_str(&raw).expect("opencode fixture should be valid JSON");
        let parsed = parse_opencode_export(&value, 20);
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = build_tail(parsed)
        else {
            panic!("expected available, got ShapeChanged or Empty");
        };

        // The fixture spans user → assistant tool → user → assistant
        // (text + reasoning + tool) → assistant (text + step-finish).
        // reasoning and step-finish must not surface as turns, so the
        // surviving sequence is [user, assistant, user, assistant, assistant].
        let roles: Vec<&str> = turns.iter().map(|t| t.role.as_str()).collect();
        assert_eq!(
            roles,
            vec!["user", "assistant", "user", "assistant", "assistant"],
            "turns: {turns:#?}"
        );
        assert_eq!(turns[0].text, "Inspect src/login.ts for the redirect bug.");
        // First assistant turn: text + tool call (no reasoning, no
        // step-finish).
        assert_eq!(turns[1].text, "I'll inspect the file first.");
        assert_eq!(turns[1].tool_calls.len(), 1);
        assert_eq!(turns[1].tool_calls[0].name, "read_file");
        assert_eq!(
            turns[1].tool_calls[0].input["file_path"],
            "src/login.ts",
            "OpenCode `state.input` is mapped onto the shared `input` wire shape"
        );
        // Second assistant turn: reasoning (skipped), text, tool call — the
        // reasoning block must NOT pollute Turn.text.
        assert_eq!(
            turns[3].text,
            "Found it — the redirect drops the query string. Shall I apply the fix?"
        );
        assert!(
            !turns[3].text.contains("URL"),
            "reasoning content must not leak into Turn.text, got: {}",
            turns[3].text
        );
        assert_eq!(turns[3].tool_calls[0].name, "search_replace");
        assert_eq!(
            turns[3].tool_calls[0].input["file_path"],
            "src/login.ts"
        );
        // Final assistant turn: text only, with a trailing step-finish part
        // that must be silently dropped.
        assert_eq!(
            turns[4].text,
            "The redirect now preserves the query string."
        );
        assert_eq!(turns[4].tool_calls.len(), 0);
        // The last assistant message is the whole-stream recovery — same
        // contract as every other parser.
        assert_eq!(
            last_assistant_message.as_deref(),
            Some("The redirect now preserves the query string."),
        );
    }

    /// **The digest-bug pin (issue #1296 review finding A).** When the latest
    /// SQLite row is a user reply, the digest must still surface the
    /// preceding assistant message as `last_assistant_message`. A
    /// pre-review implementation fetched exactly one row, so the user
    /// reply wiped the blocking question. The digest now reads the
    /// [DIGEST_WINDOW] rows (default 50) so the parser sees the assistant
    /// turn that preceded the user reply. We drive the pure
    /// `read_opencode_digest_from_messages` here; the env-coupled
    /// `read_opencode_digest` is exercised by the short-circuit test.
    #[test]
    fn opencode_digest_user_reply_at_latest_does_not_wipe_blocking_question() {
        let messages: Vec<serde_json::Value> = vec![
            serde_json::json!({
                "info": { "role": "assistant" },
                "parts": [{ "type": "text", "text": "Did the fix work?" }]
            }),
            // User reply at the latest row.
            serde_json::json!({
                "info": { "role": "user" },
                "parts": [{ "type": "text", "text": "Yes, ship it." }]
            }),
        ];
        let tail = read_opencode_digest_from_messages(&messages);
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = tail
        else {
            panic!(
                "digest must remain available when the latest row is a user reply; \
                 got {tail:?}"
            );
        };
        // Contract: digest returns Vec::new() — materialising the turn list
        // would defeat the bounded-memory optimisation.
        assert!(
            turns.is_empty(),
            "digest path must return turns: Vec::new(); got {turns:?}"
        );
        assert_eq!(
            last_assistant_message.as_deref(),
            Some("Did the fix work?"),
            "the digest must surface the assistant message BEFORE the user \
             reply — fetching only the latest row wipes the blocking question"
        );
    }

    /// **Pin for issue #1296 review finding B (digest contract):** the
    /// digest must NOT degrade as `Empty` when the window contains user
    /// turns but the agent hasn't responded yet. An actively-working
    /// node (user typed a prompt, agent is mid-thinking or
    /// `awaiting_input`) must be reported as `Available` so the
    /// Coordinator's spine fields (status, needs_feedback) can show
    /// the activity. `last_assistant_message: None` simply means
    /// "agent hasn't spoken yet in the polled window."
    #[test]
    fn opencode_digest_only_user_turns_returns_available_with_none() {
        let messages: Vec<serde_json::Value> = vec![
            serde_json::json!({
                "info": { "role": "user" },
                "parts": [{ "type": "text", "text": "fix the login redirect" }]
            }),
            serde_json::json!({
                "info": { "role": "user" },
                "parts": [{ "type": "text", "text": "are you there?" }]
            }),
        ];
        let tail = read_opencode_digest_from_messages(&messages);
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = tail
        else {
            panic!(
                "digest must remain Available when the agent hasn't spoken yet — \r
                 the Coordinator renders spine fields regardless of digest content; \r
                 got {tail:?}"
            );
        };
        assert!(turns.is_empty(), "digest path must return turns: Vec::new()");
        assert!(
            last_assistant_message.is_none(),
            "no assistant message in the window —> last_assistant_message: None"
        );
    }

    /// Reasoning-only assistant turns in the window are dropped from the
    /// turn list (same as every other parser), but their PRESENCE in
    /// the window means `parsed.turns` is non-empty — so the digest
    /// remains `Available` with `last_assistant_message: None` (since
    /// reasoning isn't dialogue). This is the same outcome as a node
    /// that has only user turns.
    #[test]
    fn opencode_digest_reasoning_only_assistant_with_user_turns_returns_available() {
        let messages: Vec<serde_json::Value> = vec![
            serde_json::json!({
                "info": { "role": "user" },
                "parts": [{ "type": "text", "text": "think about it" }]
            }),
            serde_json::json!({
                "info": { "role": "assistant" },
                "parts": [{ "type": "reasoning", "text": "pondering..." }]
            }),
            serde_json::json!({
                "info": { "role": "user" },
                "parts": [{ "type": "text", "text": "any answer?" }]
            }),
        ];
        let tail = read_opencode_digest_from_messages(&messages);
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = tail
        else {
            panic!(
                "reasoning-only assistant with user turns must NOT degrade — \
                 node is actively working; got {tail:?}"
            );
        };
        assert!(turns.is_empty());
        assert!(
            last_assistant_message.is_none(),
            "reasoning content is transport plumbing, not dialogue"
        );
    }

    /// The digest legitimately degrades when the window contains NO
    /// turns at all — an empty window from a brand-new session (before
    /// the user's first prompt) is `Empty`, and an all-malformed
    /// window is `ShapeChanged`. These are the two genuine Empty-
    /// degrade cases.
    #[test]
    fn opencode_digest_truly_empty_window_degrades_to_empty() {
        let messages: Vec<serde_json::Value> = vec![];
        let tail = read_opencode_digest_from_messages(&messages);
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::Empty),
            "an empty `messages` window represents a brand-new session that \
             hasn't reached its first user prompt"
        );
    }

    /// The full /log path also uses `read_opencode_tail_from_messages`
    /// once the DB read is done. Drive the pure function with a
    /// multi-message shape to confirm the rolling buffer keeps the
    /// tail turns in timeline order (matches `parse_opencode_messages`
    /// 's contract).
    #[test]
    fn opencode_tail_from_messages_keeps_rolling_tail_in_timeline_order() {
        let messages: Vec<serde_json::Value> = (0..50)
            .map(|i| {
                serde_json::json!({
                    "info": { "role": if i % 2 == 0 { "user" } else { "assistant" } },
                    "parts": [{ "type": "text", "text": format!("msg #{i}") }]
                })
            })
            .collect();
        let tail = read_opencode_tail_from_messages(&messages, 2);
        let TranscriptTail::Available { turns, .. } = tail else {
            panic!("expected available, got {tail:?}");
        };
        assert_eq!(turns.len(), 2);
        // Last two turns are user(48) and assistant(49).
        assert_eq!(turns[0].text, "msg #48");
        assert_eq!(turns[1].text, "msg #49");
    }

    /// A renamed `info.role` value is a structural break in the OpenCode
    /// message envelope — degrade loudly as `ShapeChanged`, not the quieter
    /// `Empty`. Issue #1296 acceptance: a busy node must never look quiet.
    #[test]
    fn opencode_renamed_role_field_degrades_to_shape_changed() {
        let value = serde_json::json!({
            "info": { "id": "ses_abc" },
            "messages": [
                { "info": { "role": "human" }, "parts": [] },
                { "info": { "role": "user", "content": "ok" }, "parts": [{"type": "text", "text": "hi"}] },
            ]
        });
        let parsed = parse_opencode_export(&value, 10);
        // The malformed message is flagged; the well-formed one still
        // surfaces. The malformed flag trips `build_tail`'s
        // `empty_or_shape_changed` only when no turns survive — with a
        // surviving turn, the result is `Available` carrying the recovered
        // turn + the `saw_malformed` flag is preserved for callers that
        // want to surface it. The contract test pins the surviving path;
        // the "all broken" path is pinned separately below.
        assert!(parsed.saw_malformed, "renamed role is malformed");
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].text, "hi");
    }

    /// When *every* message has a broken `info.role`, the parsed turn list
    /// is empty and `saw_malformed` is true — `build_tail` must degrade
    /// loudly, not as the quiet `Empty`.
    #[test]
    fn opencode_all_broken_messages_degrade_to_shape_changed() {
        let value = serde_json::json!({
            "info": { "id": "ses_abc" },
            "messages": [
                { "info": { "type": "user" }, "parts": [] }, // role missing
                { "info": { "role": "system" }, "parts": [] }, // role unknown
            ]
        });
        let tail = build_tail(parse_opencode_export(&value, 10));
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::ShapeChanged),
            "all-broken messages must degrade as ShapeChanged"
        );
    }

    /// A top-level export whose `messages` field is missing entirely is a
    /// structural break, not a quiet empty session — same degrade rule as
    /// the per-message rename case.
    #[test]
    fn opencode_missing_messages_field_degrades_to_shape_changed() {
        let value = serde_json::json!({ "info": { "id": "ses_abc" } });
        let tail = build_tail(parse_opencode_export(&value, 10));
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::ShapeChanged),
            "missing top-level `messages` is a structural break"
        );
    }

    /// An export whose `messages` array is empty is a genuinely-quiet
    /// session — `Empty`, not `ShapeChanged`. A brand-new OpenCode session
    /// legitimately has zero messages before the user types a prompt.
    #[test]
    fn opencode_empty_messages_array_degrades_to_empty() {
        let value = serde_json::json!({
            "info": { "id": "ses_abc" },
            "messages": []
        });
        let tail = build_tail(parse_opencode_export(&value, 10));
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::Empty),
            "zero messages is a quiet session, not a shape break"
        );
    }

    /// Unknown part types (`file`, `patch`, `agent`, `subtask`, …) must be
    /// silently dropped — same "graceful failure on unknown event types"
    /// rule Grok (#1281) follows. Never flagged as malformed.
    #[test]
    fn opencode_unknown_part_types_are_silently_skipped() {
        let value = serde_json::json!({
            "info": { "id": "ses_abc" },
            "messages": [
                {
                    "info": { "role": "assistant" },
                    "parts": [
                        { "type": "file", "url": "https://example.com/spec.md" },
                        { "type": "patch", "hash": "abc123" },
                        { "type": "agent", "source": { "value": "plan" } },
                        { "type": "step-start", "snapshot": "snap-1" },
                        { "type": "text", "text": "Real reply." },
                    ]
                }
            ]
        });
        let parsed = parse_opencode_export(&value, 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].text, "Real reply.");
        assert!(
            !parsed.saw_malformed,
            "unknown part types must not flag malformed, got: {parsed:?}"
        );
    }

    /// An assistant message whose only parts are reasoning (chain-of-thought)
    /// and tool calls with empty inputs is a no-op turn — drop it instead of
    /// surfacing an empty `assistant` turn. Mirrors Claude's thinking-only
    /// skip.
    #[test]
    fn opencode_reasoning_only_assistant_turn_is_dropped() {
        let value = serde_json::json!({
            "info": { "id": "ses_abc" },
            "messages": [
                {
                    "info": { "role": "assistant" },
                    "parts": [
                        { "type": "reasoning", "text": "I should think about this carefully." },
                    ]
                }
            ]
        });
        let tail = build_tail(parse_opencode_export(&value, 10));
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::Empty),
            "reasoning-only turn produces no dialogue, so the file is empty"
        );
    }

    /// `parse_opencode_messages` honours the rolling-buffer contract: a
    /// `keep=2` request retains only the last two turns but the
    /// `last_assistant_message` survives the eviction (issue #335 invariant
    /// — same as every other parser).
    #[test]
    fn opencode_rolling_buffer_retains_only_last_keep() {
        let mut messages = Vec::new();
        for i in 0..50 {
            messages.push(serde_json::json!({
                "info": { "role": "user" },
                "parts": [{"type": "text", "text": format!("prompt {i}")}]
            }));
            messages.push(serde_json::json!({
                "info": { "role": "assistant" },
                "parts": [{"type": "text", "text": format!("reply {i}")}]
            }));
        }
        let value = serde_json::json!({ "info": { "id": "ses_abc" }, "messages": messages });
        let parsed = parse_opencode_export(&value, 2);
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].text, "prompt 49");
        assert_eq!(parsed.turns[1].text, "reply 49");
        assert_eq!(
            parsed.last_assistant_message.as_deref(),
            Some("reply 49"),
            "last assistant message survives eviction of older turns"
        );
    }

    /// A tool call whose `state.input` carries a multi-MB string is bounded
    /// through the shared `truncate_json_strings` helper — same defensive
    /// rule as Claude / Codex / Command Code so a `write_file` body doesn't
    /// blow up the Coordinator payload.
    #[test]
    fn opencode_tool_input_truncates_large_string_leaves() {
        let big = "x".repeat(MAX_TOOL_STRING + 50);
        let value = serde_json::json!({
            "info": { "id": "ses_abc" },
            "messages": [{
                "info": { "role": "assistant" },
                "parts": [{
                    "type": "tool",
                    "state": {
                        "status": "completed",
                        "input": { "path": "src/main.rs", "content": big },
                        "output": "ok",
                        "title": "write_file"
                    }
                }]
            }]
        });
        let parsed = parse_opencode_export(&value, 10);
        let call = &parsed.turns[0].tool_calls[0];
        assert_eq!(call.name, "write_file");
        let content = call.input["content"].as_str().unwrap();
        assert!(content.ends_with('…'), "large args body must be truncated");
        assert!(content.chars().count() <= MAX_TOOL_STRING + 1);
    }

    /// A tool part whose `state.title` is missing falls back to a top-level
    /// `name` field — defensive breadth for an OpenCode version that hasn't
    /// yet populated `state.title`.
    #[test]
    fn opencode_tool_name_falls_back_to_part_name_when_state_title_missing() {
        let value = serde_json::json!({
            "info": { "id": "ses_abc" },
            "messages": [{
                "info": { "role": "assistant" },
                "parts": [{
                    "type": "tool",
                    "name": "bash",
                    "state": {
                        "status": "completed",
                        "input": { "command": "ls" },
                        "output": ""
                    }
                }]
            }]
        });
        let parsed = parse_opencode_export(&value, 10);
        assert_eq!(parsed.turns[0].tool_calls[0].name, "bash");
    }

    /// `for_harness` routes `"opencode"` to the OpenCode variant so the
    /// dispatch table in `read_tail` / `read_last_assistant_message` agrees
    /// with the harness id (issue #1296).
    #[test]
    fn opencode_transcript_format_for_harness_routes_opencode() {
        assert_eq!(
            TranscriptFormat::for_harness("opencode"),
            TranscriptFormat::OpenCode,
            "the dispatch table must include the OpenCode variant"
        );
    }

    // --- Locator (SQLite read) tests ---
    //
    // Each locator test opens a `tempfile::NamedTempFile` and inserts rows
    // via the production schema. RAII cleanup means a panic mid-test still
    // removes the file from the OS temp dir (no leaked files when CI runs
    // 50 tests in parallel and one panics).

    /// Build a file-backed SQLite matching the assumed `message` schema
    /// and insert the given `(time_created, role, raw_data_json)` rows
    /// (one `(1, "user", "...")` triplet per row). The function returns
    /// the temp file path; the test calls `read_opencode_messages(db_path,
    /// session_id, row_budget)` directly and pins the parsed shape.
    fn tempfile_opencode_db(rows: &[(i64, &str, &str)]) -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        let conn = rusqlite::Connection::open(tmp.path()).expect("open temp db");
        conn.execute_batch(
            "CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                data TEXT NOT NULL
            );",
        )
        .expect("create message table");
        for (idx, (time, _role, data)) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO message (id, session_id, time_created, data) \
                 VALUES (?1, 'ses_fixedsid000000000000000000001', ?2, ?3)",
                rusqlite::params![format!("row-{idx}"), time, data],
            )
            .expect("insert row");
        }
        // RAII: the connection drops when this function returns.
        drop(conn);
        tmp
    }

    /// Build the canonical `(role, text)` message JSON for a row payload.
    fn opencode_text_message(role: &str, text: &str) -> String {
        serde_json::json!({
            "info": { "role": role, "time": { "created": 1 } },
            "parts": [{ "type": "text", "text": text }]
        })
        .to_string()
    }

    /// The locator must pull rows for the requested session id and only
    /// that session — multi-session isolation. Drives the production
    /// path (`read_opencode_messages` → `parse_opencode_messages`) so a
    /// any regression in either layer surfaces here, not just in the
    /// parser in isolation.
    #[test]
    fn opencode_locator_reads_messages_from_file_backed_db() {
        let tmp = tempfile_opencode_db(&[
            (100, "user", &opencode_text_message("user", "Inspect src/login.ts.")),
            (200, "assistant", &opencode_text_message("assistant", "Looking now.")),
            // Row for a *different* session id — must not leak in. We
            // re-open and write to that session id below.
        ]);
        let conn = rusqlite::Connection::open(tmp.path()).expect("reopen for foreign row");
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, data) \
             VALUES ('foreign', 'ses_othersessionid0000000000001', 150, ?1)",
            rusqlite::params![opencode_text_message("user", "this should not surface")],
        )
        .expect("insert foreign row");
        drop(conn);

        let messages =
            read_opencode_messages(tmp.path(), "ses_fixedsid000000000000000000001", 10)
                .expect("locator should return a value");
        let parsed = parse_opencode_messages(&messages, 10);
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].role, "user");
        assert_eq!(parsed.turns[0].text, "Inspect src/login.ts.");
        assert_eq!(parsed.turns[1].role, "assistant");
        assert_eq!(parsed.turns[1].text, "Looking now.");
        assert_eq!(
            parsed.last_assistant_message.as_deref(),
            Some("Looking now.")
        );
        for turn in &parsed.turns {
            assert!(
                !turn.text.contains("this should not surface"),
                "session-id filter leaked a row from the other session id"
            );
        }
    }

    #[test]
    fn circuit_opencode_report_identity_survives_restart_and_identical_replies() {
        let session = "ses_fixedsid000000000000000000001";
        let tmp = tempfile_opencode_db(&[
            (100, "assistant", &opencode_text_message("assistant", "Done.")),
            (200, "user", &opencode_text_message("user", "Finish now")),
        ]);
        let first = opencode_assistant_report(tmp.path(), session).unwrap();
        assert_eq!(first.text, "Done.");
        let conn = rusqlite::Connection::open(tmp.path()).unwrap();
        conn.execute("INSERT INTO message (id, session_id, time_created, data) VALUES ('new-reply', ?1, 300, ?2)",
            rusqlite::params![session, opencode_text_message("assistant", "Done.")]).unwrap();
        drop(conn);
        let second = opencode_assistant_report(tmp.path(), session).unwrap();
        assert_eq!(second.text, first.text);
        assert_ne!(second.revision, first.revision);
        assert_eq!(opencode_assistant_report(tmp.path(), session).unwrap().revision, second.revision);
    }

    /// A session id that doesn't match any row is not an error — the
    /// locator returns an empty `messages` Vec and the parser degrades as
    /// `Empty`, never as `ShapeChanged`. The RAII handle cleans up the
    /// file on drop.
    #[test]
    fn opencode_locator_returns_empty_for_unknown_session() {
        let tmp = tempfile_opencode_db(&[(
            100,
            "user",
            &opencode_text_message("user", "hi"),
        )]);
        let messages = read_opencode_messages(tmp.path(), "ses_unknown0000000000000000000000001", 10)
            .expect("locator must not error when the session has no rows");
        let parsed = parse_opencode_messages(&messages, 10);
        assert_eq!(
            parsed,
            Parsed {
                turns: Vec::new(),
                last_assistant_message: None,
                saw_malformed: false,
            },
            "an unknown session id is a quiet session, not a shape break"
        );
    }

    /// A row whose `data` blob is not valid JSON is silently dropped —
    /// a single bad row doesn't break the whole session (graceful
    /// failure on bad rows, same defensive rule as
    /// `services::opencode_session`).
    #[test]
    fn opencode_locator_drops_malformed_rows() {
        let tmp = tempfile_opencode_db(&[
            (100, "user", "not valid json"),
            (200, "user", &opencode_text_message("user", "good")),
        ]);
        let messages = read_opencode_messages(tmp.path(), "ses_fixedsid000000000000000000001", 10)
            .expect("locator must not error on a bad row");
        let parsed = parse_opencode_messages(&messages, 10);
        assert_eq!(parsed.turns.len(), 1, "the malformed row must be skipped");
        assert_eq!(parsed.turns[0].text, "good");
    }

    /// The locator must return rows in chronological order (oldest →
    /// newest), reversing the DESC query the underlying SQL emits. The
    /// parser consumes ASC and tracks `last_assistant_message` across
    /// the *whole* fetched window, not just the bounded tail.
    #[test]
    fn opencode_locator_returns_rows_in_ascending_order() {
        let tmp = tempfile_opencode_db(&[
            (0, "user", &opencode_text_message("user", "first ever row")),
            (50_000, "assistant", &opencode_text_message("assistant", "early assistant")),
            (100_000, "user", &opencode_text_message("user", "near latest 2")),
            (150_000, "assistant", &opencode_text_message("assistant", "latest 1")),
            (200_000, "user", &opencode_text_message("user", "latest 2")),
        ]);
        let messages =
            read_opencode_messages(tmp.path(), "ses_fixedsid000000000000000000001", 3)
                .expect("locator must accept the bounded row_budget");
        assert_eq!(
            messages.len(),
            3,
            "locator must cap the row fetch at the parser's row_budget"
        );
        // SQL emits DESC LIMIT 3 → [latest 2 (200_000), latest 1 (150_000),
        // near latest 2 (100_000)]. After the Rust-side reverse the
        // natural timeline is restored: ascending time order.
        assert!(messages[0].to_string().contains("near latest 2"));
        assert!(messages[1].to_string().contains("latest 1"));
        assert!(messages[2].to_string().contains("latest 2"));
    }

    /// A `session_id` that isn't a `ses_…` id short-circuits before
    /// opening the DB — keeps a non-OpenCode session-id shape from
    /// triggering an avoidable I/O round trip. Distinct from
    /// `NoSession` (missing session_id), which corresponds to a
    /// supported-but-not-yet-captured node.
    #[test]
    fn opencode_locator_short_circuits_non_ses_ids() {
        let tail = read_opencode_tail(Some("not-an-opencode-id"), "/home/adam/src/proj", 10);
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::NoTranscript),
            "non-`ses_` ids must short-circuit before any disk read"
        );
        let tail = read_opencode_digest(Some("not-an-opencode-id"), "/home/adam/src/proj");
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::NoTranscript),
            "non-`ses_` ids must short-circuit on the digest path too"
        );
        // A missing session_id is the supported-provider-but-no-session
        // state, distinct from `NoTranscript`.
        let tail = read_opencode_tail(None, "/home/adam/src/proj", 10);
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::NoSession),
            "missing session id is NoSession, not NoTranscript"
        );
        let tail = read_opencode_digest(None, "/home/adam/src/proj");
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::NoSession),
            "missing session id is NoSession on the digest path too"
        );
    }
}


