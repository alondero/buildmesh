//! Format-agnostic envelope types + helpers shared by every
//! `TranscriptAdapter` (issue #1661). No harness knowledge lives here;
//! per-format locators, parsers, and line predicates move into
//! `crate::services::transcript_reader::adapters::*`.
//
// Issue #340 history: `truncate` lives here (not in a per-adapter file or in
// `transcript_paths`) so the two callers (`transcript_reader` +
// `agent_node_discovery`) cannot drift on the truncation rule the way they
// did before it was centralised.

use std::collections::VecDeque;
use serde::Serialize;

// --- Caps and constants ---
/// Per-turn text cap. Generous (this is the deep drill-in, not the scan) but
/// bounded so a single huge assistant message can't dominate the payload.
pub const MAX_TURN_TEXT: usize = 4000;
/// Per-turn tool-call cap. Within one `message.id` the count is naturally small
/// (parallel calls usually span separate message ids → separate turns), so this
/// is defensive only — but it honours the same "no single turn dominates the
/// payload" intent as [`MAX_TURN_TEXT`] (issue #335). Generous so a real turn is
/// never clipped; a turn that hits it was already pathological.
pub const MAX_TURN_TOOL_CALLS: usize = 50;
/// Cap applied to every string leaf inside a tool call's raw `input`, so a
/// `Write` carrying a whole file body doesn't blow up the response while the
/// input's *structure* is still delivered raw.
pub const MAX_TOOL_STRING: usize = 1000;
/// Default tail length when the caller supplies none.
pub const DEFAULT_TAIL: usize = 20;
/// Hard ceiling on the caller-supplied tail, so a `?tail=100000` can't ask the
/// reader to hold an unbounded transcript in memory.
pub const MAX_TAIL: usize = 200;

// --- Format-agnostic helpers ---

/// Truncate to at most `max` *bytes* (the right unit for bounding payload
/// size), appending `…` if cut. Respects UTF-8 boundaries so we never split a
/// multi-byte character — for non-ASCII text the result is therefore fewer than
/// `max` characters. `pub(crate)` so the sibling Claude-Code JSONL consumers
/// (`agent_node_discovery`, formerly `session_discovery`) share one truncation
/// rule — divergence here is how the two copies of this fn used to drift
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
pub(crate) fn truncate_json_strings(value: serde_json::Value, max: usize) -> serde_json::Value {
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

/// Effective tail length: `0` → default, otherwise clamp to the ceiling.
pub(crate) fn effective_tail(tail: usize) -> usize {
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
    pub(super) turns: Vec<Turn>,
    pub(super) last_assistant_message: Option<String>,
    pub(super) saw_malformed: bool,
}

/// The degrade reason for a file that opened but yielded no turns: `ShapeChanged`
/// (loud) when a malformed message line proves the recognizable shape is gone,
/// else `Empty` (quiet) for a genuinely-new/quiet session (issue #335).
pub(crate) fn empty_or_shape_changed(saw_malformed: bool) -> UnavailableReason {
    if saw_malformed {
        UnavailableReason::ShapeChanged
    } else {
        UnavailableReason::Empty
    }
}

/// Build the wire result from a [`Parsed`]: an `Available` tail, or — for a file
/// that yielded no turns — the typed empty-vs-shape-changed degrade.
pub(crate) fn build_tail(parsed: Parsed) -> TranscriptTail {
    if parsed.turns.is_empty() {
        return TranscriptTail::unavailable(empty_or_shape_changed(parsed.saw_malformed));
    }
    TranscriptTail::Available {
        turns: parsed.turns,
        last_assistant_message: parsed.last_assistant_message,
    }
}

/// Push a turn into the rolling buffer, evicting the oldest if it now exceeds
/// `keep`. Only the front is dropped, so the most-recent (open) turn always
/// survives for a continuation line to coalesce into.
pub(crate) fn push_bounded(turns: &mut VecDeque<Turn>, turn: Turn, keep: usize) {
    turns.push_back(turn);
    while turns.len() > keep {
        turns.pop_front();
    }
}

/// Bound a turn's tool-call count to [`MAX_TURN_TOOL_CALLS`] (issue #335), so no
/// single turn dominates the payload even if a message carries a pathological
/// number of parallel tool calls.
pub(crate) fn cap_tool_calls(tool_calls: &mut Vec<ToolCall>) {
    tool_calls.truncate(MAX_TURN_TOOL_CALLS);
}

/// Merge a continuation line of the same assistant message into the open turn:
/// append any text (re-truncating the combined result) and add its tool calls
/// (re-capping the combined list so a turn split across many lines still honours
/// [`MAX_TURN_TOOL_CALLS`]).
pub(crate) fn merge_into(turn: &mut Turn, more_text: &str, mut more_tools: Vec<ToolCall>) {
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
/// message — text plus any tool calls it made).
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
/// genuinely-quiet node from a degraded rich layer (ADR-0008 §3) — never a
/// panic, never a silent empty result.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    /// The provider doesn't produce a readable transcript at all (capability
    /// flag off — e.g. OpenCode, Agy, Terminal). Distinct from `NoSession` so a
    /// Coordinator can tell "this provider never has a transcript" from "this
    /// supported provider hasn't captured a session yet". The reader never
    /// emits this itself; a route gates on the provider capability and returns
    /// it before reading.
    Unsupported,
    /// The node has no captured CLI session id — e.g. a supported provider that
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
    /// turns could be parsed — the Claude Code JSONL shape has changed. A busy
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
        turns: Vec<Turn>,
        /// Last assistant text seen across the *whole* stream, not just the
        /// retained `turns` window — a small tail still surfaces the blocking
        /// question.
        last_assistant_message: Option<String>,
    },
    Unavailable {
        reason: UnavailableReason,
    },
}

impl TranscriptTail {
    pub fn unavailable(reason: UnavailableReason) -> Self {
        TranscriptTail::Unavailable { reason }
    }
}

/// A circuit needs the identity of the assistant response, not the file's
/// mtime: a new user prompt or tool event also changes the transcript file.
/// Unsupported/non-JSONL stores fall back to live per-turn PTY observation.
#[derive(Debug, Clone)]
pub(crate) struct AssistantReport {
    pub text: String,
    pub revision: String,
}