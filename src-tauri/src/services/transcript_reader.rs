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
use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::env;
use crate::models::EnvType;
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
/// Encode a filesystem path the way Command Code does for its
/// `~/.commandcode/projects/<slug>` directory names: lowercase, replace every
/// non-alphanumeric character with `-`, collapse consecutive `-` runs, and
/// trim leading/trailing `-` (issue #1500).
///
/// For example `F:\src\buildmesh\.claude\worktrees\foo` becomes
/// `f-src-buildmesh-claude-worktrees-foo`, and `/home/user/project` becomes
/// `home-user-project`. This matches the on-disk layout observed in Command
/// Code v1.43.0 and the `c-users-user` / `home-...` slugs reported upstream.
/// Pass the CLI cwd form; for raw paths that may be WSL UNC
/// (`\\wsl$\...`), normalize with `env::normalize_unc_to_wsl` first.
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
    // File-based formats only. OpenCode's per-session data lives in a
    // shared SQLite DB rather than a transcript file (issue #1296), so
    // `read_tail` / `read_last_assistant_message` short-circuit before
    // this dispatch runs. The wildcard keeps the match exhaustive when
    // new file-based formats are added later.
    match format {
        TranscriptFormat::ClaudeCode => Some(transcript_path(session_id, node_path)),
        TranscriptFormat::Cursor => Some(cursor_transcript_path(session_id, node_path)),
        TranscriptFormat::Codex => find_codex_rollout(session_id),
        TranscriptFormat::Agy => find_agy_transcript(session_id),
        TranscriptFormat::Grok => find_grok_transcript(session_id, node_path),
        TranscriptFormat::CommandCode => find_commandcode_transcript(session_id, node_path),
        _ => None,
    }
}

/// Find the on-disk JSONL for an AGY conversation. AGY keeps the
/// token-efficient `transcript.jsonl` first and the untruncated
/// `transcript_full.jsonl` as a fallback (issue #1283); both live at
/// `<brain_dir>/<conversation-id>/.system_generated/logs/`. The session id
/// itself is the `conversation-id` and the brain root comes from
/// `env::agy_brain_dir()` — globally keyed (not project-scoped).
fn find_agy_transcript(session_id: &str) -> Option<PathBuf> {
    agy_locator_in(&env::agy_brain_dir(), session_id)
}

/// Pure AGY locator — split from [`find_agy_transcript`] so the contract
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

/// The host-accessible Command Code session directory for an agent
/// environment: `<home>/.commandcode/projects/<encoded-cwd>/` (issue #1500).
/// Mirrors [`transcript_path`] (Claude Code): the transcript-format module
/// composes the slug, while `env` owns the host-accessible projects base
/// (including WSL translation). `spawn_path` is the CLI cwd form; raw WSL UNC
/// paths are normalized first so `\\wsl$\<distro>\home\user\repo` resolves to
/// the same slug the in-WSL CLI wrote (`home-user-repo`).
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

/// Find the Command Code session transcript for the node's runtime
/// environment. Command Code stores one file per session under
/// `<commandcode-home>/projects/<encoded-cwd>/<session-id>.jsonl` (issue
/// #1500); WSL homes are converted to host-readable paths by the shared
/// environment path module.
fn find_commandcode_transcript(session_id: &str, node_path: &str) -> Option<PathBuf> {
    let env_type = EnvType::from(env::env_for_path(Path::new(node_path)));
    let sessions_dir = commandcode_sessions_dir(env_type, node_path)?;
    let path = commandcode_transcript_path_in(&sessions_dir, session_id);
    path.exists().then_some(path)
}

/// Pure Command Code locator used by the contract test and kept separate from
/// process-global home/environment discovery.
pub(crate) fn commandcode_transcript_path_in(
    sessions_root: &Path,
    session_id: &str,
) -> PathBuf {
    sessions_root.join(format!("{session_id}.jsonl"))
}
/// Build the expected on-disk path of a Claude Code session transcript:
/// `<claude_dir>/projects/<encoded node_path>/<session_id>.jsonl`.
fn transcript_path(session_id: &str, node_path: &str) -> PathBuf {
    env::claude_dir()
        .join("projects")
        .join(encode_path(node_path))
        .join(format!("{session_id}.jsonl"))
}

/// Build the expected on-disk path of a Cursor CLI session transcript:
/// `<cursor_dir>/projects/<workspace-slug>/agent-transcripts/<session>/<session>.jsonl`.
fn cursor_transcript_path(session_id: &str, node_path: &str) -> PathBuf {
    cursor_transcript_path_in(&env::cursor_dir(), session_id, node_path)
}

/// Pure path builder for Cursor transcripts, split from the environment lookup
/// so the workspace layout can be tested without process-global state.
pub(crate) fn cursor_transcript_path_in(
    cursor_home: &Path,
    session_id: &str,
    node_path: &str,
) -> PathBuf {
    cursor_home
        .join("projects")
        .join(cursor_workspace_slug(node_path))
        .join("agent-transcripts")
        .join(session_id)
        .join(format!("{session_id}.jsonl"))
}

/// Convert a workspace path into Cursor's lossy project directory slug.
/// Cursor drops a leading separator, removes a Windows drive colon, and uses
/// dashes for path separators and other non-alphanumeric characters.
pub(crate) fn cursor_workspace_slug(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let mut parts = normalized
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();

    if let Some(first) = parts.first_mut() {
        if first.len() == 2 && first.as_bytes()[1] == b':' {
            first.truncate(1);
            first.make_ascii_lowercase();
        }
    }

    parts
        .into_iter()
        .map(|part| {
            part.chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("-")
}

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

/// Fixed-row window for the Coordinator digest path. Chosen to be wide
/// enough to span a typical user → assistant exchange (a few turns each
/// with reasoning + tool + text parts) but bounded so a 10k-row session
/// doesn't full-scan on every poll. Tuned so the digest finds the blocking
/// question even when the latest row is a user reply.
const OPENCODE_DIGEST_WINDOW: usize = 50;

/// Row-to-turn factor for the full /log read path. OpenCode rows are
/// message events, not turns — assistant turn coalescing, reasoning-only
/// drop, and tool-only drop mean `factor` rows typically produce 1 turn.
/// Factor > 1 ensures the caller can always extract `tail` turns once the
/// parser has coalesced/dropped, even on dense conversations.
const OPENCODE_TURN_TO_MESSAGE_FACTOR: usize = 3;

/// SQLite busy_timeout the OpenCode reader applies on every open: lets a
/// concurrent writer (the live OpenCode CLI) hold the lock briefly instead
/// of returning `SQLITE_BUSY` to a Coordinator poll. 500 ms is generous
/// enough for a single write transaction and short enough that a stuck
/// writer doesn't stall the digest path.
const OPENCODE_READER_BUSY_TIMEOUT_MS: u64 = 500;

/// Read the row tail of an OpenCode session's `message` table. Returns the
/// up-to-`row_budget` newest rows in chronological order (oldest → newest),
/// matching how `opencode export <id>` orders them. Returns `None` on any
/// I/O or query failure so callers can degrade to `Unreadable`.
///
/// `Limit` is bound as a real SQL parameter (`?2`) — no `format!` SQL,
/// even though `limit` is server-controlled. The shape matches the rest
/// of the reader's prepared statements and stays parameter-bound for the
/// case where the upstream schema adds a filter column we don't control
/// yet.
fn read_opencode_messages(
    db_path: &Path,
    session_id: &str,
    row_budget: usize,
) -> Option<Vec<serde_json::Value>> {
    use rusqlite::{Connection, OpenFlags};
    use std::time::Duration;
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    if let Err(error) = conn.busy_timeout(Duration::from_millis(OPENCODE_READER_BUSY_TIMEOUT_MS)) {
        // Busytimeout is best-effort — log but don't fail the read;
        // a non-zero busy_timeout simply means concurrent writers will
        // surface as SQLITE_BUSY (the previous behaviour).
        tracing::debug!(
            "opencode transcript reader: busy_timeout set failed ({error}); \
             concurrent writes may degrade to Unreadable"
        );
    }
    let mut stmt = conn
        .prepare(
            "SELECT data FROM message \
             WHERE session_id = ?1 \
             ORDER BY time_created DESC \
             LIMIT ?2",
        )
        .ok()?;
    let mut latest: Vec<serde_json::Value> = Vec::new();
    let mut rows = stmt
        .query(rusqlite::params![session_id, row_budget as i64])
        .ok()?;
    while let Some(row) = rows.next().ok()? {
        let data: String = row.get(0).ok()?;
        // Each row's `data` is one message record. We accept any JSON shape
        // here — structural validation lives in the parser so an unknown
        // shape degrades as `ShapeChanged`, not a panic. Rows that aren't
        // valid JSON are silently dropped (graceful failure on bad rows).
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) {
            latest.push(value);
        }
    }
    // The query returned DESC; the parser consumes ASC (matches the
    // natural timeline that `opencode export` orders by). Reverse once
    // here so the parser never has to know about the bound direction.
    latest.reverse();
    Some(latest)
}

/// Pure tail reader over a Vec of OpenCode message values. Splits
/// parse + result shaping so tests can drive the dispatch (env path
/// resolution) and the parsing semantics independently — same split the
/// `agy_locator_in` / `agy_locator` pair uses for the AGY adapter. The
/// `tail` argument flows through `effective_tail` to the parser's
/// rolling-buffer keep.
pub(crate) fn read_opencode_tail_from_messages(
    messages: &[serde_json::Value],
    tail: usize,
) -> TranscriptTail {
    build_tail(parse_opencode_messages(messages, effective_tail(tail)))
}

/// Pure digest reader. Always returns `turns: Vec::new()` so the
/// digest consumer's bounded-memory optimisation holds for OpenCode
/// (issue #1296 review finding A — a pre-review implementation
/// returned `Vec<Turn>` from this path and broke the digest contract).
/// `last_assistant_message` comes from the parser's whole-stream
/// tracker; a session with no surviving turns degrades as `Empty` /
/// `ShapeChanged`.
pub(crate) fn read_opencode_digest_from_messages(
    messages: &[serde_json::Value],
) -> TranscriptTail {
    let parsed = parse_opencode_messages(messages, 1);
    // The digest's contract: only return `Available` when the window
    // contained at least one assistant message — the Coordinator polls
    // many nodes and `last_assistant_message: None` is indistinguishable
    // from "node hasn't spoken yet". A session of only user prompts
    // degrades as `Empty`; an all-malformed window degrades as
    // `ShapeChanged`.
    let Some(last) = parsed.last_assistant_message else {
        return TranscriptTail::unavailable(empty_or_shape_changed(parsed.saw_malformed));
    };
    TranscriptTail::Available {
        turns: Vec::new(),
        last_assistant_message: Some(last),
    }
}

/// Open the OpenCode SQLite DB and read the latest `tail` turns (with a
/// `factor` row budget to span assistant coalescing) for a session_id.
/// Wraps [`read_opencode_tail_from_messages`] over the rows the locator
/// returns, and degrades through the same [`UnavailableReason`] ladder
/// as every other harness so the Coordinator's `/nodes/{id}/log`
/// endpoint sees a uniform error surface.
pub(crate) fn read_opencode_tail(
    session_id: Option<&str>,
    node_path: &str,
    tail: usize,
) -> TranscriptTail {
    let _session_id = session_id;
    let Some((db_path, session_id)) = opencode_resolve(session_id, node_path) else {
        return TranscriptTail::unavailable(opencode_unavailable_reason(
            _session_id.filter(|s| !s.is_empty()).map(str::to_string),
        ));
    };
    let row_budget = effective_tail(tail).saturating_mul(OPENCODE_TURN_TO_MESSAGE_FACTOR);
    let Some(messages) = read_opencode_messages(&db_path, &session_id, row_budget) else {
        return TranscriptTail::unavailable(UnavailableReason::Unreadable);
    };
    read_opencode_tail_from_messages(&messages, tail)
}

/// Coordinator digest read path (`GET /nodes`). Resolves the env-aware
/// DB path, fetches a fixed-size window (independent of the caller-
/// supplied `tail`, which is ignored on every digest path), and shapes
/// the result through [`read_opencode_digest_from_messages`]. Mirrors
/// the AGY split: test the semantics with explicit messages, drive the
/// dispatch through this.
pub(crate) fn read_opencode_digest(
    session_id: Option<&str>,
    node_path: &str,
) -> TranscriptTail {
    let _session_id = session_id;
    let Some((db_path, session_id)) = opencode_resolve(session_id, node_path) else {
        return TranscriptTail::unavailable(opencode_unavailable_reason(
            _session_id.filter(|s| !s.is_empty()).map(str::to_string),
        ));
    };
    let Some(messages) =
        read_opencode_messages(&db_path, &session_id, OPENCODE_DIGEST_WINDOW)
    else {
        return TranscriptTail::unavailable(UnavailableReason::Unreadable);
    };
    read_opencode_digest_from_messages(&messages)
}

/// Shared session-id + DB-path resolver for both OpenCode read paths.
/// Returns `Some((PathBuf, String))` on success; `None` on NoSession /
/// NoTranscript, with `session_id: None` on the error path so the caller
/// can map back to the right [`UnavailableReason`] via
/// [`opencode_unavailable_reason`].
fn opencode_resolve(
    session_id: Option<&str>,
    node_path: &str,
) -> Option<(PathBuf, String)> {
    let session_id = session_id.filter(|s| !s.is_empty())?.to_string();
    if !crate::services::opencode_session::is_opencode_session_id(&session_id) {
        // A non-`ses_` id cannot match any OpenCode row. Degrade quietly
        // rather than opening the DB to find nothing. The gate is shared
        // with `services::opencode_session::is_opencode_session_id` so
        // the two readers (transcript + capture poller) cannot drift on
        // what an OpenCode session id looks like.
        return None;
    }
    let env_type = EnvType::from(env::env_for_path(Path::new(node_path)));
    let db_path = crate::services::opencode_session::opencode_db_path(env_type)?;
    if !db_path.exists() {
        return None;
    }
    Some((db_path, session_id))
}

/// Map a missing-input outcome from [`opencode_resolve`] back to the
/// right [`UnavailableReason`]. `Some(id)` means the session id was
/// present (and `OpenCode`-shaped) but the DB path was missing →
/// `NoTranscript`. `None` means the session id was absent →
/// `NoSession`.
fn opencode_unavailable_reason(session_id: Option<String>) -> UnavailableReason {
    match session_id {
        Some(_) => UnavailableReason::NoTranscript,
        None => UnavailableReason::NoSession,
    }
}

/// Parse a slice of OpenCode message envelopes into the shared [`Parsed`]
/// contract: rolling `keep`-bounded turn window, whole-stream
/// last-assistant-message tracking, malformed-flag so a renamed-field
/// message degrades as `ShapeChanged`. Maps each message's `parts` array
/// onto text + tool calls:
///
/// - `text` parts → concatenated into `Turn.text`
/// - `reasoning` parts → silently dropped (chain-of-thought, not dialogue)
/// - `tool` parts → converted to [`ToolCall`]s using `state.input` and
///   `state.title` as the tool name (falling back to the part's `name`)
/// - `step-start` / `step-finish` / unknown parts → silently skipped, never
///   flagged (the "graceful failure on unknown event types" rule)
///
/// A user turn with no `text` parts, or an assistant turn with neither
/// text nor tool calls, is dropped — same as Claude's thinking-only line.
pub(crate) fn parse_opencode_messages(
    messages: &[serde_json::Value],
    keep: usize,
) -> Parsed {
    let keep = keep.max(1);
    let mut turns: VecDeque<Turn> = VecDeque::new();
    let mut last_assistant_message: Option<String> = None;
    let mut saw_malformed = false;

    for message in messages {
        // Each message envelope is `{"info": {role, ...}, "parts": [...]}`. A
        // missing `info.role` is a structural break on a recognized message
        // shape — flag malformed. A missing `parts` is empty (no text, no
        // tool calls); the message is then dropped as a no-op.
        let Some(info) = message.get("info") else {
            saw_malformed = true;
            continue;
        };
        let Some(role) = info.get("role").and_then(|r| r.as_str()) else {
            saw_malformed = true;
            continue;
        };
        if role != "user" && role != "assistant" {
            // Unknown role on a recognized envelope — flag as malformed so a
            // future "system" or "tool" role doesn't silently degrade.
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
            // Empty user prompts (e.g. a file-only attachment with no text)
            // are dropped — mirrors Claude's empty `user` line rule.
            if text.trim().is_empty() {
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

        // Assistant turn: drop thinking-only (text empty AND no tool calls).
        if text.trim().is_empty() && tool_calls.is_empty() {
            continue;
        }
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
    }

    Parsed {
        turns: turns.into(),
        last_assistant_message,
        saw_malformed,
    }
}

/// Parse the on-disk export JSON shape (`{info, messages}`) used by the
/// fixture + the `opencode export <id>` CLI. Pure: passes the `messages`
/// slice straight to [`parse_opencode_messages`]. A missing `messages`
/// array is a structural break — `ShapeChanged`, not `Empty`. An empty
/// `messages` array is a brand-new session — `Empty`.
///
/// **Test-only helper** — the production runtime path uses
/// [`read_opencode_messages`] (which returns `Vec<serde_json::Value>`
/// directly, with no envelope) and feeds that to
/// [`parse_opencode_messages`] without going through this wrapper. The
/// fixture + parser contract tests are the only callers; cargo's
/// `dead_code` analysis doesn't see `#[cfg(test)]` use-sites in some
/// versions, hence the `#[allow(dead_code)]`.
#[allow(dead_code)]
pub(crate) fn parse_opencode_export(
    export: &serde_json::Value,
    keep: usize,
) -> Parsed {
    match export.get("messages").and_then(|m| m.as_array()) {
        Some(messages) => parse_opencode_messages(messages, keep),
        None => Parsed {
            turns: Vec::new(),
            last_assistant_message: None,
            saw_malformed: true,
        },
    }
}

/// Concatenate the `text` parts of an OpenCode message, separated by
/// newlines. Reasoning parts are deliberately excluded — chain-of-thought is
/// transport plumbing, not Coordinator dialogue (matches Claude's
/// `thinking`-block skip). Returns the empty string when no `text` parts
/// exist (an assistant turn then degrades to "tool calls only").
fn concat_opencode_text_parts(parts: &[serde_json::Value]) -> String {
    parts
        .iter()
        .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Pull `tool` parts out of an OpenCode message into the shared
/// [`ToolCall`] wire shape (`{name, input}`). The OpenCode export places the
/// tool name on `state.title` (e.g. `"read_file"`, `"search_replace"`) and
/// the raw input on `state.input`; output lives on `state.output` but the
/// Coordinator only consumes `input` — output re-emission is a future
/// harness-shape addition. Unknown part types (`file`, `patch`, `agent`, …)
/// are silently dropped, mirroring Grok's "graceful failure on unknown event
/// types" rule (#1281).
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
            let input = state.get("input").cloned().unwrap_or(serde_json::Value::Null);
            Some(ToolCall {
                name,
                input: truncate_json_strings(input, MAX_TOOL_STRING),
            })
        })
        .collect()
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
    // JSONL pipeline only. OpenCode never reaches this dispatch — see
    // the comment on `locate_transcript`. The wildcard arm returns an
    // empty `Parsed` (no turns, no digest message) so a future file-based
    // harness that ends up routed here in error degrades as `Empty`,
    // not a panic.
    match format {
        TranscriptFormat::ClaudeCode | TranscriptFormat::Cursor => parse_turns(lines, keep),
        TranscriptFormat::Codex => parse_codex_turns(lines, keep),
        TranscriptFormat::Agy => parse_agy_turns(lines, keep),
        TranscriptFormat::Grok => parse_grok_turns(lines, keep),
        TranscriptFormat::CommandCode => parse_commandcode_turns(lines, keep),
        _ => Parsed {
            turns: Vec::new(),
            last_assistant_message: None,
            saw_malformed: false,
        },
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
#[derive(Debug, PartialEq)]
pub(crate) struct Parsed {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandCodeMessageActivity {
    UserTurn,
    ToolUse,
    ToolResult,
    AssistantResponse,
}

/// Classify a Command Code message payload using the canonical text and tool
/// extraction rules. Empty, synthetic, tool-result, thinking, and reasoning
/// records are classified separately so the watcher can clear pending tool
/// calls, while the digest parser still omits them from normalized turns.
pub(crate) fn commandcode_message_activity(
    message: &serde_json::Value,
) -> Option<CommandCodeMessageActivity> {
    let role = message.get("role")?.as_str()?;
    let content = message.get("content")?;
    let text = concat_text_blocks(Some(content));
    let tool_calls = extract_tool_calls(Some(content));

    match role {
        "user" if contains_tool_result(content) => Some(CommandCodeMessageActivity::ToolResult),
        "user" if is_synthetic_message(&text) || text.trim().is_empty() => None,
        "user" => Some(CommandCodeMessageActivity::UserTurn),
        "assistant" if !tool_calls.is_empty() => Some(CommandCodeMessageActivity::ToolUse),
        "assistant" if !text.trim().is_empty() => Some(CommandCodeMessageActivity::AssistantResponse),
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

/// Parse Command Code JSONL lines into normalized turns. The rolling buffer,
/// assistant digest, malformed-shape signal, and assistant-id coalescing all
/// follow the shared transcript-reader contract used by Claude Code/Cursor.
fn parse_commandcode_turns(lines: impl Iterator<Item = String>, keep: usize) -> Parsed {
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
        // recognized message envelope with a broken payload is different: it
        // should degrade to ShapeChanged when no usable turns remain.
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
        let mut tool_calls = extract_tool_calls(Some(raw_content));

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

/// Parse Antigravity JSONL lines into logical turns, honouring the same
/// [`Parsed`] contract as [`parse_turns`] (rolling `keep`-bounded turn window,
/// whole-stream last-assistant-message tracking, malformed flag). Each line
/// is one self-contained turn — no message-id coalescing is needed for AGY
/// because its emission shape never splits a single assistant message across
/// multiple JSONL lines.
fn parse_agy_turns(lines: impl Iterator<Item = String>, keep: usize) -> Parsed {
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
                        text: truncate(text, MAX_TURN_TEXT),
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
                let text = val
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                let mut tool_calls = extract_agy_tool_calls(val.get("tool_calls"));
                if text.trim().is_empty() && tool_calls.is_empty() {
                    // thinking-only turn — nothing the Coordinator can use.
                    continue;
                }
                cap_tool_calls(&mut tool_calls);
                let turn = Turn {
                    role: "assistant".to_string(),
                    text: truncate(text, MAX_TURN_TEXT),
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

/// Synthetic AGY user injections — the AGY equivalent of Claude Code's
/// `<local-command-caveat>` wrapper. Today's transcripts don't carry
/// any of these (issue #1283 research); the predicate is a forward-
/// compat shim so an environment-injected row never masquerades as a
/// real user prompt.
fn is_agy_synthetic(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<local-command-caveat>")
        || t.starts_with("<system>")
        || t.starts_with("<environment_context>")
        || t.starts_with("<task-notification>")
}

/// Pull AGY `tool_calls` (`{name, args}`) into the same [`ToolCall`] wire
/// shape Claude / Codex emit: `{name, input}`. Each `args` object's
/// string leaves are truncated via the shared [`truncate_json_strings`]
/// helper so a `run_command` carrying a multi-megabyte body doesn't blow
/// up the payload while the args *structure* is still delivered raw.
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

/// Resolve a Grok session directory to its primary transcript file.
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

/// Wrapper that mixes the env lookup back in. Env-keyed so a process-global
/// `GROK_HOME` override reaches the reader without a signature change.
fn find_grok_transcript(session_id: &str, node_path: &str) -> Option<PathBuf> {
    grok_locator_in(&env::grok_dir().join("sessions"), session_id, node_path)
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
fn parse_grok_turns(lines: impl Iterator<Item = String>, keep: usize) -> Parsed {
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
        let parsed = parse_grok_turns(lines.into_iter(), 10);
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
        let parsed = parse_grok_turns(lines.into_iter(), 10);
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
        let parsed = parse_grok_turns(lines.into_iter(), 10);
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
        let parsed = parse_grok_turns(lines.into_iter(), 3);
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
        let parsed = parse_grok_turns(lines.into_iter(), 10);
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
    fn grok_locator_prefers_chat_history_over_updates() {
        let suffix = std::process::id();
        let temp = std::env::temp_dir().join(format!(
            "buildmesh_test_grok_locator_prefer_{suffix}"
        ));
        let session = temp.join("session-abc");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("chat_history.jsonl"), "{}").unwrap();
        std::fs::write(session.join("updates.jsonl"), "{}").unwrap();
        let found = grok_locator_in(&temp, "session-abc", "");
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
        let found = grok_locator_in(&temp, "session-abc", "");
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
        assert!(grok_locator_in(&temp, "session-abc", "").is_none());
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
    fn grok_urlencode_cwd_matches_rfc3986_unreserved_only() {
        // RFC 3986 unreserved set: ALPHA / DIGIT / "-" / "." / "_" / "~".
        // Everything else becomes %XX, uppercase hex (the form Grok emits).
        assert_eq!(
            grok_urlencode_cwd(r"C:\Users\adam\src\buildmesh"),
            "C%3A%5CUsers%5Cadam%5Csrc%5Cbuildmesh",
            "Windows drive colon and backslashes must be percent-encoded so the \
             session-directory segment is filesystem-safe"
        );
        assert_eq!(
            grok_urlencode_cwd("/home/adam/src/buildmesh"),
            "%2Fhome%2Fadam%2Fsrc%2Fbuildmesh",
            "POSIX slashes also percent-encoded"
        );
        // Unreserved per RFC 3986 stays literal; the locator only encodes
        // non-unreserved bytes.
        assert_eq!(
            grok_urlencode_cwd("project-with_under.dots~tildas"),
            "project-with_under.dots~tildas",
            "RFC 3986 unreserved chars pass through unchanged"
        );
        assert_eq!(grok_urlencode_cwd(""), "");
        assert_eq!(
            grok_urlencode_cwd("with space"),
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

    /// The digest also handles the "no assistant message in the window" case
    /// — degrades as `Empty`, not `ShapeChanged` or `Unreadable`. A busy
    /// node that has only seen user prompts must not flip to a structural
    /// break degrade.
    #[test]
    fn opencode_digest_no_assistant_message_degrades_to_empty() {
        let messages: Vec<serde_json::Value> = vec![
            serde_json::json!({
                "info": { "role": "user" },
                "parts": [{ "type": "text", "text": "hi" }]
            }),
            serde_json::json!({
                "info": { "role": "user" },
                "parts": [{ "type": "text", "text": "are you there?" }]
            }),
        ];
        let tail = read_opencode_digest_from_messages(&messages);
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::Empty),
            "a session with no assistant turns is a quiet session, not a structural break"
        );
    }

    /// The digest path excludes reasoning-only assistant turns from
    /// `last_assistant_message` — reasoning is transport plumbing, not
    /// Coordinator dialogue. A session whose latest assistant rows are
    /// reasoning-only still degrades as `Empty`.
    #[test]
    fn opencode_digest_reasoning_only_assistant_turns_degrade_to_empty() {
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
        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::Empty),
            "reasoning-only assistant rows must not surface as last_assistant_message"
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
