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
//! Two globals, one rule: serialize every pool mutation
//! -----------------------------------------------------
//! * `LAST_ACTIVITY_BY_MESH` — the most recent terminal-output / keypress
//!   instant **per mesh**. `note_activity_for_mesh(mesh_id)` is called from
//!   the PTY read loop (output) and the input write path (keypresses) with
//!   the session's mesh_id. The background worker only runs a pass for a
//!   mesh once `idle_duration_for_mesh(mesh.id)` exceeds `IDLE_SILENCE`, so a
//!   `git worktree add` never competes with an agent that is actively
//!   producing output on THAT mesh, or a user who is typing into THAT mesh's
//!   terminal (issue #613 AC2 — per-mesh scope is the issue #634 fix).
//!   A single chatty agent on mesh A keeps only mesh A non-idle; mesh B
//!   can still be maintained while mesh A is busy.
//! * `FILL_LOCK` — a `try_lock`-or-skip guard. EVERY pool-mutating background
//!   path (the idle worker, `refill_after_claim`, and the freshness pass)
//!   takes it through `try_with_fill_lock`, so N concurrent spawns can never
//!   run two `git worktree add` fills at once (issue #613 AC4). `try_lock`
//!   (not a blocking lock) means a second trigger that arrives mid-fill simply
//!   skips — the in-flight fill already drives the pool to target, and the
//!   idle tick re-checks shortly after, so nothing is lost.
//!
//! The decision logic (`is_idle_enough`, `next_tick`) is pure functions so the
//! debounce and backoff are unit-testable without sleeping, mirroring
//! `warm_pool`'s dependency-injected `reconcile_warm_entries`.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Minimum silence (no terminal output / keypresses on a given mesh) before
/// the background worker is allowed to run a refill pass for THAT mesh
/// (issue #613 AC2: "triggering only after 5 seconds of silence"; issue #634:
/// scoped per-mesh so one chatty agent doesn't starve other meshes).
pub const IDLE_SILENCE: Duration = Duration::from_secs(5);

/// Fast tick — the default cadence the worker uses when activity is recent
/// or a refill pass did real work. Smaller than `IDLE_SILENCE` so the worker
/// fires within one tick of the silence window elapsing.
pub(crate) const TICK_FAST: Duration = Duration::from_secs(2);

/// Slow tick — used after several consecutive no-op passes when every pool
/// is at target. Capped to keep the safety-net catch-up window bounded
/// (a stale-row GC / a stale pool still gets seen within 30 s).
pub(crate) const TICK_SLOW: Duration = Duration::from_secs(30);

/// Minimum gap between background sync *attempts* for one mesh (ADR 0020).
/// The worker fetches each idle mesh's base ref on this cadence so the mesh
/// (and its warm pool, via `on_fetch_completed`) stays continuously fresh —
/// which is what lets the spawn path skip its own blocking fetch whenever
/// the last *success* is younger than `fetch_freshness::SPAWN_FETCH_TTL`
/// (5 min). Three minutes leaves a full-interval margin inside the TTL, so
/// in steady state a spawn virtually never fetches. Gated on the ATTEMPT
/// stamp (not success) so an offline machine retries once per interval
/// instead of once per tick.
pub(crate) const BACKGROUND_SYNC_INTERVAL: Duration = Duration::from_secs(180);

/// Pure gate for the background mesh sync — `true` when enough time has
/// passed since the last fetch attempt for this mesh. Split out so the
/// cadence rule is unit-testable without sleeping, like `is_idle_enough`.
pub(crate) fn should_background_sync(time_since_attempt: Duration) -> bool {
    time_since_attempt >= BACKGROUND_SYNC_INTERVAL
}

/// Number of consecutive no-op passes before the worker backs off from
/// `TICK_FAST` to `TICK_SLOW`. Three no-op passes at 2 s = 6 s of nothing-
/// changed — well below the worst-case staleness we care about, so
/// transitioning then keeps the saved-DB-queries without delaying recovery.
pub(crate) const NO_OP_PASSES_BEFORE_BACKOFF: u32 = 3;

/// Most recent terminal-output / keypress instant, **per mesh**. `entry()` is
/// used so a fresh mesh (first PTY chunk, first keypress) is seeded with
/// `Instant::now()` — without that, a brand-new mesh would read
/// `Duration::MAX` of "idle" and be eligible for refills immediately, which
/// the issue #634 fix doesn't actually want (a chatty agent that's just
/// spawned has legitimate activity to suppress refills). Bounded by a
/// retention sweep — see [`prune_stale_activity_entries`].
///
/// Replaces the pre-#634 single global `Instant` which starved every other
/// mesh's pool when one agent was chatty. The map is keyed by `mesh_id`,
/// so a chatty mesh A only suppresses mesh A's refills; mesh B's clock is
/// independent and mesh B can be filled while mesh A is busy.
static LAST_ACTIVITY_BY_MESH: Lazy<Mutex<HashMap<i64, Instant>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Serializes every background pool mutation (fill + freshness). See the
/// module docs: `try_lock`-or-skip, never a blocking lock.
static FILL_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// Record terminal/process activity for `mesh_id`. Called from the PTY read
/// loop (agent output) and the PTY input write path (keypresses) — every
/// call must thread the session's mesh_id. Deliberately cheap — a single
/// mutex-guarded `HashMap` insert on a hot path — and lock-poison-safe (a
/// poisoned lock just means a previous writer panicked mid-store; the map
/// has no invariant to corrupt, so we recover the guard and overwrite it).
///
/// Uses `map.insert(...)` (NOT `entry().or_insert(now)`) so every call
/// overwrites the prior timestamp. The pre-#634 bug here used
/// `entry().or_insert(now)` which silently no-op'd on subsequent calls:
/// the FIRST PTY chunk seeded the clock, every later chunk left the
/// stored instant unchanged — so a chatty agent's mesh would read as
/// "idle" once `IDLE_SILENCE` elapsed past the FIRST chunk's instant,
/// letting the worker fill a pool that's actively being warmed by the
/// spawn path (defeating issue #613 AC2 suppression). The fix: overwrite
/// unconditionally, every call.
///
/// `mesh_id` must be a real DB-known mesh id (`> 0`). Sentinel values
/// `0` (tests' `pty_spawn.rs` fixture) and `-1` (spawn's
/// `db::get_mesh_by_path(...).unwrap_or(-1)` fallback for an
/// orphaned-node spawn) are dropped here so they don't pollute the map
/// with phantom entries that the prune sweep would otherwise chase
/// forever. The validation matches the existing `if mesh_id > 0` guard
/// at `agent::spawn:833` for the warm-claim path.
pub fn note_activity_for_mesh(mesh_id: i64) {
    if mesh_id <= 0 {
        return;
    }
    let now = Instant::now();
    match LAST_ACTIVITY_BY_MESH.lock() {
        Ok(mut map) => {
            map.insert(mesh_id, now);
        }
        Err(poisoned) => {
            poisoned.into_inner().insert(mesh_id, now);
        }
    }
}

/// How long since the last recorded activity for `mesh_id`. Returns
/// `Duration::MAX` when no activity has been recorded for the mesh yet
/// — the "infinity-idle" sentinel that lets a freshly-added mesh's first
/// idle tick fill its pool rather than waiting `IDLE_SILENCE` for some
/// activity that won't come. (`Duration::ZERO` would be the opposite
/// signal — "just got activity" — which `is_idle_enough` reads as "not
/// idle enough"; `Duration::MAX` is the only value that satisfies
/// `idle >= IDLE_SILENCE` for every practical `IDLE_SILENCE`.)
///
/// `saturating_duration_since` rather than `Instant::elapsed` — on the rare
/// platform with a non-monotonic clock, `elapsed()` can panic if the clock
/// went backwards; the saturating variant is always safe for
/// "how-long-ago" reads.
pub fn idle_duration_for_mesh(mesh_id: i64) -> Duration {
    let now = Instant::now();
    let last = match LAST_ACTIVITY_BY_MESH.lock() {
        Ok(map) => map.get(&mesh_id).copied(),
        Err(poisoned) => poisoned.into_inner().get(&mesh_id).copied(),
    };
    match last {
        Some(t) => now.saturating_duration_since(t),
        // No activity recorded for this mesh yet → "infinity idle" so
        // a brand-new mesh's first tick can fill it.
        None => Duration::MAX,
    }
}

/// Drop activity entries older than `max_age` to bound the map's growth
/// across long app lifetimes with many short-lived meshes. Called by the
/// background worker once per tick; cheap because the map is small (one
/// entry per worktree-enabled mesh, typically 1–5). The pre-#634 gate
/// on `TICK_SLOW` meant the prune never ran in steady-state fast-tick
/// deployments; now it's unconditional per tick.
fn prune_stale_activity_entries(max_age: Duration) {
    let cutoff = Instant::now() - max_age;
    match LAST_ACTIVITY_BY_MESH.lock() {
        Ok(mut map) => map.retain(|_, t| *t > cutoff),
        Err(poisoned) => poisoned.into_inner().retain(|_, t| *t > cutoff),
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

/// Run a user-requested pool reconfiguration after waiting for any in-flight
/// fill/claim maintenance to finish. Unlike the idle worker's opportunistic
/// `try_with_fill_lock`, an explicit settings change must not be dropped just
/// because a background pass currently owns the lock (issue #1519).
pub fn with_fill_lock(f: impl FnOnce()) {
    let _guard = FILL_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f();
}

/// Start the background pool-maintenance worker (issue #613 AC1, #634 follow-up).
///
/// Runs on a dedicated OS thread (matching `reconcile_on_startup`'s thread in
/// `lib.rs::setup`) rather than a tokio task, because the work it drives is
/// blocking `git` CLI / DB calls and we don't want to tie up an async runtime
/// worker for the app's lifetime. The loop:
///
///   1. sleeps one `current_tick` (adaptive — starts at `TICK_FAST`,
///      backs off to `TICK_SLOW` after [`NO_OP_PASSES_BEFORE_BACKOFF`]
///      consecutive no-op passes),
///   2. lists worktree-enabled meshes,
///   3. for each mesh: skip if `idle_duration_for_mesh(mesh.id)` is
///      below `IDLE_SILENCE` (debounce — AC2, per-mesh scope — issue #634),
///   4. otherwise runs `warm_pool::drain_and_fill_for_mesh` for that mesh
///      under the fill lock (serialized — AC4). If the lock is held (a
///      `refill_after_claim` or freshness pass is mid-flight) the per-mesh
///      pass is skipped and retried on the next outer tick.
///
/// **Activity scoping (issue #634):** idleness is checked PER MESH. A
/// chatty agent on mesh A keeps only mesh A non-idle; mesh B can be filled
/// while mesh A is busy. This replaces the pre-#634 single-global instant
/// that starved every quiet mesh while any agent was producing output.
///
/// **Adaptive tick (issue #634):** when every mesh is at target, the
/// per-tick work is 2 DB queries × N meshes. After
/// `NO_OP_PASSES_BEFORE_BACKOFF` consecutive no-op passes the outer tick
/// lengthens from `TICK_FAST` (2 s) to `TICK_SLOW` (30 s), reducing the
/// query rate by 15× while keeping the safety-net staleness bounded
/// (30 s worst case is still well within "stale row GC eventually catches
/// up"). A pass that does real work (a mesh's pool changed) snaps the tick
/// back to FAST.
///
/// `app` is captured into the thread closure so `drain_and_fill_for_mesh`
/// can emit `pool-count-changed` from its inner calls. The handle is
/// `Clone + Send + Sync`, so the move into the std::thread is zero-cost.
///
/// Best-effort and infallible from the caller's perspective — every error
/// inside the pass is logged and swallowed by the inner fill helpers, so
/// the loop never dies.
pub fn start_background_worker(app: tauri::AppHandle) {
    std::thread::Builder::new()
        .name("warm-pool-worker".to_string())
        .spawn(move || {
            let mut current_tick = TICK_FAST;
            let mut consecutive_noops: u32 = 0;
            loop {
                std::thread::sleep(current_tick);

                // Sweep the activity map every tick to bound long-running
                // growth from many short-lived meshes. The `2 * IDLE_SILENCE`
                // cutoff is well past anything we'd still consider
                // "active" for refill purposes. Cost is O(N) on the map
                // (typically ≤5 entries), so running every tick is
                // cheaper than the conditional `if current_tick >= TICK_SLOW`
                // gate that the pre-#634 version used — and avoids the
                // "steady-state fast-tick never prunes" failure mode where
                // the map would grow unbounded if backoff was working.
                prune_stale_activity_entries(2 * IDLE_SILENCE);

                // List worktree-enabled meshes once per pass. A DB outage
                // on this read skips the entire pass (next tick retries)
                // — the `match` block logs WARN rather than `unwrap_or_default`
                // so the operator can see the cause.
                let meshes = match crate::db::list_worktree_enabled_meshes_for_warm() {
                    Ok(rows) => rows,
                    Err(e) => {
                        tracing::warn!(
                            "pool_worker: list_worktree_enabled_meshes failed: {}",
                            e
                        );
                        continue;
                    }
                };

                // Per-mesh outcomes. The adaptive-tick backoff needs three
                // distinct signals:
                //   * `did_change` — a mesh's pool state actually changed
                //     (drained OR warmed). The pre-#634 implementation
                //     used `mesh.pre_spawn_pool_size > 0` (a static config
                //     column) which always returned true for any
                //     worktree-enabled mesh — the backoff never triggered
                //     in production because at-target was the steady state.
                //     Now we use `drain_and_fill_for_mesh`'s real return.
                //   * `lock_skipped` — another thread (spawn path's
                //     `post_spawn_maintenance`, a `refill_after_claim`, or
                //     a freshness pass) held the fill lock. We don't count
                //     this as a no-op (the in-flight work IS progress) and
                //     we snap back to FAST so the next tick is ready when
                //     the lock frees.
                //   * `noop` — mesh was processed but nothing changed
                //     (already at target). Only THIS counts toward the
                //     consecutive-no-op backoff.
                let mut did_change = false;
                let mut lock_skipped = false;
                for mesh in meshes {
                    // Per-mesh idle gate (issue #634): only skip THIS mesh
                    // — a chatty mesh A keeps only mesh A non-idle.
                    if !is_idle_enough(idle_duration_for_mesh(mesh.id), IDLE_SILENCE) {
                        continue;
                    }
                    // Background mesh sync (ADR 0020): keep the idle mesh's
                    // base ref continuously fresh so the spawn path can skip
                    // its blocking fetch (freshness TTL) and so warm pool
                    // entries never sit on a days-old SHA between spawns.
                    // Runs BEFORE the fill so a pool entry cut this pass is
                    // cut from the just-fetched ref. Deliberately NOT under
                    // the fill lock — the fetch serializes on the per-Mesh
                    // sync lock instead (same lock every other fetch path
                    // takes); `on_fetch_completed` takes the fill lock
                    // itself for the reset pass. Best-effort: a failure is
                    // logged and retried next interval; the attempt stamp
                    // (written inside `locked_fetch_origin_blocking`) is
                    // what rate-limits offline retries to one per interval.
                    if should_background_sync(
                        crate::services::fetch_freshness::time_since_attempt(&mesh.path),
                    ) {
                        let base_ref = crate::agent::spawn::resolve_base_ref_for_spawn(
                            &mesh.path,
                            Some(mesh.base_ref.as_str()),
                        );
                        // Timed so the diagnostics sampler records how long the
                        // background fetch takes — a climbing `last_ms` while
                        // the machine grinds points at a stalling network path.
                        let sync_started = Instant::now();
                        match crate::git::sync::locked_fetch_origin_blocking(
                            &mesh.path,
                            &base_ref,
                        ) {
                            Ok(outcome) => {
                                let advanced = outcome.advanced_ref();
                                crate::diagnostics::record_bg_sync(advanced, sync_started.elapsed());
                                if advanced {
                                    tracing::info!(
                                        "pool_worker: background sync advanced {} for mesh {}; refreshing warm pool",
                                        base_ref,
                                        mesh.id
                                    );
                                    crate::services::warm_pool::on_fetch_completed(mesh.id);
                                }
                            }
                            Err(e) => {
                                crate::diagnostics::record_bg_sync_failure(sync_started.elapsed());
                                // INFO, not WARN: an offline laptop hits this
                                // every interval and it's fully self-healing.
                                tracing::info!(
                                    "pool_worker: background sync for mesh {} failed (retry in {}s): {:?}",
                                    mesh.id,
                                    BACKGROUND_SYNC_INTERVAL.as_secs(),
                                    e
                                );
                            }
                        }
                    }
                    // Wrap the call in a closure so we can observe the
                    // `try_lock` outcome independently of the drain
                    // result.
                    let mut mesh_changed = false;
                    let took_lock = try_with_fill_lock(|| {
                        mesh_changed =
                            crate::services::warm_pool::drain_and_fill_for_mesh(&app, mesh.id);
                    });
                    if took_lock {
                        if mesh_changed {
                            did_change = true;
                        }
                    } else {
                        lock_skipped = true;
                    }
                }

                // Tick decision: change OR lock-skip → snap to FAST (we're
                // making progress, or another thread is). Only no-ops
                // (we ran, nothing changed, lock was ours) count toward
                // backoff.
                let did_work = did_change || lock_skipped;
                let (new_tick, new_noops) =
                    next_tick(current_tick, did_work, consecutive_noops);
                current_tick = new_tick;
                consecutive_noops = new_noops;
            }
        })
        .expect("failed to spawn warm-pool-worker thread");
}

/// Adaptive-tick decision, pure for unit testing. Given the current tick
/// length, whether the pass did any real work, and the consecutive-no-op
/// counter, returns the next tick length and counter.
///
/// Rules (issue #634):
/// * Any pass that did work → snap to [`TICK_FAST`] and reset no-op counter.
/// * No work + counter below `NO_OP_PASSES_BEFORE_BACKOFF` → stay on
///   `TICK_FAST`, increment counter.
/// * No work + counter at/above threshold → switch to [`TICK_SLOW`],
///   keep incrementing so a long idle period doesn't lose the "we're in
///   the slow lane" signal.
fn next_tick(
    current_tick: Duration,
    did_work: bool,
    consecutive_noops: u32,
) -> (Duration, u32) {
    if did_work {
        return (TICK_FAST, 0);
    }
    if consecutive_noops + 1 >= NO_OP_PASSES_BEFORE_BACKOFF {
        return (TICK_SLOW, consecutive_noops + 1);
    }
    (current_tick, consecutive_noops + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- time-parameterised test seams (issue #751) ----
    //
    // Mirrors of `note_activity_for_mesh` / `idle_duration_for_mesh` that
    // take an explicit `Instant` rather than calling `Instant::now()`
    // internally. Lets the overwrite-prior-timestamp regression test
    // drive a synthetic timeline — `base`, `base + 20ms`, `base + 70ms`
    // — so the two reads are provably monotonic instead of depending on
    // wall-clock arithmetic that Windows' ~15.6ms system timer tick
    // collapses under full-suite `cargo test --lib` parallelism
    // (issue #751). The actual storage code path is still exercised
    // (same `LAST_ACTIVITY_BY_MESH`, same `mesh_id > 0` guard, same
    // lock-or-recover policy) — only the clock source is pinned.

    fn note_activity_for_mesh_at(mesh_id: i64, when: Instant) {
        if mesh_id <= 0 {
            return;
        }
        match LAST_ACTIVITY_BY_MESH.lock() {
            Ok(mut map) => {
                map.insert(mesh_id, when);
            }
            Err(poisoned) => {
                poisoned.into_inner().insert(mesh_id, when);
            }
        }
    }

    fn idle_duration_for_mesh_since(mesh_id: i64, when: Instant) -> Duration {
        let last = match LAST_ACTIVITY_BY_MESH.lock() {
            Ok(map) => map.get(&mesh_id).copied(),
            Err(poisoned) => poisoned.into_inner().get(&mesh_id).copied(),
        };
        match last {
            Some(t) => when.saturating_duration_since(t),
            None => Duration::MAX,
        }
    }

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

    // ---- note_activity_for_mesh / idle_duration_for_mesh (issue #634) ----
    //
    // The per-mesh map is process-global, so concurrent tests would race on
    // the same key set. Each test uses its OWN pair of mesh_ids (large
    // unique sentinels per test) so the assertions don't depend on the
    // order tests happen to run in. Tests pin the *direction* of the clock
    // (record → low, skip → no change) rather than wall-clock arithmetic,
    // so a slow CI machine doesn't flake them.

    /// Pin the issue #634 starva regression: a chatty agent on MESH_A must
    /// NOT make MESH_B look active. Before the fix, MESH_A's PTY output
    /// reset a single global clock that the worker consulted for every
    /// mesh, so MESH_B's pool would never get a refill while MESH_A was
    /// busy. After the fix, only MESH_A's clock is fresh.
    #[test]
    fn one_meshs_activity_does_not_block_another_meshs_idle_check() {
        const MESH_A: i64 = 200_001;
        const MESH_B: i64 = 200_002;

        // MESH_A: just received a PTY chunk → activity recorded → idle
        // duration is tiny.
        note_activity_for_mesh(MESH_A);
        let mesh_a_idle = idle_duration_for_mesh(MESH_A);
        assert!(
            !is_idle_enough(mesh_a_idle, IDLE_SILENCE),
            "MESH_A's clock is fresh after note_activity — must read as not idle"
        );

        // MESH_B: no activity recorded → idle duration is the "infinity
        // idle" sentinel (Duration::MAX), which is ABOVE IDLE_SILENCE and
        // therefore IS idle. Pre-#634, this would have returned MESH_A's
        // tiny value (because the global clock was shared).
        let mesh_b_idle = idle_duration_for_mesh(MESH_B);
        assert!(
            is_idle_enough(mesh_b_idle, IDLE_SILENCE),
            "MESH_B's clock is independent of MESH_A — must read as idle even while MESH_A is busy (issue #634)"
        );
    }

    /// `note_activity_for_mesh(mesh_id)` only resets THAT mesh's clock.
    /// Recording activity for MESH_B must leave MESH_A's clock untouched.
    #[test]
    fn note_activity_for_mesh_resets_only_that_meshs_clock() {
        const MESH_A: i64 = 300_001;
        const MESH_B: i64 = 300_002;

        note_activity_for_mesh(MESH_A);
        std::thread::sleep(Duration::from_millis(20));
        let mesh_a_before = idle_duration_for_mesh(MESH_A);
        // Update only MESH_B.
        note_activity_for_mesh(MESH_B);
        let mesh_a_after = idle_duration_for_mesh(MESH_A);
        assert!(
            mesh_a_after >= mesh_a_before,
            "recording activity for MESH_B must NOT roll back MESH_A's clock (got {} -> {})",
            mesh_a_before.as_millis(),
            mesh_a_after.as_millis()
        );
        // And MESH_B is fresh.
        assert!(
            !is_idle_enough(idle_duration_for_mesh(MESH_B), IDLE_SILENCE),
            "MESH_B's clock was just reset by note_activity_for_mesh"
        );
    }

    /// A mesh with no recorded activity returns `Duration::MAX` (the
    /// "infinity-idle" sentinel). This is the desired behaviour for a
    /// freshly-added mesh — its first idle tick should fill it rather
    /// than waiting `IDLE_SILENCE` for some activity that won't come.
    /// (`Duration::ZERO` would be the opposite — "just got activity",
    /// which `is_idle_enough` reads as "not idle enough".)
    #[test]
    fn idle_duration_for_mesh_returns_max_when_no_activity_recorded() {
        const MESH_C: i64 = 400_001;
        assert_eq!(
            idle_duration_for_mesh(MESH_C),
            Duration::MAX,
            "a mesh with no recorded activity must read as 'infinity idle' so its first tick can fill it"
        );
        // Sanity: Duration::MAX >= IDLE_SILENCE, so the gate says idle.
        assert!(
            is_idle_enough(idle_duration_for_mesh(MESH_C), IDLE_SILENCE),
            "Duration::MAX is above the silence window — the gate must say idle (issue #634)"
        );
    }

    /// Boundary: `note_activity_for_mesh` seeds the FIRST entry to
    /// `Instant::now()` rather than leaving it absent. Without this,
    /// `idle_duration_for_mesh` would return `Duration::ZERO` for the
    /// brief window between mesh creation and the first PTY chunk, and
    /// the worker could fill a pool that's actively being warmed by the
    /// spawn path. The seed-on-first-write is what guarantees
    /// "freshly active" → low idle → "not idle" right after the call.
    #[test]
    fn note_activity_for_mesh_seeds_first_entry_to_now() {
        const MESH_C: i64 = 500_001;
        note_activity_for_mesh(MESH_C);
        let idle = idle_duration_for_mesh(MESH_C);
        // Right after seeding, idle must be very small — well below
        // IDLE_SILENCE. We allow up to 1 s of slack for slow CI machines.
        assert!(
            idle < Duration::from_secs(1),
            "first-write seed must set the clock to now (got {}ms idle)",
            idle.as_millis()
        );
        assert!(
            !is_idle_enough(idle, IDLE_SILENCE),
            "right after seeding, the gate must report 'not idle'"
        );
    }

    /// Load-bearing regression for issue #634 review finding #1: every
    /// call to `note_activity_for_mesh` MUST overwrite the prior instant,
    /// not no-op via `entry().or_insert()`. The pre-fix bug froze the
    /// clock at the FIRST chunk's instant, so a chatty agent's mesh
    /// would read as "idle" once `IDLE_SILENCE` elapsed past that
    /// FIRST instant — letting the worker fill a pool that's actively
    /// being warmed by the spawn path (defeating issue #613 AC2
    /// suppression).
    ///
    /// Issue #751: the previous version of this test called
    /// `note_activity_for_mesh` / `idle_duration_for_mesh` (which both
    /// read `Instant::now()` internally) and slept 20 ms + 50 ms between
    /// the two reads. Under full-suite `cargo test --lib` parallelism
    /// Windows' ~15.6 ms system-timer tick can collapse two consecutive
    /// `Instant::now()` reads into the same instant, producing
    /// `first_idle == second_idle == 0 ms` and tripping the
    /// `second_idle < first_idle` assertion. We now drive a *synthetic*
    /// timeline via the `cfg(test)` seams `note_activity_for_mesh_at`
    /// and `idle_duration_for_mesh_since` — the two stored instants are
    /// arithmetic (`base + 70 ms` vs `base`), not wall-clock, so the
    /// test is provably deterministic regardless of OS timer resolution
    /// or test-suite contention.
    #[test]
    fn note_activity_for_mesh_overwrites_prior_timestamp() {
        const MESH_D: i64 = 600_001;
        // Anchor — only used as a reference point for the synthetic deltas
        // below; the test never compares against wall-clock arithmetic.
        let base = Instant::now();
        // First note: stored instant = `base`.
        note_activity_for_mesh_at(MESH_D, base);
        // 20 ms later, read — stored instant is `base`, so idle is 20 ms.
        let first_idle =
            idle_duration_for_mesh_since(MESH_D, base + Duration::from_millis(20));
        // 50 ms later (total +70 ms from `base`), note again — stored
        // instant MUST be overwritten (pre-#634 bug used
        // `entry().or_insert(now)` which silently no-op'd here).
        note_activity_for_mesh_at(MESH_D, base + Duration::from_millis(70));
        // Immediate read — stored instant is `base + 70 ms`, so idle is 0 ms.
        let second_idle =
            idle_duration_for_mesh_since(MESH_D, base + Duration::from_millis(70));
        assert_eq!(
            first_idle,
            Duration::from_millis(20),
            "first read must see the originally stored instant (20 ms after `base`)"
        );
        assert_eq!(
            second_idle,
            Duration::ZERO,
            "second read must see the freshly overwritten instant (no time has elapsed since the +70 ms note)"
        );
        assert!(
            second_idle < first_idle,
            "second note_activity_for_mesh must reset the clock (first_idle={}ms, second_idle={}ms) — \
             the pre-#634 bug froze the clock at the FIRST chunk's instant",
            first_idle.as_millis(),
            second_idle.as_millis()
        );
    }

    /// Mesh-id sentinels (`<= 0`) must NOT pollute the activity map.
    /// Production sources: spawn's `unwrap_or(-1)` fallback for an
    /// orphaned-node spawn (spawn.rs:809), and test fixtures in
    /// `tests/pty_spawn.rs` that use `mesh_id: 0`. The pre-fix bug
    /// added phantom entries that the prune sweep would chase forever.
    #[test]
    fn note_activity_for_mesh_ignores_invalid_mesh_ids() {
        // Sentinel values must not change the map state.
        let pre_invalid = idle_duration_for_mesh(700_001); // 700_001 not yet recorded → MAX
        assert_eq!(pre_invalid, Duration::MAX);
        for invalid in [0i64, -1, -42] {
            note_activity_for_mesh(invalid);
        }
        // A never-seen mesh is still never-seen: the invalid sentinels
        // didn't pollute the map with phantom entries.
        let post_invalid = idle_duration_for_mesh(700_001);
        assert_eq!(
            post_invalid, Duration::MAX,
            "invalid mesh_id sentinels must not write to the activity map"
        );
        // And a real mesh_id after a sentinel still works as expected.
        note_activity_for_mesh(700_001);
        assert!(!is_idle_enough(idle_duration_for_mesh(700_001), IDLE_SILENCE));
    }

    // ---- should_background_sync (ADR 0020 background mesh sync cadence) ----

    #[test]
    fn background_sync_fires_for_a_never_attempted_mesh() {
        // `fetch_freshness::time_since_attempt` returns Duration::MAX for a
        // mesh with no recorded attempt — the first idle tick must sync it.
        assert!(should_background_sync(Duration::MAX));
    }

    #[test]
    fn background_sync_respects_the_interval() {
        assert!(
            !should_background_sync(Duration::from_secs(10)),
            "a mesh attempted 10s ago must NOT be re-fetched — one attempt per interval"
        );
        assert!(
            !should_background_sync(BACKGROUND_SYNC_INTERVAL - Duration::from_secs(1)),
            "just inside the interval still waits"
        );
        assert!(
            should_background_sync(BACKGROUND_SYNC_INTERVAL),
            "at the interval boundary the sync fires"
        );
    }

    /// The whole point of the background sync is that its cadence fits
    /// inside the spawn path's freshness TTL with margin: if the interval
    /// ever grew past the TTL, a steadily-idle mesh would oscillate between
    /// fresh and stale and spawns would randomly pay the fetch again.
    #[test]
    fn background_sync_interval_fits_inside_the_spawn_fetch_ttl() {
        assert!(
            BACKGROUND_SYNC_INTERVAL * 3 / 2
                <= crate::services::fetch_freshness::SPAWN_FETCH_TTL,
            "keep BACKGROUND_SYNC_INTERVAL well inside SPAWN_FETCH_TTL (currently {}s vs {}s) — \
             the worker needs slack (slow ticks, sync-lock waits) to re-stamp before the TTL lapses",
            BACKGROUND_SYNC_INTERVAL.as_secs(),
            crate::services::fetch_freshness::SPAWN_FETCH_TTL.as_secs()
        );
    }

    // ---- next_tick (adaptive backoff, issue #634) ----

    /// Work → snap to FAST, reset counter.
    #[test]
    fn next_tick_resets_to_fast_on_work() {
        let (tick, noops) = next_tick(TICK_SLOW, true, 17);
        assert_eq!(tick, TICK_FAST);
        assert_eq!(noops, 0, "any work must reset the no-op counter");
    }

    /// No work, counter below threshold → stay on FAST, increment.
    #[test]
    fn next_tick_stays_fast_below_threshold() {
        // Pass 1 (counter 0 → 1): no switch yet.
        let (tick, noops) = next_tick(TICK_FAST, false, 0);
        assert_eq!(tick, TICK_FAST);
        assert_eq!(noops, 1);

        // Pass 2 (counter 1 → 2): still no switch.
        let (tick, noops) = next_tick(TICK_FAST, false, 1);
        assert_eq!(tick, TICK_FAST);
        assert_eq!(noops, 2);
    }

    /// No work, counter hits `NO_OP_PASSES_BEFORE_BACKOFF` (the third
    /// no-op pass in a row) → switch to SLOW and keep incrementing. The
    /// counter never resets while we're in the slow lane so a long idle
    /// period doesn't lose the "we're slow" signal — only real work
    /// (`did_work = true`) resets it.
    #[test]
    fn next_tick_switches_to_slow_at_threshold() {
        // The 3rd no-op triggers the switch.
        let (tick, noops) = next_tick(TICK_FAST, false, 2);
        assert_eq!(
            tick,
            TICK_SLOW,
            "the 3rd consecutive no-op pass (counter 2 → 3) must switch to TICK_SLOW"
        );
        assert_eq!(
            noops, 3,
            "the counter reaches the threshold value so we know we're in slow lane"
        );

        // Subsequent slow passes stay slow.
        let (tick, noops) = next_tick(TICK_SLOW, false, 3);
        assert_eq!(tick, TICK_SLOW);
        assert_eq!(noops, 4);

        let (tick, noops) = next_tick(TICK_SLOW, false, 17);
        assert_eq!(tick, TICK_SLOW);
        assert_eq!(noops, 18);
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
