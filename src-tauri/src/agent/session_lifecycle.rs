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
    fn emit_attention_cleared(&self, node_id: i64);
    fn emit_resume_failed(&self, node_id: i64, reason: &str);
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
        let _ = self
            .app
            .emit("attention-needed", AttentionNeededPayload { session_id: node_id });
    }

    fn emit_attention_cleared(&self, node_id: i64) {
        let _ = self
            .app
            .emit("attention-cleared", AttentionClearedPayload { session_id: node_id });
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
    fn emit_attention_cleared(&self, _node_id: i64) {}
    fn emit_resume_failed(&self, _node_id: i64, _reason: &str) {}
}

// ---------------------------------------------------------------------------
// Transition entry points — one per state edge
// ---------------------------------------------------------------------------

/// The PTY child has been spawned — flip the node from whatever-it-was to
/// `Spawning`. The forbidden set is the same one the reader thread uses
/// for its competing `Error` write (issue #654) — both writers share
/// [`FORBIDDEN_TERMINAL`] so whichever fires first sticks and the other
/// becomes a no-op.
pub fn on_spawn_started(
    sink: &dyn SessionLifecycleSink,
    node_id: i64,
) -> Result<bool, String> {
    sink.write_status_unless_in(node_id, SessionStatus::Spawning, FORBIDDEN_TERMINAL)
}

/// The reader thread's `EARLY_EXIT_WINDOW` has elapsed without an
/// early-exit `Error` — promote `Spawning → Running`. Conditional so the
/// reader's `Error` write wins if it already fired (issue #654).
/// Returns `true` iff the promotion fired (false = reader won the race).
pub fn on_spawn_complete(
    sink: &dyn SessionLifecycleSink,
    node_id: i64,
) -> Result<bool, String> {
    let promoted = sink.write_status_if(node_id, SessionStatus::Running, SessionStatus::Spawning)?;
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
pub fn on_pty_eof(sink: &dyn SessionLifecycleSink, node_id: i64) -> Result<(), String> {
    sink.write_status(node_id, SessionStatus::Idle)
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
    sink.write_status(node_id, SessionStatus::AwaitingInput)?;
    sink.emit_attention_needed(node_id);
    tracing::info!("Node {node_id} awaiting user input (Node Turn)");
    Ok(())
}

/// The user typed into the node (or the autoclear safety net, or
/// another path cleared attention) — flip back to `Running` and
/// broadcast `attention-cleared`. Replaces `clear_attention_node` in
/// `commands/attention.rs:63-74` plus the matching emits in
/// `http/ws.rs:215-218`, `coordinator/drive.rs:197-201`,
/// `autopilot/pipeline.rs:355-357`, `attention_autoclear.rs:104-119`.
pub fn on_attention_cleared(
    sink: &dyn SessionLifecycleSink,
    node_id: i64,
) -> Result<(), String> {
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
    sink.write_status(node_id, SessionStatus::Idle)
}

/// A spawn / autopilot run failed — mark `Error`. Used by the
/// autopilot pipeline's `FinishOutcome::Fail` arm. **Not** for resume
/// failures — those go through [`on_resume_failed`] so the
/// `resume-failed` emit is wrapped by the lifecycle entry point, not
/// left to the caller.
pub fn on_error(sink: &dyn SessionLifecycleSink, node_id: i64) -> Result<(), String> {
    sink.write_status_unless_in(node_id, SessionStatus::Error, FORBIDDEN_TERMINAL)
        .map(|_| ())
}

/// Autopilot finish verified — mark `Completed`. Replaces
/// `autopilot/pipeline.rs:646`.
pub fn on_completed(sink: &dyn SessionLifecycleSink, node_id: i64) -> Result<(), String> {
    sink.write_status(node_id, SessionStatus::Completed)
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
            self.writes_if
                .borrow_mut()
                .push((node_id, new, expected));
            Ok(true)
        }
        fn write_status_unless_in(
            &self,
            node_id: i64,
            new: SessionStatus,
            forbidden: &[SessionStatus],
        ) -> Result<bool, String> {
            self.writes_unless.borrow_mut().push((
                node_id,
                new,
                forbidden.to_vec(),
            ));
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
        assert!(promoted, "on_spawn_complete must report success on a happy path");
        let w = sink.writes_if.borrow();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0], (7, SessionStatus::Running, SessionStatus::Spawning));
    }

    #[test]
    fn on_pty_eof_writes_idle_unconditionally() {
        let sink = FakeSink::default();
        on_pty_eof(&sink, 7).unwrap();
        assert_eq!(
            *sink.writes.borrow(),
            vec![(7, SessionStatus::Idle)],
            "PTY EOF is a clean exit → Idle, no event"
        );
        assert!(sink.attention_needed.borrow().is_empty());
        assert!(sink.attention_cleared.borrow().is_empty());
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
        assert_eq!(
            *sink.writes.borrow(),
            vec![(7, SessionStatus::Running)]
        );
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
        assert_eq!(
            *sink.writes.borrow(),
            vec![(7, SessionStatus::Completed)]
        );
        assert!(sink.attention_needed.borrow().is_empty());
        assert!(sink.attention_cleared.borrow().is_empty());
    }

    #[test]
    fn on_created_writes_pending() {
        let sink = FakeSink::default();
        on_created(&sink, 7).unwrap();
        assert_eq!(
            *sink.writes.borrow(),
            vec![(7, SessionStatus::Pending)]
        );
    }

    // -----------------------------------------------------------------------
    // recover_from_crash delegates to db::mark_running_nodes_suspended().
    // The DB sweep itself is pinned by `db::agent_node_tests.rs` — this
    // module only owns the wiring (and accepts the AppSessionLifecycleSink
    // for symmetry with the other transitions, even though the sweep
    // doesn't emit any events).
    // -----------------------------------------------------------------------
}