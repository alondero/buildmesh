//! Single owner of agent-node state transitions (issue #132).
//!
//! ## Why this module exists
//!
//! Before this refactor, every state transition on an agent node —
//! `Spawning → Running → AwaitingInput → Idle/Error/Suspended/...` — was
//! driven from whichever code path happened to trigger it. The DB write
//! (`db::update_agent_node_status*`) and the matching Tauri events
//! (`attention-needed`, `attention-cleared`, `resume-failed`) were emitted
//! from a sprawl of files (`agent/spawn.rs`, `commands/agent.rs`,
//! `commands/attention.rs`, `coordinator/drive.rs`, `autopilot/pipeline.rs`,
//! `http/ws.rs`, `attention_autoclear.rs`). New developers — and AI
//! agents — couldn't answer "what does it mean for a session to enter
//! state X?" without grepping 4-5 files, and the startup sweep that
//! patches `Running → Suspended` after a crash lived in `lib.rs::setup`
//! with no relationship to the rest of the state-machine code.
//!
//! ## Design
//!
//! `SessionLifecycle` is the **single writer** for `agent_nodes.status`
//! and the **single emitter** of the three lifecycle Tauri events. Every
//! other module fires *into* the lifecycle (`on_attention(node_id)`,
//! `on_pty_eof(node_id)`, …) and the lifecycle decides which DB write
//! and which events — if any — are needed. Mirrors the `AttentionSink`
//! trait pattern in `commands/attention.rs` for testability without a
//! real `AppHandle`.
//!
//! ```text
//!  caller fires ───────►  SessionLifecycle  ───────►  DB + Tauri events
//!                          (this module)
//! ```
//!
//! ### Open design questions — answered by the issue's acceptance criteria
//!
//! 1. **Source-of-truth scope (own vs coordinate).** OWN — acceptance
//!    criterion: *"One module is the single writer for `agent_nodes.status`.
//!    All direct `db::update_agent_node_status` calls outside it are
//!    deleted."*
//! 2. **Event direction (consequence vs trigger).** CONSEQUENCE —
//!    acceptance criterion: *"Tauri events `attention-needed`,
//!    `resume-failed`, `attention-cleared` are emitted from exactly one
//!    place."* Callers fire events into the lifecycle; the lifecycle
//!    emits the Tauri events as a *consequence* of the transition.
//! 3. **Crash recovery placement.** INSIDE — acceptance criterion:
//!    *"Crash recovery path … lives inside `SessionLifecycle`."* Lives
//!    here as `recover_from_crash()`.
//!
//! ### Invariants
//!
//! - **DB status is the single source of truth for `awaiting_input`.**
//!   `is_attention_pending` derives from `node.status`, no in-memory
//!   mirror (the previous `ATTENTION_PENDING` set was removed for this
//!   reason — see `commands/attention.rs` tests).
//! - **`ProcessRegistry` membership.** The PTY handle registry is owned
//!   by the PTY lifecycle code (`agent/process.rs`, the reader thread's
//!   `PostExitAction`, `kill_session`) — *not* by this module. The
//!   invariant ("a `Running` node has a registered process, an `Idle`
//!   node doesn't") is enforced by making sure the same trigger that
//!   flips the status also drives the corresponding registry action.
//!   Concretely: `on_pty_eof` and `on_kill` are the *callers* that
//!   invoke the reader's post-exit epilogue / `kill_session`, and they
//!   check the registry before writing the terminal state.
//! - **`Spawning → Running` race.** The conditional `if` write
//!   (`update_agent_node_status_if_inner`) keeps the delayed promotion
//!   from resurrecting a reader-written `Error` — symmetric to the
//!   orchestrator's `unless_in(Error, Archived)` guard (issue #654).
//!
//! ### API shape
//!
//! ```ignore
//! pub trait SessionLifecycleSink {
//!     fn write_status(&self, node_id: i64, new: SessionStatus) -> Result<(), String>;
//!     fn write_status_if(&self, node_id: i64, new: SessionStatus, expected: SessionStatus)
//!         -> Result<bool, String>;
//!     fn write_status_unless_in(
//!         &self,
//!         node_id: i64,
//!         new: SessionStatus,
//!         forbidden: &[SessionStatus],
//!     ) -> Result<bool, String>;
//!     fn emit_attention_needed(&self, node_id: i64);
//!     fn emit_attention_cleared(&self, node_id: i64);
//!     fn emit_resume_failed(&self, node_id: i64, reason: &str);
//! }
//! ```
//!
//! Every transition entry point is `pub fn on_*(sink: &dyn
//! SessionLifecycleSink, …)` — easy to test with a fake sink.

use crate::db;
use crate::models::SessionStatus;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use ts_rs::TS;

/// Terminal statuses a recovery/error write must never resurrect (issue
/// #654). Used by `on_resume_failed` and `on_error` — both write `Error`
/// (or skip the write) when the node is already in this set, so the
/// `status_changed_at` stamp doesn't double-bump and an `Archived` row
/// can never be brought back to life by a racing orchestrator.
pub(crate) const FORBIDDEN_TERMINAL: &[SessionStatus] =
    &[SessionStatus::Error, SessionStatus::Archived];

// ---------------------------------------------------------------------------
// Wire types — Tauri event payloads (issue #161)
// ---------------------------------------------------------------------------
//
// These structs are the source of truth for the three lifecycle event
// payloads; ts-rs generates matching `*.ts` files into
// `src/types/generated/` at `cargo test` time. The frontend imports the
// generated types from `src/stores/agentNodeStore.ts` and `src/App.tsx`
// — see the CLAUDE.md "Shared Rust↔TS types" rule. The lifecycle module
// owns these because it owns the *emit*; the Tauri commands in
// `commands/attention.rs` and `commands/agent.rs` route through the
// lifecycle, so the struct definitions stay here too.
//
// Naming follows the issue #490 wire convention: `session_id` (not
// `node_id`) for attention events because the existing agent hooks
// already use that key (CONTEXT.md ambiguity #1).

/// Payload of the `attention-needed` Tauri event. Emitted by
/// [`AppSessionLifecycleSink::emit_attention_needed`] when a Node Turn
/// lands; the frontend flips the node status to `awaiting_input`.
///
/// Generated to `src/types/generated/AttentionNeededPayload.ts`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "AttentionNeededPayload.ts")]
pub struct AttentionNeededPayload {
    #[ts(as = "i32")]
    pub session_id: i64,
    pub semantic_turn: Option<SemanticTurnPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, TS)]
#[ts(export, export_to = "SemanticTurnPayload.ts")]
pub struct SemanticTurnPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
    pub kind: SemanticTurnKind,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case", export_to = "SemanticTurnKind.ts")]
pub enum SemanticTurnKind {
    PermissionRequest,
    CommandConfirmation,
    TurnFinished,
}

/// Payload of the `attention-cleared` Tauri event. Emitted by
/// [`AppSessionLifecycleSink::emit_attention_cleared`] when a Node Turn
/// resolves (the user typed, the autoclear safety net observed a
/// resumed burst, the coordinator drove a prompt, …).
///
/// Generated to `src/types/generated/AttentionClearedPayload.ts`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "AttentionClearedPayload.ts")]
pub struct AttentionClearedPayload {
    #[ts(as = "i32")]
    pub session_id: i64,
}

// ---------------------------------------------------------------------------
// Normalized lifecycle contract (issue #1364)
// ---------------------------------------------------------------------------
//
// The provider-neutral lifecycle signal: a typed kind plus a full envelope
// preserving the provider's event name, session/turn id, completion reason,
// transcript path, timestamp, and signal health. Emitted as `agent-lifecycle`
// to BOTH the desktop Tauri bus and the mobile `/ws/events` broadcast — the
// `AppSessionLifecycleSink::emit_lifecycle_changed` method is the single
// choke point for the fan-out, so the two transports can't drift.

/// Normalized, provider-neutral meaning of a lifecycle signal (issue #1364).
///
/// Every harness-specific hook payload (Claude `Stop`/`Notification`, Codex
/// `Stop`/`PermissionRequest`, AGY `Stop`+`fullyIdle`, Grok `Notification`
/// +`notificationType`, …) maps onto exactly one of these kinds. An unknown
/// or unparseable payload maps to `SignalUnavailable` / `InputRequired` with
/// a degraded health — never silently presented as a high-confidence
/// permission request.
///
/// Generated to `src/types/generated/LifecycleKind.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case", export_to = "LifecycleKind.ts")]
pub enum LifecycleKind {
    /// An ordinary turn finished and the agent is at its prompt, ready for
    /// another prompt. The node lands in `Ready` — NOT the Autopilot-only
    /// `Completed` (issue #1364).
    TurnCompleted,
    /// Autopilot's wrap-up verified a clean worktree and opened a PR — the
    /// `Completed` terminal state (issue #485). Distinct from
    /// `TurnCompleted` so consumers never have to inspect `status` to tell
    /// "agent ready for another prompt" from "terminal PR-opened".
    AutopilotCompleted,
    /// The agent yielded and the user is needed, but the hook did not
    /// distinguish a permission from a question. Node lands in
    /// `AwaitingInput`.
    InputRequired,
    /// The agent is blocked on a tool-approval decision.
    PermissionRequested,
    /// The agent is asking the user a question (not a tool approval).
    QuestionRequested,
    /// A turn ended but background work is still running (false yield,
    /// issue #878) — the node stays `Running`.
    BackgroundRunning,
    /// A node was marked idle without a clean process exit (kill, resume
    /// skip).
    ProcessIdle,
    /// The agent's PTY exited cleanly (EOF) — the process is gone.
    SessionExited,
    /// A spawn / autopilot run failed.
    Error,
    /// The hook could not be installed, trusted, reached, or parsed — the
    /// harness state is unknown. Never "no attention needed".
    SignalUnavailable,
}

/// Hook delivery health for a node (issue #1364 §3). A layered field on the
/// node, not a status: `Ok` once provisioning succeeded or the first hook
/// callback arrived; `Degraded` for an unparseable/unknown payload;
/// `Unavailable` when provisioning or delivery failed.
///
/// Generated to `src/types/generated/SignalHealth.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case", export_to = "SignalHealth.ts")]
pub enum SignalHealth {
    #[default]
    Ok,
    Degraded,
    Unavailable,
}

impl SignalHealth {
    /// Parse a DB string (`ok` / `degraded` / `unavailable`). Unknown or
    /// empty → `None` so a stale value never surfaces as a wrong health.
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "ok" => Some(SignalHealth::Ok),
            "degraded" => Some(SignalHealth::Degraded),
            "unavailable" => Some(SignalHealth::Unavailable),
            _ => None,
        }
    }

    /// DB-string form (matches [`SignalHealth::from_db_str`]).
    pub fn to_db_str(&self) -> &'static str {
        match self {
            SignalHealth::Ok => "ok",
            SignalHealth::Degraded => "degraded",
            SignalHealth::Unavailable => "unavailable",
        }
    }
}

/// Provider-side details preserved from the hook payload (issue #1364 §1).
/// Carried by the lifecycle event so the UI can distinguish "the harness has
/// not produced an event yet" from "the hook was not installed, trusted,
/// executable, or reachable" — and so a clean turn completion is never
/// presented as a permission request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookSignalDetail {
    /// Buildmesh harness/provider id of the node (e.g. `"anthropic"`),
    /// resolved by the emitting caller (issue #1364 §1). Sparse lifecycle
    /// transitions leave `None` — clients resolve the provider from the
    /// node row they already hold.
    pub provider: Option<String>,
    /// Provider event name (`hook_event_name` / `notification_type`).
    pub provider_event: Option<String>,
    /// Provider session/turn id (`session_id` / `conversationId`), UUID-
    /// validated at the route boundary.
    pub provider_session_id: Option<String>,
    /// Completion/termination reason (`termination_reason` / `reason`).
    pub completion_reason: Option<String>,
    /// Transcript path the harness reported (host form).
    pub transcript_path: Option<String>,
    /// Signal health — `Degraded` for an unparseable/unknown payload.
    pub signal_health: SignalHealth,
    /// Semantic turn payload when the signal carries one.
    pub semantic_turn: Option<SemanticTurnPayload>,
    /// Human-facing message.
    pub message: Option<String>,
    /// Optional normalized-kind override. The attention route sets this when
    /// it can distinguish a question from a generic input-required yield
    /// (e.g. a question-shaped notification message); the lifecycle falls
    /// back to deriving the kind from the semantic turn.
    pub kind: Option<LifecycleKind>,
    /// Raw Grok `notificationType` (issue #1366) — `permission_prompt`,
    /// `idle_prompt`, `task_complete`, etc. Carried alongside
    /// `provider_event` so the UI can render the harness's own
    /// classification when the shared lifecycle has collapsed
    /// structurally distinct notifications to one normalized kind.
    pub notification_type: Option<String>,
}

/// Payload of the `agent-lifecycle` Tauri/WebSocket event (issue #1364).
/// Every meaningful lifecycle transition emits one of these — clean EOF,
/// error, completion, background-running, input-required — carrying the
/// normalized kind and the full provider envelope. Both transports share
/// this one wire shape: the desktop `AppHandle.emit("agent-lifecycle", …)`
/// bus and the mobile `EventMsg::LifecycleChanged` broadcast (the
/// `/ws/events` handler serializes the whole enum, so the shapes match).
///
/// Generated to `src/types/generated/LifecycleChangedPayload.ts`.
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export, export_to = "LifecycleChangedPayload.ts")]
pub struct LifecycleChangedPayload {
    /// Buildmesh node id (issue #490 wire convention: `session_id`).
    #[ts(as = "i32")]
    pub session_id: i64,
    /// Buildmesh harness/provider id, when the emitting caller knows it.
    pub provider: Option<String>,
    /// Normalized lifecycle kind.
    pub kind: LifecycleKind,
    /// The resulting node status.
    pub status: SessionStatus,
    /// Human-facing message (semantic description or a fallback).
    pub message: Option<String>,
    /// Provider event name (`hook_event_name` / `notification_type`).
    pub provider_event: Option<String>,
    /// Provider session/turn id (`session_id` / `conversationId`).
    pub provider_session_id: Option<String>,
    /// Completion/termination reason.
    pub completion_reason: Option<String>,
    /// Transcript path the harness reported.
    pub transcript_path: Option<String>,
    /// RFC3339 timestamp of the transition (same format as
    /// `status_changed_at`).
    pub timestamp: String,
    /// Hook delivery health for this signal.
    pub signal_health: SignalHealth,
    /// Semantic turn when the signal carries one.
    pub semantic_turn: Option<SemanticTurnPayload>,
}

impl LifecycleChangedPayload {
    pub fn new(
        session_id: i64,
        kind: LifecycleKind,
        status: SessionStatus,
        detail: &HookSignalDetail,
        fallback_message: &str,
    ) -> Self {
        Self {
            session_id,
            provider: detail.provider.clone(),
            kind,
            status,
            message: detail
                .message
                .clone()
                .or_else(|| Some(fallback_message.to_string())),
            provider_event: detail.provider_event.clone(),
            provider_session_id: detail.provider_session_id.clone(),
            completion_reason: detail.completion_reason.clone(),
            transcript_path: detail.transcript_path.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            signal_health: detail.signal_health,
            semantic_turn: detail.semantic_turn.clone(),
        }
    }
}

/// Payload of the `resume-failed` Tauri event. Emitted by
/// [`AppSessionLifecycleSink::emit_resume_failed`] when a `--resume`
/// attempt fails (PTY reader early-exit heuristic, `auto_resume_agent_nodes`
/// `spawn_agent_inner` failure). The frontend renders it as a toast.
///
/// Generated to `src/types/generated/ResumeFailedPayload.ts`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "ResumeFailedPayload.ts")]
pub struct ResumeFailedPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
    pub error: String,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Side-effect surface for lifecycle transitions. Mirrors `AttentionSink` in
/// `commands/attention.rs` so the logic is testable without `AppHandle` or the
/// process-global DB.
pub trait SessionLifecycleSink {
    /// Unconditional status write. Returns `Err` on DB failure (callers may
    /// log and continue — most transition sites already use `let _ = …`).
    fn write_status(&self, node_id: i64, new: SessionStatus) -> Result<(), String>;

    /// Conditional write — only flips `expected → new`. Used by the
    /// `Spawning → Running` promotion so the reader's early-exit `Error`
    /// write wins if it already fired (issue #654).
    fn write_status_if(
        &self,
        node_id: i64,
        new: SessionStatus,
        expected: SessionStatus,
    ) -> Result<bool, String>;

    /// Inverse of `write_status_if` — flips unless current is in
    /// `forbidden`. Used by the orchestrator's `Spawning` write so it
    /// can't resurrect a reader-written `Error` or an `Archived` row.
    fn write_status_unless_in(
        &self,
        node_id: i64,
        new: SessionStatus,
        forbidden: &[SessionStatus],
    ) -> Result<bool, String>;

    fn emit_attention_needed(&self, node_id: i64);
    fn emit_attention_needed_with_payload(
        &self,
        node_id: i64,
        semantic_turn: Option<SemanticTurnPayload>,
    ) {
        let _ = semantic_turn;
        self.emit_attention_needed(node_id);
    }
    fn emit_attention_cleared(&self, node_id: i64);
    fn emit_resume_failed(&self, node_id: i64, reason: &str);
    /// Emit the normalized `agent-lifecycle` event (issue #1364). The
    /// `AppSessionLifecycleSink` implementation fans out to BOTH the desktop
    /// Tauri bus and the mobile `/ws/events` broadcast — the single choke
    /// point for the new event family so the two transports can't drift.
    fn emit_lifecycle_changed(&self, payload: LifecycleChangedPayload);
}

// ---------------------------------------------------------------------------
// Production sink
// ---------------------------------------------------------------------------

/// Sink backed by the real DB + `AppHandle`. The **only** place in the
/// codebase that calls the `db::update_agent_node_status*` family and the
/// **only** place that emits `attention-needed` / `attention-cleared` /
/// `resume-failed`.
pub struct AppSessionLifecycleSink<'a> {
    pub app: &'a AppHandle,
}

impl SessionLifecycleSink for AppSessionLifecycleSink<'_> {
    fn write_status(&self, node_id: i64, new: SessionStatus) -> Result<(), String> {
        db::update_agent_node_status(node_id, new).map_err(|e| e.to_string())
    }

    fn write_status_if(
        &self,
        node_id: i64,
        new: SessionStatus,
        expected: SessionStatus,
    ) -> Result<bool, String> {
        db::update_agent_node_status_if(node_id, new, expected).map_err(|e| e.to_string())
    }

    fn write_status_unless_in(
        &self,
        node_id: i64,
        new: SessionStatus,
        forbidden: &[SessionStatus],
    ) -> Result<bool, String> {
        db::update_agent_node_status_unless_in(node_id, new, forbidden).map_err(|e| e.to_string())
    }

    fn emit_attention_needed(&self, node_id: i64) {
        self.emit_attention_needed_with_payload(node_id, None);
    }

    fn emit_attention_needed_with_payload(
        &self,
        node_id: i64,
        semantic_turn: Option<SemanticTurnPayload>,
    ) {
        let _ = self.app.emit(
            "attention-needed",
            AttentionNeededPayload {
                session_id: node_id,
                semantic_turn,
            },
        );
    }

    fn emit_attention_cleared(&self, node_id: i64) {
        let _ = db::persist_semantic_turn(node_id, None);
        let _ = self.app.emit(
            "attention-cleared",
            AttentionClearedPayload {
                session_id: node_id,
            },
        );
    }

    fn emit_resume_failed(&self, node_id: i64, reason: &str) {
        let _ = self.app.emit(
            "resume-failed",
            ResumeFailedPayload {
                node_id,
                error: reason.to_string(),
            },
        );
    }

    fn emit_lifecycle_changed(&self, payload: LifecycleChangedPayload) {
        // Desktop: Tauri event bus reaches only the webview.
        let _ = self.app.emit("agent-lifecycle", &payload);
        // Mobile: the same wire shape fanned into the /ws/events broadcast.
        // `http::events` already depends on `session_lifecycle` types; the
        // reverse reference is a legal Rust module cycle (the two modules
        // form one event-transport seam). Boxed variant — see EventMsg.
        crate::http::events::emit(crate::http::events::EventMsg::LifecycleChanged(Box::new(
            payload,
        )));
    }
}

// ---------------------------------------------------------------------------
// DB-only sink — for paths where no `AppHandle` is reachable (e.g. the PTY
// reader thread after app shutdown, or `clear_now` in `attention_autoclear.rs`
// when `crate::http::app_handle()` returns `None`). Writes the DB but the
// emit methods are no-ops, so a no-handle path silently drops the event
// (matching pre-refactor behaviour where the `if let Some(app)` branch
// guarded the emit).
// ---------------------------------------------------------------------------

pub struct DbOnlySink;

impl SessionLifecycleSink for DbOnlySink {
    fn write_status(&self, node_id: i64, new: SessionStatus) -> Result<(), String> {
        db::update_agent_node_status(node_id, new).map_err(|e| e.to_string())
    }

    fn write_status_if(
        &self,
        node_id: i64,
        new: SessionStatus,
        expected: SessionStatus,
    ) -> Result<bool, String> {
        db::update_agent_node_status_if(node_id, new, expected).map_err(|e| e.to_string())
    }

    fn write_status_unless_in(
        &self,
        node_id: i64,
        new: SessionStatus,
        forbidden: &[SessionStatus],
    ) -> Result<bool, String> {
        db::update_agent_node_status_unless_in(node_id, new, forbidden).map_err(|e| e.to_string())
    }

    fn emit_attention_needed(&self, _node_id: i64) {}
    fn emit_attention_cleared(&self, node_id: i64) {
        let _ = db::persist_semantic_turn(node_id, None);
    }
    fn emit_resume_failed(&self, _node_id: i64, _reason: &str) {}
    fn emit_lifecycle_changed(&self, _payload: LifecycleChangedPayload) {}
}

// ---------------------------------------------------------------------------
// Transition entry points — one per state edge
// ---------------------------------------------------------------------------

/// The PTY child has been spawned — flip the node from whatever-it-was to
/// `Spawning`. The forbidden set is the same one the reader thread uses
/// for its competing `Error` write (issue #654) — both writers share
/// [`FORBIDDEN_TERMINAL`] so whichever fires first sticks and the other
/// becomes a no-op.
pub fn on_spawn_started(sink: &dyn SessionLifecycleSink, node_id: i64) -> Result<bool, String> {
    sink.write_status_unless_in(node_id, SessionStatus::Spawning, FORBIDDEN_TERMINAL)
}

/// The reader thread's `EARLY_EXIT_WINDOW` has elapsed without an
/// early-exit `Error` — promote `Spawning → Running`. Conditional so the
/// reader's `Error` write wins if it already fired (issue #654).
/// Returns `true` iff the promotion fired (false = reader won the race).
pub fn on_spawn_complete(sink: &dyn SessionLifecycleSink, node_id: i64) -> Result<bool, String> {
    let promoted =
        sink.write_status_if(node_id, SessionStatus::Running, SessionStatus::Spawning)?;
    if !promoted {
        tracing::warn!(
            "SessionLifecycle::on_spawn_complete: session {node_id} was no longer Spawning \
             (reader early-exit Error write won the race)"
        );
    }
    Ok(promoted)
}

/// The PTY reader thread exited cleanly with no error — mark the node
/// `Idle`. Used to be `db::update_agent_node_status(.., Idle)` in the
/// reader thread's `PostExitAction::MarkIdle` arm
/// (`agent/spawn.rs:848`).
///
/// Emits `agent-lifecycle` kind `SessionExited` (issue #1364): a clean EOF
/// is a real transition the clients must observe without a refetch.
pub fn on_pty_eof(sink: &dyn SessionLifecycleSink, node_id: i64) -> Result<(), String> {
    on_pty_eof_with_detail(sink, node_id, &HookSignalDetail::default())
}

/// [`on_pty_eof`] with a provider-envelope detail (issue #1364). The PTY
/// reader and the attention route both funnel through here so the clean-exit
/// transition is written and emitted from exactly one place.
pub fn on_pty_eof_with_detail(
    sink: &dyn SessionLifecycleSink,
    node_id: i64,
    detail: &HookSignalDetail,
) -> Result<(), String> {
    sink.write_status(node_id, SessionStatus::Idle)?;
    sink.emit_lifecycle_changed(LifecycleChangedPayload::new(
        node_id,
        LifecycleKind::SessionExited,
        SessionStatus::Idle,
        detail,
        "agent process exited cleanly",
    ));
    Ok(())
}

/// A resume attempt failed — write `Error` (unless already terminal)
/// and emit `resume-failed`. Covers both detection paths:
/// the PTY reader thread's early-exit heuristic (process died inside
/// `EARLY_EXIT_WINDOW`, likely an expired `--resume` session) and
/// `auto_resume_agent_nodes`'s `spawn_agent_inner` failure. Both
/// routes must funnel through the same transition so the
/// "exactly one place emits `resume-failed`" invariant holds —
/// callers should never call `sink.emit_resume_failed` directly.
pub fn on_resume_failed(
    sink: &dyn SessionLifecycleSink,
    node_id: i64,
    reason: &str,
) -> Result<(), String> {
    sink.write_status_unless_in(node_id, SessionStatus::Error, FORBIDDEN_TERMINAL)?;
    sink.emit_resume_failed(node_id, reason);
    Ok(())
}

/// The agent yielded control — mark `AwaitingInput` and broadcast
/// `attention-needed`. Replaces `mark_attention` in
/// `commands/attention.rs:40-46`. The autoclear arming lives here too —
/// callers should still call `attention_autoclear::on_marked` directly
/// because that module is a separable safety net, not a status writer.
pub fn on_attention(sink: &dyn SessionLifecycleSink, node_id: i64) -> Result<(), String> {
    on_attention_with_detail(sink, node_id, None)
}

pub fn on_attention_with_detail(
    sink: &dyn SessionLifecycleSink,
    node_id: i64,
    semantic_turn: Option<SemanticTurnPayload>,
) -> Result<(), String> {
    on_attention_with_signal(sink, node_id, semantic_turn, &HookSignalDetail::default())
}

/// [`on_attention_with_detail`] with a full provider envelope (issue #1364).
/// The attention route passes the parsed hook signal so the `agent-lifecycle`
/// event carries the provider event name, session id, reason, transcript
/// path, and health — and the normalized kind distinguishes a permission
/// request or question from a generic input-required yield.
pub fn on_attention_with_signal(
    sink: &dyn SessionLifecycleSink,
    node_id: i64,
    semantic_turn: Option<SemanticTurnPayload>,
    detail: &HookSignalDetail,
) -> Result<(), String> {
    sink.write_status(node_id, SessionStatus::AwaitingInput)?;
    sink.emit_attention_needed_with_payload(node_id, semantic_turn.clone());
    let kind = detail
        .kind
        .unwrap_or(match semantic_turn.as_ref().map(|t| t.kind) {
            Some(SemanticTurnKind::PermissionRequest)
            | Some(SemanticTurnKind::CommandConfirmation) => LifecycleKind::PermissionRequested,
            _ => LifecycleKind::InputRequired,
        });
    let detail = HookSignalDetail {
        semantic_turn,
        signal_health: detail.signal_health,
        ..detail.clone()
    };
    sink.emit_lifecycle_changed(LifecycleChangedPayload::new(
        node_id,
        kind,
        SessionStatus::AwaitingInput,
        &detail,
        "agent is waiting for input",
    ));
    tracing::info!("Node {node_id} awaiting user input (Node Turn)");
    Ok(())
}

/// The user typed into the node (or the autoclear safety net, or
/// another path cleared attention) — flip back to `Running` and
/// broadcast `attention-cleared`. Replaces `clear_attention_node` in
/// `commands/attention.rs:63-74` plus the matching emits in
/// `http/ws.rs:215-218`, `coordinator/drive.rs:197-201`,
/// `autopilot/pipeline.rs:355-357`, `attention_autoclear.rs:104-119`.
pub fn on_attention_cleared(sink: &dyn SessionLifecycleSink, node_id: i64) -> Result<(), String> {
    sink.write_status(node_id, SessionStatus::Running)?;
    sink.emit_attention_cleared(node_id);
    tracing::info!("Node {node_id} attention cleared");
    Ok(())
}

/// A node is being marked `Idle`. Two distinct callers share this
/// transition:
///
/// - **Real kill**: `kill_session` / `kill_all_agents` finished — write
///   `Idle`. The matching `attention-cleared` emit, when the killed
///   node was awaiting input, is the *caller's* responsibility
///   (`on_attention_cleared` covers it).
/// - **Resume skip**: `auto_resume_agent_nodes` decided *not* to spawn
///   this node (no `cli_session_id` / non-resumable adapter) — there
///   is no process to kill, the row simply stays/becomes `Idle`.
///
/// Renamed from the earlier `on_kill` (the second case isn't a kill,
/// so the name overloaded two distinct transitions). Caller-visible
/// intent is the same: "this node has no live process".
pub fn on_idle(sink: &dyn SessionLifecycleSink, node_id: i64) -> Result<(), String> {
    on_idle_with_detail(sink, node_id, &HookSignalDetail::default())
}

/// [`on_idle`] with a provider-envelope detail (issue #1364).
pub fn on_idle_with_detail(
    sink: &dyn SessionLifecycleSink,
    node_id: i64,
    detail: &HookSignalDetail,
) -> Result<(), String> {
    sink.write_status(node_id, SessionStatus::Idle)?;
    sink.emit_lifecycle_changed(LifecycleChangedPayload::new(
        node_id,
        LifecycleKind::ProcessIdle,
        SessionStatus::Idle,
        detail,
        "no live agent process",
    ));
    Ok(())
}

/// A spawn / autopilot run failed — mark `Error`. Used by the
/// autopilot pipeline's `FinishOutcome::Fail` arm. **Not** for resume
/// failures — those go through [`on_resume_failed`] so the
/// `resume-failed` emit is wrapped by the lifecycle entry point, not
/// left to the caller.
pub fn on_error(sink: &dyn SessionLifecycleSink, node_id: i64) -> Result<(), String> {
    on_error_with_detail(sink, node_id, &HookSignalDetail::default())
}

/// [`on_error`] with a provider-envelope detail (issue #1364).
pub fn on_error_with_detail(
    sink: &dyn SessionLifecycleSink,
    node_id: i64,
    detail: &HookSignalDetail,
) -> Result<(), String> {
    let wrote = sink.write_status_unless_in(node_id, SessionStatus::Error, FORBIDDEN_TERMINAL)?;
    if wrote {
        sink.emit_lifecycle_changed(LifecycleChangedPayload::new(
            node_id,
            LifecycleKind::Error,
            SessionStatus::Error,
            detail,
            "agent process error",
        ));
    }
    Ok(())
}

/// Autopilot finish verified — mark `Completed`. Replaces
/// `autopilot/pipeline.rs:646`. Emits kind `AutopilotCompleted` — a distinct
/// normalized kind, so "terminal PR-opened" is never confused with an
/// ordinary `TurnCompleted` (issue #1364 review).
pub fn on_completed(sink: &dyn SessionLifecycleSink, node_id: i64) -> Result<(), String> {
    sink.write_status(node_id, SessionStatus::Completed)?;
    sink.emit_lifecycle_changed(LifecycleChangedPayload::new(
        node_id,
        LifecycleKind::AutopilotCompleted,
        SessionStatus::Completed,
        &HookSignalDetail::default(),
        "Autopilot wrap-up verified and PR opened",
    ));
    Ok(())
}

/// An ordinary turn finished and the agent is at its prompt, ready for
/// another prompt (issue #1364). Writes the new `Ready` status — distinct
/// from `AwaitingInput` (the user is NOT needed) and from `Completed`
/// (Autopilot's PR-opened terminal state) — and emits `agent-lifecycle`
/// kind `TurnCompleted`. The attention route calls this when a hook
/// callback shows a clean turn with no pending background work.
pub fn on_turn_completed(
    sink: &dyn SessionLifecycleSink,
    node_id: i64,
    detail: &HookSignalDetail,
) -> Result<(), String> {
    sink.write_status(node_id, SessionStatus::Ready)?;
    sink.emit_lifecycle_changed(LifecycleChangedPayload::new(
        node_id,
        LifecycleKind::TurnCompleted,
        SessionStatus::Ready,
        detail,
        "turn finished — agent is ready for another prompt",
    ));
    tracing::info!("Node {node_id} finished its turn (Ready)");
    Ok(())
}

/// A turn ended but background work is still running (false yield, issue
/// #878). No status write — the node stays `Running` — but the transition
/// is observable: emit `agent-lifecycle` kind `BackgroundRunning` so both
/// clients can distinguish "busy on background work" from "waiting for
/// input" without polling.
pub fn on_background_running(
    sink: &dyn SessionLifecycleSink,
    node_id: i64,
    detail: &HookSignalDetail,
) -> Result<(), String> {
    sink.emit_lifecycle_changed(LifecycleChangedPayload::new(
        node_id,
        LifecycleKind::BackgroundRunning,
        SessionStatus::Running,
        detail,
        "agent busy on background work",
    ));
    Ok(())
}

/// Initial node creation — mark `Pending`. Used by
/// `services/agent_node.rs::create_node` (line 251) and the autopilot
/// gate's `Suspended` initial status. Note: `Pending` and `Suspended`
/// are both "creation but not yet started" — kept as separate
/// transitions because the autopilot gate's `Suspended` is its own
/// user-meaningful state ("waiting for approval").
pub fn on_created(sink: &dyn SessionLifecycleSink, node_id: i64) -> Result<(), String> {
    sink.write_status(node_id, SessionStatus::Pending)
}

/// Crash recovery sweep — runs once at startup. Any node still marked
/// `Running` has no live process (a crash means no graceful shutdown).
/// Mark them `Suspended` so `auto_resume_nodes` can pick them up on
/// the frontend's first draw. Replaces the inline
/// `db::mark_running_nodes_suspended()` call in `lib.rs:265`.
///
/// Not a per-node transition, so it doesn't take the sink — the
/// frontend treats a sudden `Suspended` batch as "the app restarted"
/// and re-fetches, no per-node events are needed.
pub fn recover_from_crash() -> Result<usize, String> {
    db::mark_running_nodes_suspended().map_err(|e| e.to_string())
}

/// Exit-time sweep — runs once on `RunEvent::ExitRequested`. Mark every
/// `Running` node `Suspended` so a future startup can offer to resume
/// it. Sibling to [`recover_from_crash`]; same one-line DB call inside,
/// distinct name so the trigger (graceful shutdown vs crash) stays
/// distinguishable in logs and history (issue #949).
///
/// Not a per-node transition, so it doesn't take the sink — the
/// frontend treats a sudden `Suspended` batch identically to the
/// startup-recovery case.
pub fn on_exit_sweep() -> Result<usize, String> {
    db::mark_running_nodes_suspended().map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Records every status write and emit so a test can assert the
    /// single transition produced exactly one DB write + exactly one
    /// event emit (in that order). Pattern mirrors `FakeSink` in
    /// `commands/attention.rs` (the previous `AttentionSink` test
    /// fixture).
    #[derive(Default)]
    struct FakeSink {
        writes: RefCell<Vec<(i64, SessionStatus)>>,
        writes_if: RefCell<Vec<(i64, SessionStatus, SessionStatus)>>,
        writes_unless: RefCell<Vec<(i64, SessionStatus, Vec<SessionStatus>)>>,
        attention_needed: RefCell<Vec<i64>>,
        attention_cleared: RefCell<Vec<i64>>,
        resume_failed: RefCell<Vec<(i64, String)>>,
        lifecycle_changed: RefCell<Vec<LifecycleChangedPayload>>,
    }

    impl SessionLifecycleSink for FakeSink {
        fn write_status(&self, node_id: i64, new: SessionStatus) -> Result<(), String> {
            self.writes.borrow_mut().push((node_id, new));
            Ok(())
        }
        fn write_status_if(
            &self,
            node_id: i64,
            new: SessionStatus,
            expected: SessionStatus,
        ) -> Result<bool, String> {
            self.writes_if.borrow_mut().push((node_id, new, expected));
            Ok(true)
        }
        fn write_status_unless_in(
            &self,
            node_id: i64,
            new: SessionStatus,
            forbidden: &[SessionStatus],
        ) -> Result<bool, String> {
            self.writes_unless
                .borrow_mut()
                .push((node_id, new, forbidden.to_vec()));
            Ok(true)
        }
        fn emit_attention_needed(&self, node_id: i64) {
            self.attention_needed.borrow_mut().push(node_id);
        }
        fn emit_attention_cleared(&self, node_id: i64) {
            self.attention_cleared.borrow_mut().push(node_id);
        }
        fn emit_resume_failed(&self, node_id: i64, reason: &str) {
            self.resume_failed
                .borrow_mut()
                .push((node_id, reason.to_string()));
        }
        fn emit_lifecycle_changed(&self, payload: LifecycleChangedPayload) {
            self.lifecycle_changed.borrow_mut().push(payload);
        }
    }

    // -----------------------------------------------------------------------
    // One transition = one write + (optional) one emit.
    // The headlining acceptance criterion: "Tauri events … are emitted from
    // exactly one place."
    // -----------------------------------------------------------------------

    #[test]
    fn on_spawn_started_writes_spawning_with_forbidden_set() {
        let sink = FakeSink::default();
        on_spawn_started(&sink, 7).unwrap();
        let w = sink.writes_unless.borrow();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].0, 7);
        assert_eq!(w[0].1, SessionStatus::Spawning);
        assert_eq!(
            w[0].2,
            vec![SessionStatus::Error, SessionStatus::Archived],
            "forbidden set must match the reader thread's race-guard (#654)"
        );
    }

    #[test]
    fn on_spawn_complete_writes_running_only_if_currently_spawning() {
        let sink = FakeSink::default();
        let promoted = on_spawn_complete(&sink, 7).unwrap();
        assert!(
            promoted,
            "on_spawn_complete must report success on a happy path"
        );
        let w = sink.writes_if.borrow();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0], (7, SessionStatus::Running, SessionStatus::Spawning));
    }

    #[test]
    fn on_pty_eof_writes_idle_and_emits_session_exited() {
        let sink = FakeSink::default();
        on_pty_eof(&sink, 7).unwrap();
        assert_eq!(
            *sink.writes.borrow(),
            vec![(7, SessionStatus::Idle)],
            "PTY EOF is a clean exit → Idle"
        );
        assert!(sink.attention_needed.borrow().is_empty());
        assert!(sink.attention_cleared.borrow().is_empty());
        let events = sink.lifecycle_changed.borrow();
        assert_eq!(
            events.len(),
            1,
            "clean EOF must emit agent-lifecycle (issue #1364)"
        );
        assert_eq!(events[0].session_id, 7);
        assert_eq!(events[0].kind, LifecycleKind::SessionExited);
        assert_eq!(events[0].status, SessionStatus::Idle);
    }

    #[test]
    fn on_turn_completed_writes_ready_and_emits_turn_completed() {
        let sink = FakeSink::default();
        on_turn_completed(&sink, 7, &HookSignalDetail::default()).unwrap();
        assert_eq!(
            *sink.writes.borrow(),
            vec![(7, SessionStatus::Ready)],
            "a clean turn completion must land in Ready, never Completed (issue #1364)"
        );
        let events = sink.lifecycle_changed.borrow();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, LifecycleKind::TurnCompleted);
        assert_eq!(events[0].status, SessionStatus::Ready);
        assert!(
            sink.attention_needed.borrow().is_empty(),
            "Ready must not emit attention-needed"
        );
    }

    #[test]
    fn on_background_running_emits_without_writing_status() {
        let sink = FakeSink::default();
        on_background_running(&sink, 7, &HookSignalDetail::default()).unwrap();
        assert!(
            sink.writes.borrow().is_empty(),
            "background yield must not change status"
        );
        let events = sink.lifecycle_changed.borrow();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, LifecycleKind::BackgroundRunning);
        assert_eq!(events[0].status, SessionStatus::Running);
    }

    #[test]
    fn on_attention_with_detail_emits_permission_requested_for_permission_turn() {
        let sink = FakeSink::default();
        let turn = SemanticTurnPayload {
            node_id: 7,
            kind: SemanticTurnKind::PermissionRequest,
            description: "Allow edit: src/lib/auth.ts".into(),
        };
        on_attention_with_detail(&sink, 7, Some(turn)).unwrap();
        let events = sink.lifecycle_changed.borrow();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, LifecycleKind::PermissionRequested);
        assert_eq!(events[0].status, SessionStatus::AwaitingInput);
        assert_eq!(
            events[0].semantic_turn.as_ref().map(|t| t.kind),
            Some(SemanticTurnKind::PermissionRequest)
        );
    }

    #[test]
    fn on_idle_emits_process_idle() {
        let sink = FakeSink::default();
        on_idle(&sink, 7).unwrap();
        assert_eq!(*sink.writes.borrow(), vec![(7, SessionStatus::Idle)]);
        let events = sink.lifecycle_changed.borrow();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, LifecycleKind::ProcessIdle);
    }

    #[test]
    fn on_error_emits_error_lifecycle() {
        let sink = FakeSink::default();
        on_error(&sink, 7).unwrap();
        let events = sink.lifecycle_changed.borrow();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, LifecycleKind::Error);
        assert_eq!(events[0].status, SessionStatus::Error);
    }

    #[test]
    fn on_completed_emits_autopilot_completed_with_completed_status() {
        let sink = FakeSink::default();
        on_completed(&sink, 7).unwrap();
        let events = sink.lifecycle_changed.borrow();
        assert_eq!(events.len(), 1);
        // A distinct kind: "terminal PR-opened" must never be confused with
        // an ordinary TurnCompleted (issue #1364 review).
        assert_eq!(events[0].kind, LifecycleKind::AutopilotCompleted);
        assert_eq!(events[0].status, SessionStatus::Completed);
        assert_ne!(events[0].kind, LifecycleKind::TurnCompleted);
    }

    #[test]
    fn on_resume_failed_writes_error_with_forbidden_set_and_emits_resume_failed() {
        let sink = FakeSink::default();
        on_resume_failed(&sink, 7, "session expired").unwrap();
        let w = sink.writes_unless.borrow();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].0, 7);
        assert_eq!(w[0].1, SessionStatus::Error);
        assert_eq!(
            w[0].2,
            vec![SessionStatus::Error, SessionStatus::Archived],
            "early-exit must not resurrect Error/Archived (#654)"
        );
        assert_eq!(
            *sink.resume_failed.borrow(),
            vec![(7, "session expired".to_string())],
            "early exit must emit resume-failed exactly once with the given reason"
        );
    }

    #[test]
    fn on_attention_writes_awaiting_input_and_emits_attention_needed() {
        let sink = FakeSink::default();
        on_attention(&sink, 7).unwrap();
        assert_eq!(
            *sink.writes.borrow(),
            vec![(7, SessionStatus::AwaitingInput)]
        );
        assert_eq!(*sink.attention_needed.borrow(), vec![7]);
        assert!(
            sink.attention_cleared.borrow().is_empty(),
            "attention mark must not also emit attention-cleared"
        );
        assert!(
            sink.resume_failed.borrow().is_empty(),
            "attention mark must not emit resume-failed"
        );
    }

    #[test]
    fn on_attention_cleared_writes_running_and_emits_attention_cleared() {
        let sink = FakeSink::default();
        on_attention_cleared(&sink, 7).unwrap();
        assert_eq!(*sink.writes.borrow(), vec![(7, SessionStatus::Running)]);
        assert_eq!(*sink.attention_cleared.borrow(), vec![7]);
        assert!(
            sink.attention_needed.borrow().is_empty(),
            "clearing attention must not emit attention-needed"
        );
    }

    #[test]
    fn on_error_writes_error_with_forbidden_set_and_emits_nothing() {
        let sink = FakeSink::default();
        on_error(&sink, 7).unwrap();
        let w = sink.writes_unless.borrow();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].1, SessionStatus::Error);
        assert_eq!(w[0].2, vec![SessionStatus::Error, SessionStatus::Archived]);
        assert!(sink.attention_needed.borrow().is_empty());
        assert!(sink.attention_cleared.borrow().is_empty());
        assert!(sink.resume_failed.borrow().is_empty());
    }

    #[test]
    fn on_completed_writes_completed_unconditionally() {
        let sink = FakeSink::default();
        on_completed(&sink, 7).unwrap();
        assert_eq!(*sink.writes.borrow(), vec![(7, SessionStatus::Completed)]);
        assert!(sink.attention_needed.borrow().is_empty());
        assert!(sink.attention_cleared.borrow().is_empty());
    }

    #[test]
    fn on_created_writes_pending() {
        let sink = FakeSink::default();
        on_created(&sink, 7).unwrap();
        assert_eq!(*sink.writes.borrow(), vec![(7, SessionStatus::Pending)]);
    }

    // -----------------------------------------------------------------------
    // recover_from_crash delegates to db::mark_running_nodes_suspended().
    // The DB sweep itself is pinned by `db::agent_node_tests.rs` — this
    // module only owns the wiring (and accepts the AppSessionLifecycleSink
    // for symmetry with the other transitions, even though the sweep
    // doesn't emit any events).
    // -----------------------------------------------------------------------
}
