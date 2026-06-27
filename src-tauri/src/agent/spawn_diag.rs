//! Concurrent-spawn diagnostic instrumentation (temporary, see TODOs).
//!
//! Goal: capture enough context in production logs that a future analysis
//! session can identify the race(s) that cause "spawn-many-at-once" failures.
//! The user's reports describe several distinct symptoms — spawn never
//! finishes, worktree missing, "conversation not found" on auto-resume,
//! invalid node left behind — and the only signal a static read gave us
//! was a path-collision error in the dev profile log:
//!
//!     failed to resolve path 'X:\\src\\buildmesh/.claude/worktrees/<slug>':
//!     The system cannot find the file specified.
//!
//! Static reasoning ruled out PROCESS_REGISTRY races (it's mutex-guarded) and
//! the obvious `git fetch` / `git worktree add` lock collisions (local repro
//! at N=10 produced zero failures). What's left are timing-dependent races
//! that only reproduce against a real GitHub remote + warm pool + the full
//! spawn pipeline, so the cheapest way forward is to instrument the live
//! path and let the next repro session grep the log.
//!
//! **Every line this module emits is prefixed `[DEBUG-concurrent-spawn]`** so
//! a single `grep` filters to just the diagnostic stream. When the real bug
//! is found, this module + the call sites are deleted in one pass
//! (`grep -rn "DEBUG-concurrent-spawn" src-tauri/src`).
//!
//! What gets captured:
//! - `concurrent_spawn_in_flight` — process-global atomic counter; logged at
//!   every spawn entry/exit. If you see N=5+ in flight, that's the burst
//!   the user is reporting on.
//! - `concurrent_spawn_timeline` — per-session, per-phase wallclock delta
//!   from spawn start. Mirrors the existing `spawn_timing:` checkpoints but
//!   keeps the diagnostic stream separate so the two can be diffed.
//! - `concurrent_spawn_db_write` — every per-session DB write that affects
//!   spawn correctness: `cli_session_id`, `status`, `worktree_name`,
//!   `name`. Logged with the spawn-context so we can see e.g. two threads
//!   racing to write the same field.
//! - `concurrent_spawn_reader_event` — when the PTY reader thread captures
//!   a session-id from PTY output, with the elapsed wallclock since
//!   `spawn_start`. A captured-after-spawn-completed or captured-with-the-
//!   wrong-uuid value is the smoking gun for the "conversation not found"
//!   symptom.
//! - `concurrent_spawn_warm_claim` — warm-pool claim attempt + outcome,
//!   with the per-mesh counter so concurrent claimers are visible.
//!
//! When to delete this module: when a real concurrent-spawn failure is
//! reproduced, root-caused, fixed, and the fix has a regression test in
//! the suite. Until then, the cost is one extra log line per spawn — and
//! the dev profile log already proves a session burst (~5 lines/100ms)
//! is comfortably under any rate limit.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// Process-global count of in-flight `spawn_agent_inner` calls. Used by the
/// `concurrent_spawn_timeline` line so a log reader can see N parallel
/// spawns happening at the same wallclock instant. Not a guard — purely
/// observational; spawns are not serialized on this counter (the warm-pool
/// DB mutex + git's per-process file locks are what actually serialize).
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// RAII guard that increments `IN_FLIGHT` on construction and decrements
/// on drop. Spawn returns can be `Ok(())` or `Err(String)` early at any of
/// the `?` points; a guard-on-stack ensures the counter always decreases
/// regardless of which return path the function took.
///
/// The label is included in the entry log line so a single grep can pull
/// just one spawn's timeline.
pub(super) struct InFlightGuard {
    label: &'static str,
    session_id: i64,
    started_at: Instant,
}

impl InFlightGuard {
    pub(super) fn enter(session_id: i64, label: &'static str) -> Self {
        let n = IN_FLIGHT.fetch_add(1, Ordering::SeqCst) + 1;
        tracing::info!(
            "[DEBUG-concurrent-spawn] phase=enter session={} label={} in_flight={} elapsed_from_label_start=0ms",
            session_id,
            label,
            n,
        );
        Self {
            label,
            session_id,
            started_at: Instant::now(),
        }
    }

    /// Emit a timeline checkpoint relative to this guard's start instant.
    /// The label suffix lets a reader pick out one specific checkpoint
    /// across many spawns (e.g. `[DEBUG-concurrent-spawn] phase=before_fetch
    /// session=42 label=manual-spawn elapsed_from_label_start=85ms`).
    pub(super) fn checkpoint(&self, phase: &'static str) {
        tracing::info!(
            "[DEBUG-concurrent-spawn] phase={} session={} label={} elapsed_from_label_start={}ms in_flight={}",
            phase,
            self.session_id,
            self.label,
            self.started_at.elapsed().as_millis(),
            IN_FLIGHT.load(Ordering::SeqCst),
        );
    }

    /// Emit a DB-write event with the wallclock from this guard's start.
    /// `field` is the column name; `value` is the new value (truncated to
    /// keep the line bounded). Used for the per-session writes that the
    /// concurrent-spawn audit cares about: `cli_session_id`, `status`,
    /// `worktree_name`, `name`.
    pub(super) fn db_write(&self, field: &str, value: &str) {
        let truncated: String = value.chars().take(48).collect();
        tracing::info!(
            "[DEBUG-concurrent-spawn] phase=db_write session={} label={} field={} value={} elapsed_from_label_start={}ms in_flight={}",
            self.session_id,
            self.label,
            field,
            truncated,
            self.started_at.elapsed().as_millis(),
            IN_FLIGHT.load(Ordering::SeqCst),
        );
    }

    /// Emit a git command event (fetch, pull, worktree-add, checkout, etc.)
    /// with start/end ms timestamps relative to this guard. The `cmd` arg
    /// is a short tag (`fetch_origin`, `worktree_add`, `worktree_move`,
    /// `git_pull`); the `outcome` is `start`, `ok`, or `err:<message>`.
    pub(super) fn git_event(&self, cmd: &'static str, outcome: &str) {
        tracing::info!(
            "[DEBUG-concurrent-spawn] phase=git_cmd session={} label={} cmd={} outcome={} elapsed_from_label_start={}ms in_flight={}",
            self.session_id,
            self.label,
            cmd,
            outcome,
            self.started_at.elapsed().as_millis(),
            IN_FLIGHT.load(Ordering::SeqCst),
        );
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let n = IN_FLIGHT.fetch_sub(1, Ordering::SeqCst) - 1;
        tracing::info!(
            "[DEBUG-concurrent-spawn] phase=exit session={} label={} total_elapsed={}ms in_flight={}",
            self.session_id,
            self.label,
            self.started_at.elapsed().as_millis(),
            n,
        );
    }
}

/// Emit a one-shot session-id-capture event from the PTY reader thread.
/// `source` is `pty_output` (the regex matched) or `db_write` (the post-write
/// confirmation); `uuid_len` is the captured UUID's length (a sanity check
/// that the regex picked up a full UUID and not a fragment).
///
/// Called from `start_reader` in `spawn.rs` (PTY-reader thread, NOT the
/// spawn orchestrator) so we can correlate the reader's clock against the
/// orchestrator's. A capture-from-output that fires AFTER
/// `spawn_agent_inner` returned Err is the precise failure mode for
/// "conversation not found on auto-resume" — the row exists with a UUID
/// the agent never actually claimed.
pub(super) fn reader_event(session_id: i64, source: &'static str, uuid: &str) {
    tracing::info!(
        "[DEBUG-concurrent-spawn] phase=reader_event session={} source={} uuid_len={} uuid_prefix={}",
        session_id,
        source,
        uuid.len(),
        &uuid.chars().take(8).collect::<String>(),
    );
}

/// Emit a warm-pool claim attempt/outcome event. Called from
/// `try_claim` so concurrent claimers against the same mesh show up in
/// the log with matching timestamps.
pub(super) fn warm_claim_event(mesh_id: i64, session_id: i64, outcome: &str) {
    tracing::info!(
        "[DEBUG-concurrent-spawn] phase=warm_claim mesh={} session={} outcome={} in_flight={}",
        mesh_id,
        session_id,
        outcome,
        IN_FLIGHT.load(Ordering::SeqCst),
    );
}