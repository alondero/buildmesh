//! Background pool manager — idle refills & ref freshness (issue #613, PRD #608 §4).
//!
//! Why this exists
//! ---------------
//! The warm pool's first tranches (#609–#612) fill the pool only on two
//! events: once at startup (`warm_pool::reconcile_on_startup`) and once per
//! claim (`warm_pool::refill_after_claim`). Nothing keeps it topped up while
//! the app sits idle, and nothing brings a warm worktree forward when the
//! mesh's base ref moves on the remote — a worktree cut yesterday stays pinned
//! to yesterday's commit until it's claimed. #613 adds the two missing pieces:
//!
//!   1. A debounced **background worker** that maintains every mesh's pool to
//!      its `pre_spawn_pool_size` target while the app is idle (no terminal
//!      output / keypresses for `IDLE_SILENCE`).
//!   2. A **post-fetch ref-freshness** pass that `git reset --hard`s warm
//!      worktrees onto the new base SHA after a fetch advances it.
//!
//! Three globals, one rule: serialize every pool mutation
//! ------------------------------------------------------
//! * `LAST_ACTIVITY` — the most recent terminal output / keypress instant.
//!   `note_activity()` is called from the PTY read loop (output) and the input
//!   write path (keypresses). The background worker only runs a pass once
//!   `idle_duration()` exceeds `IDLE_SILENCE`, so a `git worktree add` never
//!   competes with an agent that is actively producing output or a user who is
//!   typing (issue #613 AC2).
//! * `FILL_LOCK` — a `try_lock`-or-skip guard. EVERY pool-mutating background
//!   path (the idle worker, `refill_after_claim`, and the freshness pass)
//!   takes it through `try_with_fill_lock`, so N concurrent spawns can never
//!   run two `git worktree add` fills at once (issue #613 AC4). `try_lock`
//!   (not a blocking lock) means a second trigger that arrives mid-fill simply
//!   skips — the in-flight fill already drives the pool to target, and the
//!   2-second idle tick re-checks shortly after, so nothing is lost.
//!
//! The decision logic (`is_idle_enough`) is a pure function so the debounce is
//! unit-testable without sleeping, mirroring `warm_pool`'s dependency-injected
//! `reconcile_warm_entries`.

use once_cell::sync::Lazy;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Minimum silence (no terminal output / keypresses) before the background
/// worker is allowed to run a refill pass (issue #613 AC2: "triggering only
/// after 5 seconds of silence").
pub const IDLE_SILENCE: Duration = Duration::from_secs(5);

/// How often the background worker wakes to re-check idleness. Smaller than
/// `IDLE_SILENCE` so the worker fires within one tick of the silence window
/// elapsing, without busy-spinning.
const TICK: Duration = Duration::from_secs(2);

/// Most recent terminal-output / keypress instant. Seeded to "now" at first
/// access so a freshly-launched app counts as "just active" — the startup
/// reconcile already filled the pool, so the worker has no reason to fire in
/// the first `IDLE_SILENCE` window anyway.
static LAST_ACTIVITY: Lazy<Mutex<Instant>> = Lazy::new(|| Mutex::new(Instant::now()));

/// Serializes every background pool mutation (fill + freshness). See the
/// module docs: `try_lock`-or-skip, never a blocking lock.
static FILL_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// Record terminal/process activity. Called from the PTY read loop (agent
/// output) and the PTY input write path (keypresses). Deliberately cheap — a
/// single mutex-guarded `Instant` write on a hot path — and lock-poison-safe
/// (a poisoned lock just means a previous writer panicked mid-store; the
/// timestamp is a single value with no invariant to corrupt, so we recover the
/// guard and overwrite it).
pub fn note_activity() {
    let now = Instant::now();
    match LAST_ACTIVITY.lock() {
        Ok(mut t) => *t = now,
        Err(poisoned) => *poisoned.into_inner() = now,
    }
}

/// How long since the last recorded activity. Lock-poison-safe (see
/// `note_activity`).
fn idle_duration() -> Duration {
    match LAST_ACTIVITY.lock() {
        Ok(t) => t.elapsed(),
        Err(poisoned) => poisoned.into_inner().elapsed(),
    }
}

/// Pure debounce gate, extracted so the "5 seconds of silence" rule is
/// unit-testable without sleeping. `true` ⇒ the app has been quiet long enough
/// for a background refill to run.
pub fn is_idle_enough(idle: Duration, min_silence: Duration) -> bool {
    idle >= min_silence
}

/// Run `f` under the global fill lock IFF no other background pool mutation is
/// in progress (issue #613 AC4 — serialized execution). Returns `true` when
/// `f` ran, `false` when the lock was already held and `f` was skipped.
///
/// Uses `try_lock` rather than a blocking lock so a burst of triggers (e.g. 5
/// concurrent spawns each calling `refill_after_claim`) collapses to a single
/// fill instead of queueing 5 sequential ones — the one that wins the lock
/// drives the pool to target for everyone. A `Poisoned` lock (a prior holder
/// panicked) is recovered rather than propagated: the guarded unit `()` has no
/// state to be left inconsistent, so it is always safe to proceed.
pub fn try_with_fill_lock(f: impl FnOnce()) -> bool {
    use std::sync::TryLockError;
    match FILL_LOCK.try_lock() {
        Ok(_guard) => {
            f();
            true
        }
        Err(TryLockError::WouldBlock) => false,
        Err(TryLockError::Poisoned(guard)) => {
            // Recover the guard and proceed — `()` cannot be corrupted.
            let _guard = guard.into_inner();
            f();
            true
        }
    }
}

/// Start the background pool-maintenance worker (issue #613 AC1).
///
/// Runs on a dedicated OS thread (matching `reconcile_on_startup`'s thread in
/// `lib.rs::setup`) rather than a tokio task, because the work it drives is
/// blocking `git` CLI / DB calls and we don't want to tie up an async runtime
/// worker for the app's lifetime. The loop:
///
///   1. sleeps one `TICK`,
///   2. skips the pass if the app has been active within `IDLE_SILENCE`
///      (debounce — AC2),
///   3. otherwise runs one `warm_pool::maintain_all_pools` pass under the fill
///      lock (serialized — AC4). If the lock is held (a `refill_after_claim`
///      or freshness pass is mid-flight) the tick is skipped and retried on
///      the next one.
///
/// Best-effort and infallible from the caller's perspective — every error
/// inside the pass is logged and swallowed by `maintain_all_pools`, so the
/// loop never dies.
///
/// Known limitation (issue #613 review, follow-up filed): `LAST_ACTIVITY` is a
/// single GLOBAL instant, so one continuously-chatty agent (a streaming build
/// log) keeps the whole app non-idle and this worker never ticks. That only
/// starves the *secondary* safety-net maintenance — the primary refill runs
/// inline on the spawn path (`warm_pool::post_spawn_maintenance`, not idle-
/// gated), so pools still top up after spawns during activity. A scoped
/// (per-mesh) activity signal or a max-staleness override would lift this; it's
/// deferred to keep this tranche focused.
pub fn start_background_worker() {
    std::thread::Builder::new()
        .name("warm-pool-worker".to_string())
        .spawn(|| loop {
            std::thread::sleep(TICK);
            if !is_idle_enough(idle_duration(), IDLE_SILENCE) {
                continue;
            }
            try_with_fill_lock(crate::services::warm_pool::maintain_all_pools);
        })
        .expect("failed to spawn warm-pool-worker thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- is_idle_enough (the debounce gate) ----

    #[test]
    fn not_idle_below_the_silence_window() {
        assert!(
            !is_idle_enough(Duration::from_secs(3), IDLE_SILENCE),
            "3s of silence is below the 5s window — a refill must NOT fire"
        );
    }

    #[test]
    fn idle_at_exactly_the_silence_window() {
        // Boundary: exactly `IDLE_SILENCE` counts as idle (>=) so a window that
        // lands precisely on the threshold still fires rather than waiting a
        // whole extra tick.
        assert!(
            is_idle_enough(IDLE_SILENCE, IDLE_SILENCE),
            "silence equal to the window must be treated as idle"
        );
    }

    #[test]
    fn idle_past_the_silence_window() {
        assert!(
            is_idle_enough(Duration::from_secs(30), IDLE_SILENCE),
            "30s of silence is well past the window — a refill must fire"
        );
    }

    // ---- note_activity / idle_duration ----

    #[test]
    fn note_activity_resets_the_idle_clock() {
        note_activity();
        // Immediately after recording activity the idle duration must be tiny —
        // far below the silence window — so the gate reports "not idle".
        assert!(
            !is_idle_enough(idle_duration(), IDLE_SILENCE),
            "right after note_activity the app must read as active"
        );
    }

    // ---- try_with_fill_lock (serialized execution, AC4) ----
    //
    // Both the "runs when free" and "skips a reentrant attempt" assertions live
    // in ONE test: `FILL_LOCK` is process-global, so two separate `#[test]` fns
    // touching it would race under cargo's parallel test runner (one holding
    // the lock while the other asserts it acquired). Keeping them sequential in
    // a single test removes that flakiness — no other test touches the lock.

    #[test]
    fn fill_lock_runs_when_free_and_skips_a_reentrant_attempt() {
        // 1. Uncontended: the body runs.
        let mut ran = false;
        let acquired = try_with_fill_lock(|| ran = true);
        assert!(acquired, "an uncontended lock must run the body");
        assert!(ran, "the body must have executed");

        // 2. Reentrant: while the outer closure holds the lock, a nested
        // attempt must be skipped (returns false, body not run) rather than
        // deadlocking or running a second concurrent fill — the exact shape of
        // "high-concurrency spawns" all racing to refill.
        let mut inner_ran = false;
        let outer = try_with_fill_lock(|| {
            let inner = try_with_fill_lock(|| inner_ran = true);
            assert!(
                !inner,
                "a nested attempt while the lock is held must be skipped"
            );
        });
        assert!(outer, "the outer attempt must have acquired the lock");
        assert!(
            !inner_ran,
            "the nested body must NOT run while a fill is in progress (AC4)"
        );
    }
}
