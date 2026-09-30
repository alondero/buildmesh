//! Cline hook adapter (issue #1775).
//!
//! Cline's CLI (verified against 3.0.62) discovers *file hooks* — executable
//! files named exactly after an event — from four fixed directories
//! (`~/Documents/Cline/Hooks`, `~/.cline/hooks`, `<ws>/.clinerules/hooks`,
//! `<ws>/.cline/hooks`). Each file is run with a JSON payload on stdin whose
//! `hookName` is the serialized event name (e.g. `agent_end`), not the file
//! name (`TaskComplete`).
//!
//! Buildmesh provisions exactly one of them
//! ([`crate::agent::provider::adapters::cline`]): `TaskComplete` → `agent_end`,
//! a clean turn completion. Cline's file-hook layer has no clean-exit dispatch
//! (`SessionShutdown`/`session_shutdown` is reachable only from the abort
//! branch of `afterRun`, so it fires on a user interrupt — never on teardown —
//! and a still-live session must not be reported as exited), no
//! permission/question primitive under the default auto-approve launch, and no
//! failure signal Buildmesh consumes. Every other recognised Cline event is
//! therefore classified lifecycle-neutral.
//!
//! **Transcript** (issue #1776). Cline's canonical history is a per-session
//! directory under `<cline data dir>/sessions/<session-id>/` holding two JSON
//! documents:
//!
//! - `<id>.json` — the session manifest (`status`, `ended_at`, `prompt`,
//!   `metadata.title`, `metadata.git.branch`, `cwd`, …). Session identity and
//!   telemetry live here; the reader does not consume it because every field it
//!   carries is either already on the node row or, for usage, an
//!   **observed-session** total that must never be presented as account quota.
//! - `<id>.messages.json` — the turn source. **One JSON object**, not JSONL:
//!   `{version: 1, messages: [...], system_prompt}`. The reader takes the
//!   top-level `messages` array only and deliberately ignores the embedded
//!   `system_prompt` (it duplicates the manifest's and dominates the file).
//!
//! `sessions.db` is deliberately *not* consulted: the research settled that it
//! stores no message content (only a `messages_path` pointer) and that its
//! `transcript_path` column is reserved and empty in every row, so the file is
//! the only real turn source. The DB row remains the fallback for session
//! *identity*, which `services::cline_session` already owns (#1774).
//!
//! **The file is rewritten wholesale, not appended.** Cline re-serialises the
//! whole document with a non-atomic `writeFileSync` at each `iteration_end`, so
//! it is not tail-able and a concurrent read can catch it empty or truncated.
//! The reader therefore parses the entire document on every read and treats an
//! unparseable document as `ShapeChanged` (the loud degrade) rather than
//! returning a partial tail that would read as a quiet session.
//!
//! **Turn reconstruction is user-delimited.** Cline writes no turn index and
//! role transitions alone are not enough: a `role: "user"` message may be a
//! harness notice (a compaction summary, a completion reminder, a
//! system/status/error display role, a `userRunSpan` injection) rather than
//! something the human typed. A user turn opens at each non-notice user
//! message, and the assistant messages up to the next one are that turn's work
//! — coalesced into a single [`Turn`] the way the Claude Code adapter coalesces
//! the several JSONL lines of one message.
//!
//! **No circuit report adapter.** The report reader
//! ([`super::super::report_snapshot`]) is line-oriented: it requires a trailing
//! newline, parses each line as a standalone JSON record, and takes the
//! publication time from a per-record `timestamp`. A Cline document is a single
//! object, so it is refused up front with `ReportReadError::Unsupported` — the
//! honest reason, and a load-bearing one: `readiness::prepare` admits
//! hook-native evidence for exactly `Unsupported | NoTranscript | Unreadable`,
//! so a Cline circuit keeps running on its `agent_end` receipt. A document that
//! fell through instead would report `PartialPublication` or `NoReport`, which
//! discard that receipt and stall the circuit permanently.
//!
//! For the same reason `line_has_assistant_text` is a per-JSONL-line predicate
//! with no meaning here and answers `false` (as OpenCode's does), and
//! `completed_turn` keeps its default `None`: no report path ever reaches them,
//! and the 256 KiB tail window ([`super::native_turn_completion_from_file`])
//! would truncate a document that has to be parsed whole. A native turn boundary
//! is a non-goal here, not an oversight.

use crate::agent::session_lifecycle::LifecycleKind;
use crate::services::transcript_reader::adapter::{
    HookClassification, HookDecision, LocateCtx, TranscriptAdapter,
};
use crate::services::transcript_reader::types::{
    cap_tool_calls, push_bounded, truncate, truncate_json_strings, Parsed, ToolCall, Turn,
    MAX_TOOL_STRING,
};
use serde_json::Value;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

pub(crate) struct ClineAdapter;

/// The Cline file-hook event names this adapter recognises. Payloads carrying
/// one of these are claimed so the attention route's generic "unknown event →
/// degraded attention mark" arm can never fire for a Cline body; anything
/// outside this set is left unclaimed for another adapter.
///
/// `pre_compact` is deliberately absent: `HOOK_CONFIG_FILE_EVENT_MAP` maps the
/// `PreCompact` file to `undefined`, so the file-hook layer never serialises
/// that event.
const CLINE_HOOK_EVENTS: &[&str] = &[
    "agent_start",
    "agent_resume",
    "agent_end",
    "agent_error",
    "agent_abort",
    "tool_call",
    "tool_result",
    "prompt_submit",
    "session_shutdown",
];

impl TranscriptAdapter for ClineAdapter {
    fn id(&self) -> &'static str {
        "cline"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        let sessions_root = cline_sessions_root(ctx.node_path)?;
        find_cline_transcript_in(&sessions_root, ctx.session_id)
    }

    fn parse(
        &self,
        lines: Box<dyn Iterator<Item = String> + '_>,
        keep: usize,
        max_text: usize,
    ) -> Parsed {
        parse_cline_messages(lines, keep, max_text)
    }

    fn line_has_assistant_text(&self, _line: &str) -> bool {
        // A Cline transcript is a single JSON *document*, not a stream of JSONL
        // records, so "does this line carry assistant text" has no meaning —
        // the answer lives in the document as a whole (OpenCode's adapter
        // answers `false` for the same reason). Consequence: the circuit report
        // reader finds no assistant offset and falls back to live per-turn PTY
        // observation, which is the honest answer for Cline.
        false
    }

    /// Normalise a Cline file-hook payload from its `hookName`.
    ///
    /// - `agent_end` — a completed turn (`afterRun` only calls `runTurnEnd`
    ///   when `result.status === "completed"`) → [`HookDecision::Ready`].
    /// - every other recognised event is lifecycle-neutral
    ///   ([`HookDecision::Ignore`]): Buildmesh provisions no hook for it, and
    ///   classifying it as anything else would mislabel the lifecycle. In
    ///   particular `session_shutdown` is the **abort** dispatch — it fires
    ///   when the user interrupts a *live* session, so it is explicitly not a
    ///   session-exit signal (claiming it here also stops a stale hook file
    ///   from a previous build falling through to the route's degraded arm).
    fn classify_hook(&self, body: &[u8], provider: &str) -> Option<HookClassification> {
        if provider != "cline" {
            return None;
        }
        let payload: serde_json::Value = serde_json::from_slice(body).ok()?;
        let event = payload
            .get("hookName")
            .or_else(|| payload.get("hook_name"))
            .and_then(|value| value.as_str())
            .map(str::to_ascii_lowercase)?;
        if !CLINE_HOOK_EVENTS.contains(&event.as_str()) {
            return None;
        }
        match event.as_str() {
            "agent_end" => Some(HookClassification {
                decision: HookDecision::Ready,
                kind: Some(LifecycleKind::TurnCompleted),
            }),
            _ => Some(HookClassification {
                decision: HookDecision::Ignore,
                kind: None,
            }),
        }
    }
}

// ── Locator (issue #1776) ───────────────────────────────────────────────

/// Root of Cline's per-session history tree: `<cline data dir>/sessions`.
///
/// Resolved through [`crate::env::cli_dir_for_spawn`] so a WSL/Interop guest
/// resolves the *guest* tree converted back to a host path (the UNC composition
/// stays inside `env` — never hand-built here), the same way the Muse and
/// mcode adapters resolve their stores. The native candidate honours
/// `CLINE_DATA_DIR`, which Cline treats as the data directory itself (its
/// `--data-dir` default is `~/.cline/data`); the guest-relative fallback
/// mirrors the default layout, since an override in the guest environment is
/// not visible from this process.
fn cline_sessions_root(node_path: &str) -> Option<PathBuf> {
    crate::env::cli_dir_for_spawn(crate::env::cline_data_dir()?, ".cline/data", node_path)
        .map(|data_dir| data_dir.join("sessions"))
}

/// Pure Cline locator: return `<sessions_root>/<id>/<id>.messages.json`.
///
/// Cline names the session directory *after the session id*, so no scan is
/// needed (unlike mcode, whose directory name is a timestamp + base64url id).
/// `locate` runs on every Coordinator poll tick, so a single `is_file` probe
/// is the whole cost.
///
/// The id is validated with [`crate::services::cline_session::is_cline_session_id`]
/// — the single owner of Cline's id shape (issue #1774). That is also the path
/// guard: the accepted charset is `[0-9a-z_]`, so a node row carrying a
/// tampered or corrupt `cli_session_id` cannot traverse out of the sessions
/// root. A directory that exists but holds no messages file is skipped, which
/// degrades upstream to `NoTranscript`.
pub(crate) fn find_cline_transcript_in(sessions_root: &Path, session_id: &str) -> Option<PathBuf> {
    if !crate::services::cline_session::is_cline_session_id(session_id) {
        return None;
    }
    let messages = sessions_root
        .join(session_id)
        .join(format!("{session_id}.messages.json"));
    messages.is_file().then_some(messages)
}

// ── Messages-document parser (issue #1776) ─────────────────────────────

/// The only document version this reader understands. Cline's own Zod schema
/// strips unknown keys, so *unknown keys* are tolerated below — but a
/// different `version` means a different message shape we have not verified, and
/// guessing would surface harness plumbing as dialogue.
const CLINE_MESSAGES_VERSION: i64 = 1;

/// `metadata.kind` values that mark a message as harness bookkeeping rather
/// than dialogue.
///
/// The compaction entries are also how a compacted session is accounted for:
/// when Cline compacts it re-materialises the collapsed prefix into `messages`,
/// so without this filter a compaction summary would open a spurious "user
/// turn" and swallow the real ones that follow it.
const CLINE_NOTICE_KINDS: &[&str] = &[
    "compaction",
    "compaction_summary",
    "auto_compaction",
    "compaction_budget_emergency",
    "completion_reminder",
    "loop_detection_notice",
    "mistake_stop_notice",
    "recovery_notice",
    "manual_compaction",
];

/// `metadata.displayRole` values that mark a message as harness output shown
/// in place of a real turn.
const CLINE_NON_DIALOGUE_DISPLAY_ROLES: &[&str] = &["system", "status", "error"];

/// Opening/closing tags of the wrapper Cline puts around a typed prompt.
const CLINE_USER_INPUT_OPEN: &str = "<user_input";
const CLINE_USER_INPUT_CLOSE: &str = "</user_input>";

/// A user prompt, unwrapped from Cline's `<user_input mode="…">` envelope.
struct UserInput {
    text: String,
    /// The envelope's `mode` attribute (`act`, `plan`, …), captured for the
    /// debug trace in [`parse_cline_turns`]. It is deliberately *not* put on the
    /// wire [`Turn`]: that type is format-agnostic, and a single harness's
    /// prompt mode is the same class of detail this reader already drops
    /// (thinking blocks, the `agent` backagent marker, per-message `metrics`).
    /// Losing it costs the Coordinator nothing, whereas widening the shared wire
    /// type to carry one harness's attribute would leak format knowledge into
    /// every adapter.
    mode: Option<String>,
}

/// Strip Cline's `<user_input mode="act">…</user_input>` envelope.
///
/// Anything that is not a well-formed wrapper is passed through unchanged, so a
/// future envelope shape degrades to "the raw text is still readable" instead
/// of silently emptying a user turn.
fn unwrap_user_input(raw: &str) -> UserInput {
    let passthrough = || UserInput {
        text: raw.to_string(),
        mode: None,
    };
    let trimmed = raw.trim();
    let Some(rest) = trimmed.strip_prefix(CLINE_USER_INPUT_OPEN) else {
        return passthrough();
    };
    let Some(tag_end) = rest.find('>') else {
        return passthrough();
    };
    let Some(body) = rest[tag_end + 1..].trim_end().strip_suffix(CLINE_USER_INPUT_CLOSE) else {
        return passthrough();
    };
    let mode = rest[..tag_end]
        .split_whitespace()
        .find_map(|attribute| attribute.strip_prefix("mode="))
        .map(|value| value.trim_matches(['"', '\'']).to_string())
        .filter(|value| !value.is_empty());
    UserInput {
        text: body.trim().to_string(),
        mode,
    }
}

/// Is this message harness bookkeeping rather than something the human typed?
///
/// Cline persists notice messages with a `role` that can look like real
/// dialogue (a compaction summary is a `user` message), so role alone cannot
/// open a turn. Any of three markers is decisive.
fn is_notice(message: &Value) -> bool {
    let Some(metadata) = message.get("metadata").and_then(Value::as_object) else {
        return false;
    };
    // A user-run span is an injection the harness addressed to itself, not a
    // prompt the human typed.
    if metadata.contains_key("userRunSpan") {
        return true;
    }
    if metadata
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| CLINE_NOTICE_KINDS.contains(&kind))
    {
        return true;
    }
    metadata
        .get("displayRole")
        .and_then(Value::as_str)
        .is_some_and(|role| CLINE_NON_DIALOGUE_DISPLAY_ROLES.contains(&role))
}

/// Concatenate the `text` of every `{type: "text"}` content block. `content`
/// may also arrive as a bare string (the same defensive convention the
/// Claude/Grok/mcode parsers accept).
fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Pull the `tool_use` blocks out of one message. `thinking`,
/// `redacted_thinking`, and `tool_result` blocks are transport details — a
/// tool result is the tool's echo, not something the agent decided — so they
/// never contribute text or a call, the same rule the mcode and AGY adapters
/// apply to their equivalents.
fn extract_cline_tool_calls(message: &Value) -> Vec<ToolCall> {
    let Some(Value::Array(blocks)) = message.get("content") else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        .map(|block| ToolCall {
            name: block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            input: truncate_json_strings(
                block.get("input").cloned().unwrap_or(Value::Null),
                MAX_TOOL_STRING,
            ),
        })
        .collect()
}

/// Parse a Cline `<id>.messages.json` document into logical turns under the
/// shared [`Parsed`] contract.
///
/// The whole document is parsed every time — Cline rewrites it wholesale with a
/// non-atomic `writeFileSync` at each `iteration_end`, so it cannot be tailed,
/// and a read that lands mid-write yields a truncated (unparseable) document
/// rather than a shorter one. The reader's line iterator is therefore joined
/// back into a single document here: JSON is whitespace-insensitive outside
/// strings, so a pretty-printed and a single-line file both round-trip.
///
/// Degradation, per the issue's ladder: a document that is absent never reaches
/// this function (`locate` returned `None` → `NoTranscript`); one that is
/// truncated, is not an object, carries a `version` other than `1`, or has no
/// `messages` array is reported as `saw_malformed` so the reader surfaces
/// `ShapeChanged` — the loud degrade — instead of a partial tail that would
/// read as a quietly-finished session. A well-formed document that simply has
/// no dialogue yet degrades to the quieter `Empty`.
pub(crate) fn parse_cline_messages(
    lines: impl Iterator<Item = String>,
    keep: usize,
    max_text: usize,
) -> Parsed {
    let document: String = lines.collect::<Vec<_>>().join("\n");
    let unreadable = || Parsed {
        turns: Vec::new(),
        last_assistant_message: None,
        saw_malformed: true,
    };
    let Ok(value) = serde_json::from_str::<Value>(&document) else {
        return unreadable();
    };
    if value.get("version").and_then(Value::as_i64) != Some(CLINE_MESSAGES_VERSION) {
        return unreadable();
    }
    let Some(Value::Array(messages)) = value.get("messages") else {
        return unreadable();
    };
    parse_cline_turns(messages, keep, max_text)
}

/// Turn reconstruction over the document's `messages` array.
///
/// A user turn opens at each non-notice `user` message; the assistant messages
/// up to the next one are that turn's work, coalesced into a single [`Turn`]
/// (one assistant "turn" is frequently split across several entries — text,
/// then a tool call, then the closing text — and emitting a turn per entry would
/// triple-count the conversation). An assistant message that arrives before any
/// user message still opens a turn: dropping it would silently lose real agent
/// output, which is the one thing this reader must never do.
fn parse_cline_turns(messages: &[Value], keep: usize, max_text: usize) -> Parsed {
    let keep = keep.max(1);
    let mut turns: VecDeque<Turn> = VecDeque::new();
    let mut open_assistant: Option<Turn> = None;
    let mut last_assistant_message: Option<String> = None;
    for message in messages {
        if is_notice(message) {
            continue;
        }
        let role = match message.get("role").and_then(Value::as_str) {
            Some("user") => "user",
            Some("assistant") => "assistant",
            // A backagent message, a tool-result echo promoted to a record, and
            // any role a future Cline adds — skipped silently, never flagged.
            _ => continue,
        };
        let mut text = message_text(message);
        if role == "user" {
            let input = unwrap_user_input(&text);
            text = input.text;
            if let Some(mode) = input.mode {
                tracing::debug!(mode, "cline: user prompt mode");
            }
        }
        let mut tool_calls = if role == "assistant" {
            extract_cline_tool_calls(message)
        } else {
            Vec::new()
        };
        // A turn with neither text nor a tool call is a no-op (an image-only
        // submission, a thinking-only message) — skip it without flagging so an
        // empty placeholder never evicts a real turn from the window.
        if text.trim().is_empty() && tool_calls.is_empty() {
            continue;
        }
        if role == "assistant" {
            cap_tool_calls(&mut tool_calls);
            let turn = open_assistant.get_or_insert_with(|| Turn {
                role: "assistant".to_string(),
                text: String::new(),
                tool_calls: Vec::new(),
            });
            if !text.trim().is_empty() {
                turn.text = if turn.text.is_empty() {
                    truncate(&text, max_text)
                } else {
                    truncate(&format!("{}\n{text}", turn.text), max_text)
                };
            }
            turn.tool_calls.append(&mut tool_calls);
            cap_tool_calls(&mut turn.tool_calls);
            continue;
        }
        // A user message closes the open assistant turn and opens a new run.
        if let Some(turn) = open_assistant.take() {
            if !turn.text.trim().is_empty() {
                last_assistant_message = Some(turn.text.clone());
            }
            push_bounded(&mut turns, turn, keep);
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
    }
    if let Some(turn) = open_assistant {
        // Whole-stream tracking: the last assistant turn *with text* wins, so a
        // trailing tool-call-only message does not erase the blocking question
        // the Coordinator needs to see.
        if !turn.text.trim().is_empty() {
            last_assistant_message = Some(turn.text.clone());
        }
        push_bounded(&mut turns, turn, keep);
    }
    Parsed {
        turns: turns.into(),
        last_assistant_message,
        saw_malformed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::transcript_reader::types::{empty_or_shape_changed, UnavailableReason};

    #[test]
    fn agent_end_maps_to_a_clean_turn_completion() {
        let body = br#"{"hookName":"agent_end","taskId":"session_1790003303940_9ouga","turn":{"status":"completed","outputText":"done"}}"#;
        let classified = ClineAdapter.classify_hook(body, "cline").expect("claimed");
        assert_eq!(classified.decision, HookDecision::Ready);
        assert_eq!(classified.kind, Some(LifecycleKind::TurnCompleted));
    }

    /// `session_shutdown` is the abort dispatch, not an exit: it fires when the
    /// user interrupts a still-live session. It must stay lifecycle-neutral —
    /// mapping it to `SessionExited` would write `Idle` on a live node (the
    /// blocked #1853 finding).
    #[test]
    fn session_shutdown_is_lifecycle_neutral_not_an_exit() {
        let body = br#"{"hookName":"session_shutdown","taskId":"session_1790003303940_9ouga","reason":"user-cancel"}"#;
        let classified = ClineAdapter.classify_hook(body, "cline").expect("claimed");
        assert_eq!(classified.decision, HookDecision::Ignore);
        assert_eq!(classified.kind, None);
    }

    /// A Cline event Buildmesh does not provision must be claimed as
    /// lifecycle-neutral: falling through would let the route's generic
    /// "unknown event" arm mark the node for attention with degraded health.
    #[test]
    fn unprovisioned_cline_events_are_lifecycle_neutral() {
        for event in [
            "tool_call",
            "tool_result",
            "prompt_submit",
            "agent_start",
            "agent_resume",
            "agent_error",
            "agent_abort",
        ] {
            let body = format!(r#"{{"hookName":"{event}","taskId":"session_1_abcde"}}"#);
            let classified = ClineAdapter
                .classify_hook(body.as_bytes(), "cline")
                .unwrap_or_else(|| panic!("{event} must be claimed"));
            assert_eq!(
                classified.decision,
                HookDecision::Ignore,
                "{event} must stay lifecycle-neutral"
            );
            assert_eq!(classified.kind, None);
        }
    }

    /// `PreCompact` maps to `undefined` in Cline's file-hook table, so the
    /// event is never serialised and must not be claimed.
    #[test]
    fn pre_compact_is_not_a_claimable_event() {
        assert!(ClineAdapter
            .classify_hook(br#"{"hookName":"pre_compact"}"#, "cline")
            .is_none());
    }

    #[test]
    fn unknown_hook_name_is_unclaimed() {
        assert!(ClineAdapter
            .classify_hook(br#"{"hookName":"mystery"}"#, "cline")
            .is_none());
        assert!(ClineAdapter
            .classify_hook(br#"{"notAHook":true}"#, "cline")
            .is_none());
        assert!(ClineAdapter.classify_hook(b"not json", "cline").is_none());
    }

    /// Provider gate: a body claiming a Cline event but arriving for a
    /// different harness must not be classified here.
    #[test]
    fn other_providers_are_not_claimed() {
        let body = br#"{"hookName":"agent_end"}"#;
        assert!(ClineAdapter.classify_hook(body, "codex").is_none());
        assert!(ClineAdapter.classify_hook(body, "").is_none());
    }

    // ── Transcript reader (issue #1776) ──────────────────────────────────

    /// A realistic single-line document, exactly as Cline's non-atomic
    /// `writeFileSync` emits it. Exercises the whole path: the `<user_input>`
    /// envelope, an assistant message split across a text entry and a tool-use
    /// entry, a `tool_result` echo, and the embedded `system_prompt` the reader
    /// must ignore.
    const DOCUMENT: &str = r#"{"version":1,"system_prompt":"You are Cline. <tool_specification>…</tool_specification>","messages":[{"id":"m1","role":"user","sessionId":"session_1790003303940_9ouga","ts":1789757012702,"content":[{"type":"text","text":"<user_input mode=\"act\">\nFix the failing login test\n</user_input>"}]},{"id":"m2","role":"assistant","content":[{"type":"thinking","thinking":"private plan"},{"type":"text","text":"Reading the auth module."}]},{"id":"m3","role":"assistant","content":[{"type":"tool_use","id":"t1","name":"read_file","input":{"path":"src/auth.ts"}},{"type":"tool_result","tool_use_id":"t1","content":"export const login = …"}]},{"id":"m4","role":"assistant","content":[{"type":"text","text":"The null check is missing. Fixed it."}]},{"id":"m5","role":"user","content":[{"type":"text","text":"<user_input mode=\"act\">\nOpen the PR\n</user_input>"}]}]}"#;

    fn parse(document: &str, keep: usize) -> Parsed {
        parse_cline_messages(document.split('\n').map(str::to_string), keep, 4_000)
    }

    /// The end-to-end contract: user prompt unwrapped from its envelope, the
    /// three assistant entries of one run coalesced into a single turn with its
    /// tool call, and the embedded `system_prompt` never surfacing as dialogue.
    #[test]
    fn reconstructs_user_delimited_turns_from_the_document() {
        let parsed = parse(DOCUMENT, 10);
        let roles: Vec<&str> = parsed.turns.iter().map(|turn| turn.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "assistant", "user"], "turns: {:#?}", parsed.turns);
        assert_eq!(parsed.turns[0].text, "Fix the failing login test");
        assert!(parsed.turns[0].tool_calls.is_empty());
        // `thinking` and the `tool_result` echo contribute neither text nor a
        // call; the two text entries concatenate.
        assert_eq!(parsed.turns[1].text, "Reading the auth module.\nThe null check is missing. Fixed it.");
        assert_eq!(parsed.turns[1].tool_calls.len(), 1);
        assert_eq!(parsed.turns[1].tool_calls[0].name, "read_file");
        assert_eq!(parsed.turns[1].tool_calls[0].input, serde_json::json!({"path": "src/auth.ts"}));
        assert_eq!(parsed.turns[2].text, "Open the PR");
        assert_eq!(
            parsed.last_assistant_message.as_deref(),
            Some("Reading the auth module.\nThe null check is missing. Fixed it.")
        );
        assert!(!parsed.saw_malformed);
    }

    /// The `system_prompt` is the largest string in the document and is *not*
    /// a turn: if it leaked, every Cline digest would open with the harness's
    /// own instruction text.
    #[test]
    fn system_prompt_never_becomes_a_turn() {
        let parsed = parse(DOCUMENT, 10);
        assert!(
            !parsed.turns.iter().any(|turn| turn.text.contains("tool_specification")),
            "the embedded system prompt must not surface as dialogue"
        );
    }

    /// A pretty-printed document reaches the parser as many lines; JSON is
    /// whitespace-insensitive outside strings, so it must parse identically to
    /// the single-line form.
    #[test]
    fn pretty_printed_documents_parse_identically() {
        let pretty = serde_json::to_string_pretty(
            &serde_json::from_str::<Value>(DOCUMENT).expect("fixture is valid json"),
        )
        .unwrap();
        assert!(pretty.lines().count() > 1, "the pretty form must really be multi-line");
        assert_eq!(parse(&pretty, 10).turns, parse(DOCUMENT, 10).turns);
    }

    /// Issue #1776's core parsing rule: a `role: "user"` message that is really
    /// a harness notice must not open a turn. Without this, a compaction
    /// summary would become a "user prompt" and the real turns after it would be
    /// attributed to the wrong run.
    #[test]
    fn notice_messages_never_open_a_user_turn() {
        let notices = [
            serde_json::json!({"metadata": {"kind": "compaction_summary"}}),
            serde_json::json!({"metadata": {"kind": "auto_compaction"}}),
            serde_json::json!({"metadata": {"kind": "manual_compaction"}}),
            serde_json::json!({"metadata": {"kind": "compaction_budget_emergency"}}),
            serde_json::json!({"metadata": {"kind": "completion_reminder"}}),
            serde_json::json!({"metadata": {"kind": "loop_detection_notice"}}),
            serde_json::json!({"metadata": {"kind": "mistake_stop_notice"}}),
            serde_json::json!({"metadata": {"kind": "recovery_notice"}}),
            serde_json::json!({"metadata": {"displayRole": "system"}}),
            serde_json::json!({"metadata": {"displayRole": "status"}}),
            serde_json::json!({"metadata": {"displayRole": "error"}}),
            serde_json::json!({"metadata": {"userRunSpan": {"start": 1}}}),
        ];
        for metadata in notices {
            let document = serde_json::json!({
                "version": 1,
                "messages": [
                    metadata,
                    {"role": "user", "content": [{"type": "text", "text": "real prompt"}]},
                    {"role": "assistant", "content": [{"type": "text", "text": "real answer"}]},
                ],
            });
            let parsed = parse(&document.to_string(), 10);
            assert_eq!(
                parsed.turns.len(),
                2,
                "notice {metadata} must not become a turn: {:#?}",
                parsed.turns
            );
            assert_eq!(parsed.turns[0].text, "real prompt");
            assert_eq!(parsed.turns[1].text, "real answer");
        }
    }

    /// A compaction re-materialises the collapsed prefix into `messages`, so
    /// the real conversation follows the summary. The summary must be dropped
    /// while the turns around it survive intact.
    #[test]
    fn compaction_summary_does_not_swallow_following_turns() {
        let document = serde_json::json!({
            "version": 1,
            "messages": [
                {"role": "user", "content": "<user_input mode=\"act\">first</user_input>"},
                {"role": "assistant", "content": [{"type": "text", "text": "first answer"}]},
                {"role": "user", "metadata": {"kind": "compaction_summary"},
                 "content": [{"type": "text", "text": "Summary of earlier work"}]},
                {"role": "user", "content": "<user_input mode=\"plan\">second</user_input>"},
                {"role": "assistant", "content": [{"type": "text", "text": "second answer"}]},
            ],
        });
        let parsed = parse(&document.to_string(), 10);
        let texts: Vec<&str> = parsed.turns.iter().map(|turn| turn.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["first", "first answer", "second", "second answer"],
            "the compaction summary must vanish without disturbing its neighbours"
        );
    }

    /// The envelope's `mode` is captured, and the prompt survives unwrapping in
    /// every mode — a plan-mode prompt is still something the human typed.
    #[test]
    fn user_input_envelope_is_stripped_and_its_mode_captured() {
        for (envelope, expected) in [
            ("<user_input mode=\"act\">do the thing</user_input>", "do the thing"),
            ("<user_input mode='plan'>draft it</user_input>", "draft it"),
            ("<user_input>no mode</user_input>", "no mode"),
            // Multi-line bodies and leading/trailing whitespace are normal.
            ("\n  <user_input mode=\"act\">\n  spaced out\n  </user_input>\n  ", "spaced out"),
        ] {
            let input = unwrap_user_input(envelope);
            assert_eq!(input.text, expected, "envelope: {envelope}");
        }
        assert_eq!(unwrap_user_input("<user_input mode=\"act\">x</user_input>").mode.as_deref(), Some("act"));
        assert_eq!(unwrap_user_input("<user_input mode=\"plan\">x</user_input>").mode.as_deref(), Some("plan"));
        assert_eq!(unwrap_user_input("<user_input>x</user_input>").mode, None);
        // A malformed or unexpected wrapper degrades to the raw text rather
        // than silently emptying the turn.
        for passthrough in ["plain text", "<user_input mode=\"act\">never closed", "<user_input", ""] {
            assert_eq!(unwrap_user_input(passthrough).text, passthrough);
            assert_eq!(unwrap_user_input(passthrough).mode, None);
        }
    }

    /// The gate is on `version === 1`: an unverified document version is a
    /// structural break and must degrade loudly rather than be guessed at.
    #[test]
    fn unverified_document_versions_degrade_to_shape_changed() {
        for document in [
            // no version at all
            r#"{"messages":[]}"#,
            // a future version whose message shape we have not verified
            r#"{"version":2,"messages":[{"role":"user","content":"hi"}]}"#,
            // not an object
            r#"[{"role":"user","content":"hi"}]"#,
            // version 1 but no messages array
            r#"{"version":1}"#,
            r#"{"version":1,"messages":{}}"#,
            // a version of the wrong JSON type
            r#"{"version":"1","messages":[]}"#,
        ] {
            let parsed = parse(document, 10);
            assert!(parsed.turns.is_empty(), "{document} must yield no turns");
            assert!(parsed.saw_malformed, "{document} must flag a shape change");
            assert_eq!(
                empty_or_shape_changed(parsed.saw_malformed),
                UnavailableReason::ShapeChanged
            );
        }
    }

    /// Cline rewrites the whole document with a non-atomic `writeFileSync`, so
    /// a read that races the write yields an empty or half-written file. A busy
    /// node must never look quietly finished, so this degrades loudly.
    #[test]
    fn a_truncated_rewrite_degrades_loudly_rather_than_reading_as_quiet() {
        let truncated = &DOCUMENT[..DOCUMENT.len() / 2];
        let parsed = parse(truncated, 10);
        assert!(parsed.turns.is_empty());
        assert!(parsed.saw_malformed, "a partial document must not parse as a quiet session");
        // Same for the empty file a non-atomic truncate-then-write leaves.
        let empty = parse("", 10);
        assert!(empty.turns.is_empty());
        assert!(empty.saw_malformed);
    }

    /// A well-formed document with only notices is a genuinely quiet session —
    /// `Empty`, not `ShapeChanged`, so the digest does not page the user.
    #[test]
    fn a_notice_only_session_degrades_to_empty() {
        let document = serde_json::json!({
            "version": 1,
            "messages": [
                {"role": "user", "metadata": {"kind": "compaction"}, "content": "summary"},
                {"role": "assistant", "content": [{"type": "thinking", "thinking": "hmm"}]},
                {"role": "assistant", "content": [{"type": "image", "data": "…"}]},
            ],
        });
        let parsed = parse(&document.to_string(), 10);
        assert!(parsed.turns.is_empty());
        assert!(!parsed.saw_malformed);
        assert_eq!(
            empty_or_shape_changed(parsed.saw_malformed),
            UnavailableReason::Empty
        );
    }

    /// Cline has no turn index, so the assistant work of a run is spread over
    /// several messages. Coalescing them is what stops the Coordinator seeing
    /// one exchange as three.
    #[test]
    fn assistant_work_of_one_run_coalesces_into_a_single_turn() {
        let document = serde_json::json!({
            "version": 1,
            "messages": [
                {"role": "user", "content": "<user_input mode=\"act\">go</user_input>"},
                {"role": "assistant", "content": [{"type": "tool_use", "name": "search", "input": {"q": "x"}}]},
                {"role": "assistant", "content": [{"type": "tool_use", "name": "read", "input": {"p": "y"}}]},
                {"role": "assistant", "content": [{"type": "text", "text": "Found it."}]},
            ],
        });
        let parsed = parse(&document.to_string(), 10);
        assert_eq!(parsed.turns.len(), 2, "turns: {:#?}", parsed.turns);
        assert_eq!(parsed.turns[1].tool_calls.len(), 2);
        assert_eq!(parsed.turns[1].text, "Found it.");
    }

    /// A trailing tool-call-only assistant message must not erase the last
    /// assistant *text* — that text is the blocking question the Coordinator
    /// needs, and losing it would make a waiting node look finished.
    #[test]
    fn a_trailing_tool_only_message_keeps_the_last_assistant_text() {
        let document = serde_json::json!({
            "version": 1,
            "messages": [
                {"role": "user", "content": "<user_input mode=\"act\">go</user_input>"},
                {"role": "assistant", "content": [{"type": "text", "text": "Which file should I edit?"}]},
                {"role": "assistant", "content": [{"type": "tool_use", "name": "glob", "input": {}}]},
            ],
        });
        let parsed = parse(&document.to_string(), 10);
        assert_eq!(parsed.last_assistant_message.as_deref(), Some("Which file should I edit?"));
    }

    /// A small `keep` must evict the oldest turns, and the whole-stream
    /// `last_assistant_message` must still come from the full document.
    #[test]
    fn keep_bounds_the_window_without_losing_the_last_message() {
        let document = serde_json::json!({
            "version": 1,
            "messages": [
                {"role": "user", "content": "one"},
                {"role": "assistant", "content": [{"type": "text", "text": "first answer"}]},
                {"role": "user", "content": "two"},
                {"role": "assistant", "content": [{"type": "text", "text": "second answer"}]},
            ],
        });
        let parsed = parse(&document.to_string(), 2);
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].text, "two");
        assert_eq!(parsed.last_assistant_message.as_deref(), Some("second answer"));
    }

    /// Unknown keys are tolerated (Cline's own Zod schema `$strip`s them), and
    /// per-message `metrics` are observed-session telemetry, not dialogue — they
    /// must never land in a turn.
    #[test]
    fn unknown_keys_and_metrics_are_tolerated_but_never_surfaced() {
        let document = serde_json::json!({
            "version": 1,
            "futureTopLevelKey": {"anything": true},
            "messages": [
                {"role": "user", "sessionId": "s", "agent": "cli",
                 "content": "<user_input mode=\"act\">go</user_input>",
                 "metrics": {"inputTokens": 1200, "outputTokens": 340, "cost": 0.42}},
                {"role": "assistant", "modelInfo": {"id": "claude-opus-5"},
                 "content": [{"type": "text", "text": "answer"}],
                 "metrics": {"inputTokens": 1300, "outputTokens": 90, "cost": 0.51}},
            ],
        });
        let parsed = parse(&document.to_string(), 10);
        assert_eq!(parsed.turns.len(), 2);
        assert!(!parsed.saw_malformed, "unknown keys must be tolerated");
        for turn in &parsed.turns {
            assert!(!turn.text.contains("inputTokens"), "telemetry must not surface as dialogue");
        }
    }

    /// A backagent / tool-echo record promoted to a message role is not a
    /// turn, and is skipped quietly rather than flagged.
    #[test]
    fn unknown_roles_skip_quietly() {
        let document = serde_json::json!({
            "version": 1,
            "messages": [
                {"role": "tool", "content": "tool output"},
                {"role": "system", "content": "you are cline"},
                {"role": "user", "content": "<user_input mode=\"act\">go</user_input>"},
            ],
        });
        let parsed = parse(&document.to_string(), 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].role, "user");
        assert!(!parsed.saw_malformed);
    }

    // ── Locator ─────────────────────────────────────────────────────────

    /// Cline names the session directory after the session id, so resolution is
    /// a single probe — no scan. The full layout must be
    /// `<sessions>/<id>/<id>.messages.json`.
    #[test]
    fn locator_resolves_the_session_messages_document() {
        let root = tempfile::tempdir().unwrap();
        let id = "session_1790003303940_9ouga";
        let dir = root.path().join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let messages = dir.join(format!("{id}.messages.json"));
        std::fs::write(&messages, DOCUMENT).unwrap();
        // The manifest sits beside it and must not confuse the locator.
        std::fs::write(dir.join(format!("{id}.json")), r#"{"status":"completed"}"#).unwrap();
        // A sibling session must not be picked up.
        let sibling = root.path().join("session_1790003303999_aaaaa");
        std::fs::create_dir_all(&sibling).unwrap();

        assert_eq!(
            find_cline_transcript_in(root.path(), id),
            Some(messages)
        );
        assert_eq!(find_cline_transcript_in(root.path(), "session_1790003303999_aaaaa"), None);
    }

    /// Degradation rungs 1 and 2: a session directory Cline never created, and
    /// one it created but whose messages document is not (yet) on disk. Both
    /// resolve to `None`, which the reader reports as `NoTranscript`.
    #[test]
    fn locator_degrades_when_the_session_or_messages_are_absent() {
        let root = tempfile::tempdir().unwrap();
        let id = "session_1790003303940_9ouga";
        // Directory present, messages file absent (Cline creates the
        // directory before the first rewrite lands).
        std::fs::create_dir_all(root.path().join(id)).unwrap();
        assert_eq!(find_cline_transcript_in(root.path(), id), None);
        // No sessions tree at all.
        assert_eq!(find_cline_transcript_in(&root.path().join("nope"), id), None);
    }

    /// The session id comes off a node row, so it is untrusted input that lands
    /// in a path join. `is_cline_session_id` (the single owner of Cline's id
    /// shape, issue #1774) is also the traversal guard: its charset is
    /// `[0-9a-z_]`, which admits no separator, no `..`, and no drive prefix.
    #[test]
    fn locator_rejects_ids_that_are_not_cline_session_ids() {
        let root = tempfile::tempdir().unwrap();
        // Plant a decoy the traversal attempts aim at, to prove the guard is
        // what stops it (not merely an absent file).
        std::fs::create_dir_all(root.path().join("decoy")).unwrap();
        std::fs::write(root.path().join("decoy/evil.json"), DOCUMENT).unwrap();
        for id in [
            "",
            "..",
            "../decoy",
            "..\\decoy",
            "/etc",
            "C:\\Windows",
            "session_1790003303940_9ouga/../../decoy",
            // right shape, wrong charset / lengths
            "session_abc_defgh",
            "session_1790003303940_9oug",
            "session_1790003303940_9OUGE",
            "session_1790003303940_9ouga__agent_x",
        ] {
            assert_eq!(
                find_cline_transcript_in(root.path(), id),
                None,
                "{id:?} is not a Cline session id and must not resolve"
            );
        }
    }

    /// `line_has_assistant_text` is a per-JSONL-line predicate and a Cline
    /// transcript is one document, so it must answer `false` — otherwise the
    /// circuit report reader would chase line offsets that do not exist.
    #[test]
    fn line_predicate_is_false_for_a_document_format() {
        assert!(!ClineAdapter.line_has_assistant_text(DOCUMENT));
        assert!(!ClineAdapter.line_has_assistant_text(
            r#"{"role":"assistant","content":"anything"}"#
        ));
    }
}
