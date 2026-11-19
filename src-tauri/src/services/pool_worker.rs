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
use tauri::Emitter;

use crate::git::fetch_failure::FetchFailureClass;

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

/// Issue #2832: record a failed background sync in the counter that matches
/// its failure class.
///
/// The DIAG line reports the two kinds separately (`fail_perm` and
/// `fail_trans`), so WHICH counter a failure lands in is part of the
/// contract rather than a detail of the log line. Routing the choice
/// through one function keeps it in one place and lets the sibling test
/// suite assert it without running the worker.
pub(crate) fn record_bg_sync_failure(class: FetchFailureClass, elapsed: Duration) {
    match class {
        FetchFailureClass::Permanent => {
            crate::diagnostics::record_bg_sync_permanent_failure(elapsed);
        }
        FetchFailureClass::Transient => {
            crate::diagnostics::record_bg_sync_transient_failure(elapsed);
        }
    }
}

/// Issue #2348: decide whether this tick runs the background sync for
/// `mesh_path` — and keep the attempt clock honest when it does not.
///
/// Four outcomes, and the last two are the ones that are easy to get wrong:
///  1. the Mesh is **paused** by a permanent failure (issue #2832) → skip,
///     before the attempt clock is read, before the repository probe and
///     before the fetch;
///  2. the attempt clock has not reached [`BACKGROUND_SYNC_INTERVAL`] → skip,
///     touching nothing (including no repository probe);
///  3. due and the folder can be fetched → run the fetch, which stamps the
///     attempt itself inside `locked_fetch_origin_blocking`;
///  4. due but the folder does not open as a git working repository
///     (`warm_pool::mesh_repo_unusable`) → skip the fetch **and stamp the
///     attempt**.
///
/// That stamp in case 4 is load-bearing. `time_since_attempt` reads
/// `Duration::MAX` for a mesh with no recorded attempt, and the only thing that
/// ever wrote one was the fetch we just skipped — so without the stamp a mesh
/// that can never be fetched re-probes the filesystem on *every* tick (2 s
/// while any mesh is active, 30 s once the worker backs off) instead of once
/// per interval, which is precisely the retry-forever behaviour #2348 removed
/// from the fill path, just moved. `note_attempt` never writes `last_success`,
/// so the spawn path's freshness gate is untouched: a mesh that cannot be
/// fetched keeps fetching on spawn.
///
/// The pause in case 1 comes FIRST because it is the cheapest check and the
/// most valuable one: a Mesh whose `base_ref` names a branch the remote does
/// not have would otherwise run a failing `git fetch` on the three-minute
/// sync cadence for the rest of the app's life. While it is paused the
/// worker does not probe the repository, and the pause lifts as soon as the
/// Mesh's configuration changes.
///
/// `paused`, `attempt_is_due`, `repo_usable` and `stamp` are injected so all
/// four outcomes are unit-testable without sleeping, a `tauri::AppHandle` or a
/// real repository — the sequencing above is the whole behaviour.
fn background_sync_allowed(
    mesh_path: &str,
    paused: impl FnOnce(&str) -> bool,
    attempt_is_due: impl FnOnce(&str) -> bool,
    repo_usable: impl FnOnce() -> bool,
    stamp: impl FnOnce(&str),
) -> bool {
    if paused(mesh_path) {
        return false;
    }
    if !attempt_is_due(mesh_path) {
        return false;
    }
    if !repo_usable() {
        stamp(mesh_path);
        return false;
    }
    true
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

/// Run `f` under the global fill lock, BLOCKING until it is free (issue
/// #1519 — directory-change rebuilds). Unlike [`try_with_fill_lock`] (which
/// skips on contention so a burst of high-frequency triggers collapses to
/// one fill), a rebuild is a rare explicit user action that must converge
/// promptly: skipping it would leave stale-location inventory until the
/// next idle tick. Call ONLY from a dedicated background thread — never the
/// IPC worker or any hot path. Poison-recovers like its sibling: the
/// guarded unit carries no state to corrupt.
pub fn with_fill_lock_blocking(f: impl FnOnce()) {
    let _guard = FILL_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f();
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
                    //
                    // Issue #2348: a Mesh whose folder cannot be opened as a
                    // git working repository can never be fetched either, so
                    // it skips this block too. Without the gate it
                    // re-attempted (and re-logged `fetch_origin: path ... is
                    // not a usable git repo`) every interval, which is the same
                    // log flood the fill gate exists to stop.
                    // `background_sync_allowed` owns the sequencing: the probe
                    // runs only once the attempt clock is due, and a suppressed
                    // mesh is stamped so the probe stays at one per interval
                    // rather than one per tick. The stamp matters on an
                    // unreachable drive: `time_since_attempt` already resolves
                    // the mesh's coordination key — a repository discovery — on
                    // every tick for every mesh (pre-existing), and without the
                    // stamp an unpoolable Mesh pays the probe on top of that,
                    // every tick, on the single worker thread.
                    //
                    // Issue #2832: a fetch that only a configuration change
                    // can fix must not be retried on the fixed three-minute
                    // schedule. The gate's first check is the
                    // permanent-failure registry: it is a
                    // cheap map lookup, and while the Mesh is paused the
                    // worker must not even probe the repository. The
                    // fingerprint is the Mesh's identity and its *configured*
                    // base ref, so changing that setting resumes the sync at
                    // once — the recorded failure then describes a situation
                    // that may have changed.
                    if background_sync_allowed(
                        &mesh.path,
                        |path| {
                            !crate::git::fetch_failure::allows_attempt(
                                path,
                                &permanent_failure_fingerprint(mesh.id, &mesh.base_ref),
                            )
                        },
                        |path| {
                            should_background_sync(
                                crate::git::fetch_freshness::time_since_attempt(path),
                            )
                        },
                        || !crate::services::warm_pool::mesh_repo_unusable(&mesh),
                        crate::git::fetch_freshness::note_attempt,
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
                                let elapsed = sync_started.elapsed();
                                // Issue #2832: not all failures deserve the
                                // fixed schedule. A failure that only a
                                // configuration change can fix (the remote has
                                // no such branch, the remote is gone, the
                                // credentials are refused) pauses this Mesh's
                                // sync; a network failure keeps the existing
                                // retry.
                                let class = crate::git::fetch_failure::classify_fetch_error(&e);
                                // The DIAG line counts the two kinds
                                // separately, so the counter is chosen by
                                // the class and nowhere else.
                                record_bg_sync_failure(class, elapsed);
                                match class {
                                    crate::git::fetch_failure::FetchFailureClass::Permanent => {
                                        let reason =
                                            crate::git::fetch_failure::fetch_failure_reason(&e);
                                        let report =
                                            crate::git::fetch_failure::record_permanent_failure(
                                                &mesh.path,
                                                &permanent_failure_fingerprint(
                                                    mesh.id,
                                                    &mesh.base_ref,
                                                ),
                                                Instant::now(),
                                            );
                                        if report.first {
                                            tracing::warn!(
                                                "pool_worker: background sync for mesh {} failed permanently: \
                                                 {}; retrying in {}s",
                                                mesh.id,
                                                reason,
                                                report.interval.as_secs()
                                            );
                                            // Surface it ONCE on the mesh: the
                                            // first permanent failure is the
                                            // one the user has not been told
                                            // about. The later ones are the
                                            // same message on a longer timer.
                                            surface_permanent_sync_failure(
                                                &app,
                                                &mesh.path,
                                                &base_ref,
                                                &reason,
                                            );
                                        } else if report.interval_changed {
                                            // The retry interval grew: report
                                            // the escalation once and then go
                                            // quiet, so a Mesh that stays broken
                                            // produces a handful of lines rather
                                            // than one per attempt.
                                            tracing::info!(
                                                "pool_worker: background sync for mesh {} still failing \
                                                 (attempt {}): {}; retrying in {}s",
                                                mesh.id,
                                                report.failures,
                                                reason,
                                                report.interval.as_secs()
                                            );
                                        }
                                    }
                                    crate::git::fetch_failure::FetchFailureClass::Transient => {
                                        // INFO, not WARN: an offline laptop hits
                                        // this on each interval and recovers by
                                        // itself.
                                        tracing::info!(
                                            "pool_worker: background sync for mesh {} failed (retry in {}s): {:?}",
                                            mesh.id,
                                            BACKGROUND_SYNC_INTERVAL.as_secs(),
                                            e
                                        );
                                    }
                                }
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

/// Surface a permanent background-sync failure on the Mesh it is about
/// (issue #2832).
///
/// The `mesh-sync-warning` event is the app's existing "this Mesh's sync is
/// broken" channel: `App.tsx` raises the toast and the sidebar's mesh row
/// lights its failure icon for the Mesh whose path the warning carries. The
/// backend does not rewrite the Mesh's `base_ref` — the message states what
/// is wrong and offers the branch the remote actually has, and the user
/// decides (issue #2832's "out of scope" list).
///
/// The remote's default branch is read from the local symbolic ref first and
/// from `git ls-remote` only when that is missing, so the common case costs
/// no network round-trip.
fn surface_permanent_sync_failure(
    app: &tauri::AppHandle,
    mesh_path: &str,
    base_ref: &str,
    reason: &str,
) {
    use crate::agent::spawn::{MeshSyncOutcome, MeshSyncWarningPayload};

    let remote = crate::git::sync::parse_remote_for_base_ref(base_ref);
    // Only the failure's OWN message may say which branch is missing. Falling
    // back to the base ref's branch would tell the user "origin has no branch
    // 'main'" for an authentication failure — a claim the failure did not
    // make.
    let missing_branch = crate::git::fetch_failure::missing_remote_branch(reason);
    let default_branch = remote.as_deref().and_then(|remote_name| {
        crate::git::fetch_failure::detect_remote_default_branch(
            &crate::env::to_host_path(mesh_path),
            remote_name,
        )
    });
    let message = crate::git::fetch_failure::describe_permanent_failure(
        mesh_path,
        remote.as_deref(),
        missing_branch.as_deref(),
        default_branch.as_deref(),
        reason,
    );
    tracing::warn!("pool_worker: {}", message);
    let _ = app.emit(
        "mesh-sync-warning",
        MeshSyncWarningPayload {
            // The warning is about a Mesh, not about a Node — no Node
            // caused it. `0` is the "no node" sentinel; the frontend matches
            // the Mesh by `mesh_path`.
            node_id: 0,
            mesh_path: mesh_path.to_string(),
            outcome: MeshSyncOutcome::PermanentFetchFailure,
            new_commits: None,
            pr_number: None,
            head_ref: None,
            expected_sha: None,
            actual_sha: None,
            fallback_base_ref: None,
            head_repo_owner: None,
            head_repo_clone_url: None,
            message,
        },
    );
}

/// The fingerprint the permanent-failure record is keyed on: the Mesh's
/// identity and its configured `base_ref`.
///
/// The `base_ref` is the setting the user changes to fix a permanent
/// failure, so a change to it must resume the sync at once. The id is in
/// the fingerprint too: a Mesh deleted and re-created at the same path is a
/// different Mesh, and without the id the new row would inherit the old
/// pause — and, because a pause suppresses the "first failure" report, the
/// user would not be told why its sync is stopped.
fn permanent_failure_fingerprint(mesh_id: i64, base_ref: &str) -> String {
    format!("{mesh_id}|{base_ref}")
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
fn next_tick(current_tick: Duration, did_work: bool, consecutive_noops: u32) -> (Duration, u32) {
    if did_work {
        return (TICK_FAST, 0);
    }
    if consecutive_noops + 1 >= NO_OP_PASSES_BEFORE_BACKOFF {
        return (TICK_SLOW, consecutive_noops + 1);
    }
    (current_tick, consecutive_noops + 1)
}

#[cfg(test)]
#[path = "pool_worker_tests.rs"]
mod tests;
