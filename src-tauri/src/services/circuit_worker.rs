//! The Autopilot Circuits worker (spec #1205 / walking skeleton #1206):
//! the impure seam around the pure stepper
//! (`autopilot::circuit::stepper`).
//!
//! ## Shape: dedicated OS thread + hybrid wakeups
//!
//! A dedicated `std::thread` (not a tokio task) owns the pass loop, per
//! the spec's runtime decision — it keeps blocking SQLite/git work off
//! the async runtime. Wakeups are hybrid:
//! - **Fast tick** every 2s: interval pacing, capacity unblocking, and
//!   piloted-node observation.
//! - **Condition-variable wake**: direct IPC dispatch — Trigger Now
//!   bumps the wake counter so a manual run starts within milliseconds.
//!   (GitHub poll passes and attention-webhook wakes arrive in later
//!   milestones; they plug into the same condvar.)
//!
//! ## One pass = observe → step → commit → execute
//!
//! For each active run the pass:
//! 1. **Observes** live state (agent-node status, process liveness,
//!    capacity counters) and turns it into pure [`CircuitEvent`]s;
//! 2. **Steps** via [`advance`](autopilot::circuit::stepper::advance) —
//!    no DB, no I/O;
//! 3. **Commits** the decided writes atomically through
//!    [`db::commit_circuit_advance`] (one transaction);
//! 4. **Executes** effects against the real world (spawn agent node,
//!    inject PTY prompt, set node status, notify UI).
//!
//! A crash between commit and effect execution is recovered by
//! observation on the next pass: a spawn step whose agent node has
//! since vanished maps to `AgentLost`, and an effect that fails
//! synchronously fails its step immediately (a Running step with no
//! attached agent would otherwise wedge the run — nothing observes it).
//! The one remaining gap — a process crash inside the milliseconds
//! between commit and stage-1 attach, invisible to observation — is
//! closed by [`startup_reconcile_pass`], which runs once per app launch
//! (milestone 3, issue #1208) and evaluates `running` runs against live
//! process and git state (resume / fail). The [`lost_turn_watchdog_pass`]
//! recovers quiet piloted nodes whose turn webhook was missed so
//! multi-hour runs self-heal.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use tauri::{AppHandle, Emitter};

use crate::autopilot::circuit::context::CircuitContext;
use crate::autopilot::circuit::model::{
    CircuitGraph, CircuitNodeKind, StepOutcome as GraphStepOutcome,
};
use crate::autopilot::circuit::stepper::{
    advance, Capacity, CircuitEvent, RunState, RunView, StepStatus, StepView,
};
use crate::db;
use crate::models::SessionStatus;
use crate::process_util::run_worker_pass;
use crate::agent::spawn::ExplicitSpawnOverrides;

/// Fast tick — covers interval pacing headroom, slot unblocking latency,
/// and piloted-agent observation lag.
const TICK_INTERVAL: Duration = Duration::from_secs(2);

/// Startup delay so boot-time DB migration finishes before the first
/// pass (mirrors the legacy autopilot worker).
const STARTUP_DELAY: Duration = Duration::from_secs(5);

/// Wake condvar. Trigger Now notifies; the worker otherwise wakes on
/// its fast tick. Milestone 2 (#1207): PTY yields also notify (reactive
/// gate evaluation), as do collaborator approvals.
static WAKE: Lazy<(Mutex<()>, Condvar)> = Lazy::new(|| (Mutex::new(()), Condvar::new()));

/// Stage-2 circuit spawns are asynchronous, while cancellation and circuit
/// deletion are synchronous commands. This barrier closes the small window in
/// which a command could snapshot the run's attached agents, delete the
/// ledger, and then observe a process created by a spawn that was already in
/// flight. The permit is held from stage-1 row creation through stage-2
/// teardown; commands take the barrier before terminalising/deleting runs.
static CIRCUIT_SPAWNS: Lazy<(Mutex<HashMap<i64, usize>>, Condvar)> =
    Lazy::new(|| (Mutex::new(HashMap::new()), Condvar::new()));
const CIRCUIT_SPAWN_QUIESCE_TIMEOUT: Duration = Duration::from_secs(30);

/// Cancellation invalidation for an in-flight effect batch. The worker takes
/// one durable run-state snapshot per transition (avoiding an N+1 query),
/// while command-side cancellation flips this token before waiting for any
/// spawn teardown. Every effect checks the token immediately before it runs,
/// so a cancellation that arrives between two slow external effects still
/// stops the remainder of the batch.
struct CircuitEffectCancellation {
    cancelled: Arc<AtomicBool>,
    finished: bool,
    in_flight: usize,
}

static CIRCUIT_EFFECT_CANCELLATIONS: Lazy<Mutex<HashMap<i64, CircuitEffectCancellation>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub struct CircuitEffectBatchPermit {
    run_id: i64,
    cancelled: Arc<AtomicBool>,
}

impl CircuitEffectBatchPermit {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl Drop for CircuitEffectBatchPermit {
    fn drop(&mut self) {
        let mut active = CIRCUIT_EFFECT_CANCELLATIONS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(entry) = active.get_mut(&self.run_id) else {
            return;
        };
        entry.in_flight = entry.in_flight.saturating_sub(1);
        if entry.in_flight == 0
            && (!entry.cancelled.load(Ordering::Acquire) || entry.finished)
        {
            // A cancellation command that has already acknowledged the
            // durable terminal state can leave the marker behind only until
            // the final in-flight batch drops.
            active.remove(&self.run_id);
        }
    }
}

pub fn begin_circuit_effect_batch(run_id: i64) -> CircuitEffectBatchPermit {
    let mut active = CIRCUIT_EFFECT_CANCELLATIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = active.entry(run_id).or_insert_with(|| CircuitEffectCancellation {
        cancelled: Arc::new(AtomicBool::new(false)),
        finished: false,
        in_flight: 0,
    });
    entry.in_flight += 1;
    CircuitEffectBatchPermit {
        run_id,
        cancelled: Arc::clone(&entry.cancelled),
    }
}

/// Invalidate effects before a cancellation command waits on external
/// cleanup. The marker remains until the command acknowledges the durable
/// terminal state, closing the race where a new batch starts during that wait.
pub fn mark_circuit_run_cancelled(run_id: i64) {
    let mut active = CIRCUIT_EFFECT_CANCELLATIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = active.entry(run_id).or_insert_with(|| CircuitEffectCancellation {
        cancelled: Arc::new(AtomicBool::new(false)),
        finished: false,
        in_flight: 0,
    });
    entry.cancelled.store(true, Ordering::Release);
    entry.finished = false;
}

/// Release a cancellation marker once the run's durable state is terminal.
/// An in-flight batch removes itself when it observes the marker and drops.
pub fn finish_circuit_run_cancellation(run_id: i64) {
    let mut active = CIRCUIT_EFFECT_CANCELLATIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(entry) = active.get_mut(&run_id) {
        entry.finished = true;
        if entry.in_flight == 0 {
            active.remove(&run_id);
        }
    }
}

struct CircuitSpawnPermit {
    run_id: i64,
}

impl Drop for CircuitSpawnPermit {
    fn drop(&mut self) {
        let (lock, wake) = &*CIRCUIT_SPAWNS;
        let mut active = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(count) = active.get_mut(&self.run_id) {
            if *count <= 1 {
                active.remove(&self.run_id);
            } else {
                *count -= 1;
            }
        }
        wake.notify_all();
    }
}

/// Reserve the spawn barrier while checking the durable run state. The check
/// and insertion share the mutex with command-side quiescence, so a delete
/// that has acquired the barrier cannot be followed by a late stage-1 spawn.
fn begin_circuit_spawn(run_id: i64) -> Result<Option<CircuitSpawnPermit>, String> {
    let (lock, _) = &*CIRCUIT_SPAWNS;
    let mut active = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if !run_accepts_effects(run_id)? {
        return Ok(None);
    }
    *active.entry(run_id).or_insert(0) += 1;
    Ok(Some(CircuitSpawnPermit { run_id }))
}

fn wait_for_spawn_set_to_empty<'a>(
    mut active: std::sync::MutexGuard<'a, HashMap<i64, usize>>,
    run_ids: &[i64],
) -> (std::sync::MutexGuard<'a, HashMap<i64, usize>>, bool) {
    let (_, wake) = &*CIRCUIT_SPAWNS;
    let deadline = Instant::now() + CIRCUIT_SPAWN_QUIESCE_TIMEOUT;
    while run_ids.iter().any(|run_id| active.contains_key(run_id)) {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return (active, false);
        };
        let (next, result) = wake
            .wait_timeout(active, remaining)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active = next;
        if result.timed_out() {
            return (active, false);
        }
    }
    (active, true)
}

/// Run a synchronous command while no spawn for this run is in flight. The
/// closure executes while the barrier is held, preventing a worker pass that
/// already loaded the run from starting a new stage-1 spawn after the ledger
/// has been terminalised.
pub fn with_circuit_run_spawns_quiesced<T>(run_id: i64, f: impl FnOnce() -> T) -> Result<T, String> {
    let (lock, _) = &*CIRCUIT_SPAWNS;
    let active = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_active, quiesced) = wait_for_spawn_set_to_empty(active, &[run_id]);
    if !quiesced {
        return Err(format!("circuit run {} still has a spawn in flight", run_id));
    }
    Ok(f())
}

/// Same barrier for deleting a whole circuit. The caller must disable new
/// trigger ingestion before entering this function; existing worker passes
/// are covered by the mutex and late spawns fail their durable-state check.
pub fn with_circuit_spawns_quiesced<T>(circuit_id: i64, f: impl FnOnce() -> T) -> Result<T, String> {
    let (lock, _) = &*CIRCUIT_SPAWNS;
    let active = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Snapshot run ids only after taking the same mutex used by stage-1
    // spawn admission. A trigger that raced the caller's disable write cannot
    // slip into the protected deletion window between a query and lock
    // acquisition.
    let run_ids = db::list_circuit_run_ids_for_cleanup(circuit_id)
        .map_err(|error| error.to_string())?;
    let (_active, quiesced) = wait_for_spawn_set_to_empty(active, &run_ids);
    if !quiesced {
        return Err(format!("circuit {} still has a spawn in flight", circuit_id));
    }
    Ok(f())
}

/// Pending collaborator approvals (#1207): `(run_id, node_id)` pairs the
/// user approved via IPC while the gate step parks in `blocked`. Drained
/// by the owning run's next pass into pure `CollaboratorApproved` events.
/// Deliberately in-memory: approvals are click-moments, not durable
/// state — after an app restart the user simply approves again.
static APPROVALS: Lazy<Mutex<Vec<(i64, String)>>> = Lazy::new(|| Mutex::new(Vec::new()));

/// Lock one of the circuit worker's statics, recovering from a poisoned
/// mutex instead of panicking (issue #1224).
///
/// The worker holds two small state mutexes — `WAKE` (a `Mutex<()>` used
/// only to enter `Condvar::wait_timeout`) and `APPROVALS` (a `Vec<(i64,
/// String)>`). Both are guarded by app-lifetime invariants that a panic
/// mid-write does not corrupt (the guard is dropped on unwind, leaving
/// the inner value in a consistent empty-or-fully-formed state). `.unwrap()`
/// on `PoisonError` permanently bricks the worker: the next pass would
/// panic the spawned thread, `wake_circuit_worker()` would silently fail
/// to wake anyone, and the entire circuit poller would stall. The recover
/// shape matches `db::write_conn()` and `services::autopilot::lock_planner_set`.
fn lock_circuit_worker_static<T>(mutex: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(
                "circuit worker mutex was poisoned by a prior panic — recovering (issue #1224)"
            );
            poisoned.into_inner()
        }
    }
}

/// Queue a collaborator approval and wake the worker immediately so the
/// parked run advances within milliseconds.
pub fn request_circuit_approval(run_id: i64, node_id: String) {
    lock_circuit_worker_static(&APPROVALS).push((run_id, node_id));
    wake_circuit_worker();
}

/// Take this run's queued approvals (leaving other runs' entries alone).
/// Uses `Vec::extract_if` (Rust 1.87+) to partition in-place — no
/// allocation for the rest-list, no re-assignment. The returned `Vec`
/// holds only the taken entries' `node_id`s.
fn drain_approvals_for(run_id: i64) -> Vec<String> {
    let mut queue = lock_circuit_worker_static(&APPROVALS);
    queue
        .extract_if(.., |(r, _)| *r == run_id)
        .map(|(_, node_id)| node_id)
        .collect()
}

/// Sweep the approvals queue against the current active-run set
/// (issue #1263). A user can click "Approve" for a run that completes,
/// fails, or gets deleted before the next pass — without this sweep
/// its queued approval would sit forever for a vanished run. Click-bounded
/// volume (a few approvals per app lifetime at most), so the cost must
/// stay at zero allocations on the hot 2-second tick: early-return on
/// empty queue, and a linear scan over the active-runs slice for tiny
/// lists (avoids building a HashSet just to test membership).
fn sweep_stale_approvals(active_runs: &[db::ActiveCircuitRun]) {
    let mut queue = lock_circuit_worker_static(&APPROVALS);
    if queue.is_empty() {
        return;
    }
    let before = queue.len();
    queue.retain(|(run_id, _)| active_runs.iter().any(|r| r.run.id == *run_id));
    let dropped = before - queue.len();
    if dropped > 0 {
        tracing::debug!(
            "circuits: dropped {} stale approval(s) for vanished runs",
            dropped
        );
    }
}

/// Wake the circuit worker immediately (manual trigger dispatch).
pub fn wake_circuit_worker() {
    let (_lock, cvar) = &*WAKE;
    cvar.notify_all();
}

/// Start the dedicated circuits worker thread. Called once from Tauri
/// `setup`, alongside the legacy autopilot worker. Startup order:
/// reconcile → loop (interval pass, GitHub poll pass, drive pass,
/// lost-turn watchdog).
///
/// Issue #1235: every per-pass body runs inside
/// [`crate::process_util::run_worker_pass`] so a single panic deep in
/// `run_pass` / `lost_turn_watchdog_pass` (e.g. an out-of-range serde
/// tag, a DB invariant violation surfaced as a panic, a panic while
/// holding `APPROVALS`) unwinds the pass — not the thread. Without
/// this, the worker dies silently for the rest of the session and
/// circuits stop advancing while the UI badge sits at idle with no
/// signal. The lock-recovery side of #1235 is covered by
/// [`lock_circuit_worker_static`] (issue #1224); this worker just needs
/// the panic boundary.
pub fn start_circuit_worker(app: AppHandle) {
    std::thread::Builder::new()
        .name("circuit-worker".to_string())
        .spawn(move || {
            std::thread::sleep(STARTUP_DELAY);
            // Startup reconcile runs OUTSIDE the catch — a panic here
            // means the worker can't even start, so retrying on the
            // next pass would just panic again. The panic hook in
            // lib::setup already captures the cause in panic.log.
            startup_reconcile_pass(&app);
            let (lock, cvar) = &*WAKE;
            loop {
                // Each per-pass body is its own catch_unwind scope so
                // a panic in the interval-pass doesn't skip the
                // lost-turn watchdog (and vice-versa). `run_worker_pass`
                // logs the panic with the worker name and returns
                // false; we discard the return — recovery is the
                // important behaviour, not the signal.
                run_worker_pass("circuits:interval", || {
                    super::circuit_triggers::run_interval_pass();
                });
                run_worker_pass("circuits:github-poll", || {
                    super::circuit_triggers::maybe_poll_github();
                });
                run_worker_pass("circuits:drive", || run_pass(&app));
                run_worker_pass("circuits:watchdog", || {
                    lost_turn_watchdog_pass(&app);
                });
                // Wait for the next tick OR an immediate wake, whichever
                // first (`wait_timeout` returns either way).
                let guard = lock_circuit_worker_static(lock);
                let _ = cvar.wait_timeout(guard, TICK_INTERVAL).unwrap();
            }
        })
        .expect("circuit-worker thread spawn failed");
}

/// Drive this run even if its circuit is a draft. Background pollers
/// (`list_enabled_circuits`) never mint `manual:` identities; Trigger
/// Now does, and issue #1356 keeps that dry-run seam independent of
/// the enabled flag. Interval/GitHub runs on a disabled circuit stay
/// parked until the user opts in.
fn should_drive_circuit_run(enabled: bool, trigger_identity: &str) -> bool {
    enabled || trigger_identity.starts_with("manual:")
}

/// Issue #1467 admission gate. Returns `true` if a fresh `pending` run
/// on this mesh should fire its `Triggered` event this pass.
///
/// Semantics:
///   * The mesh-level cap (`meshes.circuit_run_capacity`, default 2)
///     counts **admitted** runs (`running`/`paused`). Deliberately
///     excludes `pending` — counting pending would self-deadlock (every
///     pending run's count read would see itself + peers, so
///     `count < cap` is always false and no run ever admits). See
///     `db::count_active_circuit_runs`'s doc for the full rationale.
///     Every admitted run holds one slot regardless of how many agent
///     nodes its blueprint fans out to.
///   * A `running` or `paused` run always passes the gate (it's already
///     been admitted; the legacy `set_circuit_run_state` flow doesn't
///     reject its target here).
///   * A `pending` run is admitted if the mesh's slot count is below
///     the cap; otherwise it stays `pending` in the DB and is re-
///     evaluated on the next pass (every 2s fast tick, plus the wake
///     condvar that fires when a terminal transition releases a slot).
///
/// **Read failure fails CLOSED** (admit = false) so a transient
/// DB hiccup never lets a run escalate past the gate without a count
/// proof — silence is preferable to over-admitting into a saturated
/// mesh. The error is logged loudly; on the next 2s tick the read is
/// retried.
fn may_admit_run(
    active: &db::ActiveCircuitRun,
    mesh: &crate::models::Mesh,
) -> bool {
    if active.run.state != "pending" {
        return true; // already admitted or never-needs-admission
    }
    let active_runs = match db::count_active_circuit_runs(active.run.mesh_id) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(
                "circuits: per-mesh active-run count failed for gate, failing closed: {}",
                e
            );
            return false;
        }
    };
    if active_runs < i64::from(mesh.circuit_run_capacity) {
        return true;
    }
    tracing::info!(
        "circuits: mesh {} held — {} active run(s) >= {} capacity, run {} stays pending",
        active.run.mesh_id,
        active_runs,
        mesh.circuit_run_capacity,
        active.run.id,
    );
    false
}

/// Number of agent process slots declared by a blueprint. This conservative
/// reservation is derived from the durable graph, not transient child
/// associations: a completed implementation step can still retain a live
/// process while a reviewer is spawned.
fn required_agent_slots(active: &db::ActiveCircuitRun) -> i64 {
    CircuitGraph::from_json(&active.circuit_graph_json)
        .map(|graph| {
            graph
                .nodes
                .iter()
                .filter(|node| crate::autopilot::circuit::model::consumes_agent_slot(&node.kind))
                .count() as i64
        })
        .unwrap_or(0)
}

fn agent_reservation_fits(
    mesh: &crate::models::Mesh,
    required: i64,
    mesh_reserved: i64,
    global_reserved: i64,
    nonleased_mesh_active: i64,
    nonleased_total: i64,
    global_pool: Option<u32>,
) -> bool {
    if required <= 0 {
        return true;
    }
    let mesh_free = i64::from(mesh.autopilot_concurrency_limit)
        .saturating_sub(nonleased_mesh_active)
        .saturating_sub(mesh_reserved);
    if mesh_free < required {
        return false;
    }
    global_pool
        .map(|pool| {
            i64::from(pool)
                .saturating_sub(nonleased_total)
                .saturating_sub(global_reserved)
                >= required
        })
        .unwrap_or(true)
}

/// One full pass over every active circuit run. Per-run failures are
/// logged and isolated — one broken run must not starve the others.
fn run_pass(app: &AppHandle) {
    let runs = match db::list_active_circuit_runs() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("circuits: could not list active runs: {}", e);
            return;
        }
    };
    // Issue #1263: drain approvals queued for runs that vanished
    // (deleted, completed, failed) between this user and last pass.
    // Borrows the already-loaded active-run slice so the sweep costs
    // zero heap on the hot 2-second tick.
    sweep_stale_approvals(&runs);

    // Issue #1467: cache the mesh row per unique mesh id so a mesh with
    // N pending runs but only 1 row read is the common case in the
    // deadlock scenario. `entry().or_insert_with()` makes the cache
    // transparent — the first pending run on each mesh reads, every
    // later one hits the cache. A `None` cache entry (mesh row missing
    // — likely deleted between run-mint and this pass) is logged once
    // per missing mesh id; the run stays pending until the orphan-
    // admission sweep clears it.
    use std::collections::HashMap;
    let mut mesh_cache: HashMap<i64, Option<crate::models::Mesh>> = HashMap::new();
    let mut reserved_by_mesh: HashMap<i64, i64> = HashMap::new();
    // Repair leases for active runs created before the reservation table was
    // introduced. This is idempotent and keeps restarts from losing a claim.
    for active in &runs {
        if matches!(active.run.state.as_str(), "running" | "paused") {
            let required = required_agent_slots(active);
            if required > 0 && db::circuit_agent_slots_reserved(active.run.id).unwrap_or(0) < required {
                let _ = db::reserve_circuit_agent_slots(active.run.id, required);
            }
        }
    }
    let global_pool = crate::preferences::autopilot_pool_size();
    // Active circuit runs are represented by their lease. Terminal runs do
    // not hold a lease, but retained implementation PTYs still consume host
    // capacity until their agent rows are archived/deleted; include those
    // non-leased agents in the admission baseline.
    let retained_total = db::count_retained_circuit_agent_nodes_total().unwrap_or(i64::MAX);
    let legacy_total = db::count_active_autopilot_nodes_total()
        .unwrap_or(i64::MAX)
        .saturating_add(retained_total);
    let mut global_reserved = db::count_reserved_circuit_agent_slots_total().unwrap_or(i64::MAX);
    for active in runs {
        // Pending runs that the gate deferred re-appear next pass;
        // running/paused runs always proceed (they already hold a slot).
        if active.run.state == "pending" {
            let mesh = mesh_cache
                .entry(active.run.mesh_id)
                .or_insert_with(|| db::get_mesh_by_id(active.run.mesh_id).ok());
            match mesh {
                Some(m) if !may_admit_run(&active, m) => continue,
                None => {
                    tracing::warn!(
                        "circuits: pending run {} on mesh_id={} cannot be gate-evaluated: \
                         mesh row missing (deleted between mint and this pass?); run stays pending",
                        active.run.id,
                        active.run.mesh_id,
                    );
                    continue;
                }
                Some(_) => {} // admitted by may_admit_run — fall through to drive
            }
        }
        if !should_drive_circuit_run(active.circuit_enabled, &active.run.trigger_identity) {
            continue;
        }
        if active.run.state == "pending" {
            if let Some(mesh) = mesh_cache.get(&active.run.mesh_id).and_then(|m| m.as_ref()) {
                let required = required_agent_slots(&active);
                if required > 0 {
                    let existing = db::circuit_agent_slots_reserved(active.run.id).unwrap_or(0);
                    let additional = required.saturating_sub(existing);
                    let mesh_reserved = *reserved_by_mesh
                        .entry(active.run.mesh_id)
                        .or_insert_with(|| db::count_reserved_circuit_agent_slots(active.run.mesh_id).unwrap_or(i64::MAX));
                    let retained_mesh_active = db::count_retained_circuit_agent_nodes(active.run.mesh_id).unwrap_or(i64::MAX);
                    let legacy_mesh_active = db::count_active_autopilot_nodes(active.run.mesh_id)
                        .unwrap_or(i64::MAX)
                        .saturating_add(retained_mesh_active);
                    if additional > 0
                        && !agent_reservation_fits(
                            mesh,
                            additional,
                            mesh_reserved,
                            global_reserved,
                            legacy_mesh_active,
                            legacy_total,
                            global_pool,
                        )
                    {
                        tracing::info!(
                            "circuits: mesh {} held — run {} needs {} reserved agent slot(s)",
                            active.run.mesh_id, active.run.id, required
                        );
                        continue;
                    }
                    if additional > 0 {
                        match db::reserve_circuit_agent_slots(active.run.id, required) {
                            Ok(true) => {
                                *reserved_by_mesh.get_mut(&active.run.mesh_id).unwrap() += additional;
                                global_reserved = global_reserved.saturating_add(additional);
                            }
                            Ok(false) | Err(_) => continue,
                        }
                    }
                }
            }
        }
        if let Err(e) = drive_run(app, &active) {
            tracing::warn!("circuits: run {} pass failed: {}", active.run.id, e);
        }
    }
}

fn drive_run(
    app: &AppHandle,
    active: &db::ActiveCircuitRun,
) -> Result<(), String> {
    let graph = CircuitGraph::from_json(&active.circuit_graph_json)?;
    let mut context = CircuitContext::from_json(&active.run.context_json)?;
    // Older runs (and the pre-seeding window) may lack `circuit.run_id`;
    // top it up on the first pass and persist through the normal commit.
    if context.get("circuit.run_id") != Some(active.run.id.to_string().as_str()) {
        context.with_run(active.run.id);
    }
    let mut view = RunView {
        run_id: active.run.id,
        graph,
        state: RunState::from_db_str(&active.run.state),
        context: context.clone(),
        steps: load_steps(active.run.id)?,
    };

    if let Some(source) = active.run.source_agent_node_id {
        let lost = db::get_agent_node_by_id(source).map(|n|
            matches!(n.status, SessionStatus::Archived | SessionStatus::Error)
        ).unwrap_or(true);
        if lost {
            db::commit_circuit_advance(active.run.id, Some("failed"), None, &[])
                .map_err(|e| e.to_string())?;
            close_run_agents(&view);
            crate::autopilot::evaluator::unregister(source);
            let _ = app.emit("circuit-run-updated", CircuitRunUpdatedPayload { run_id: active.run.id, state: "failed".into() });
            return Ok(());
        }
        crate::autopilot::evaluator::register_circuit(source);
    }

    for event in observe(app, active, &view) {
        let transition = advance(&mut view, &event);

        // Commit FIRST (atomically), then execute effects — a crash
        // after the commit is repaired by observation next pass. The
        // (possibly run_id-topped-up) context rides along with every
        // commit so the seeding above lands whenever anything else
        // writes; a pass with no commits simply re-seeds next time.
        if !transition.step_writes.is_empty()
            || transition.run_state_changed
            || transition.context_changed
        {
            let ops = transition
                .step_writes
                .iter()
                .map(|w| db::CircuitStepOp {
                    node_id: w.node_id.clone(),
                    status: w.status.as_db_str().to_string(),
                    outcome: w.outcome.map(|o| o.map(|v| v.as_db_str().to_string())),
                    error: w.error.clone(),
                    agent_node_id: None,
                    attempt: w.attempt,
                    fresh_attempt: w.fresh_attempt,
                })
                .collect::<Vec<_>>();
            let run_state = if transition.run_state_changed {
                Some(view.state.as_db_str())
            } else {
                None
            };
            db::commit_circuit_advance(
                active.run.id,
                run_state,
                Some(&view.context.to_json()?),
                &ops,
            )
            .map_err(|e| format!("commit failed: {}", e))?;
        }

        if let Err(e) = execute_effects(app, active, &mut view, &transition.effects) {
            // An effect that fails synchronously (e.g. the spawn row
            // creation) must not leave its step Running forever — the
            // observation loop has nothing to observe and would wedge
            // the run. Fail the offending step directly; the next pass's
            // sweep cancels the siblings.
            tracing::warn!("circuits: run {} effect failed: {}", active.run.id, e);
            let failed: Vec<String> = transition
                .effects
                .iter()
                .filter_map(|eff| match eff {
                    crate::autopilot::circuit::stepper::Effect::SpawnAgentNode { node_id }
                    | crate::autopilot::circuit::stepper::Effect::InjectPty { node_id, .. }
                    | crate::autopilot::circuit::stepper::Effect::SetNodeStatus { node_id, .. }
                    | crate::autopilot::circuit::stepper::Effect::CloseAgentNode { node_id, .. }
                    | crate::autopilot::circuit::stepper::Effect::CallGithub { node_id, .. } => {
                        Some(node_id.clone())
                    }
                    _ => None,
                })
                .collect();
let ops = failed
                .iter()
                .map(|node_id| db::CircuitStepOp {
                    node_id: node_id.clone(),
                    status: "failed".to_string(),
                    outcome: Some(Some("failed".to_string())),
                    error: Some(e.clone()),
                    agent_node_id: None,
                    attempt: 1,
                    fresh_attempt: false,
                })
                .collect::<Vec<_>>();
            db::commit_circuit_advance(
                active.run.id,
                Some(crate::autopilot::circuit::stepper::RunState::Failed.as_db_str()),
                None,
                &ops,
            )
            .map_err(|commit_err| format!("effect-failure commit also failed: {}", commit_err))?;
            view.state = RunState::Failed;
            let _ = app.emit(
                "circuit-run-updated",
                CircuitRunUpdatedPayload {
                    run_id: active.run.id,
                    state: "failed".to_string(),
                },
            );
        }

        // Live ledger: every step transition or state change refreshes
        // the Probe tab, not just terminal ones — otherwise a long agent
        // run renders as a frozen list until it finishes.
        if !transition.step_writes.is_empty()
            || transition.run_state_changed
            || transition.context_changed
        {
            let _ = app.emit(
                "circuit-run-updated",
                CircuitRunUpdatedPayload {
                    run_id: active.run.id,
                    state: view.state.as_db_str().to_string(),
                },
            );
        }

        // A failed circuit has no future step that can close its agents. Do
        // the same best-effort cleanup used by an explicit CloseAgentNode so
        // reviewer processes/worktrees cannot leak after a spawn, injection,
        // classifier, or close effect failure.
        if view.state == RunState::Failed {
            close_run_agents(&view);
            break;
        }
    }
    if matches!(view.state, RunState::Completed | RunState::Failed) {
        if let Some(source) = active.run.source_agent_node_id {
            crate::autopilot::evaluator::unregister(source);
        }
    }
    Ok(())
}

/// Retire every agent attached to a failed circuit run. The operation is
/// idempotent with the normal close effect: a missing row simply means a
/// previous cleanup already won the race.
fn close_run_agents(view: &RunView) {
    let mut agent_ids = HashSet::new();
    for agent_node_id in view.steps.iter().filter_map(|step| step.agent_node_id) {
        if !agent_ids.insert(agent_node_id) {
            continue;
        }
        match db::get_agent_node_by_id(agent_node_id) {
            Ok(_) => {
                if let Err(error) = crate::services::agent_node::delete(agent_node_id, true) {
                    tracing::warn!(
                        "circuits: failed to clean up agent {} after run failure: {}",
                        agent_node_id,
                        error
                    );
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(error) => tracing::warn!(
                "circuits: could not inspect agent {} during failed-run cleanup: {}",
                agent_node_id,
                error
            ),
        }
        crate::autopilot::evaluator::unregister(agent_node_id);
    }
}

/// Load this run's committed steps into the stepper's view shape.
fn load_steps(run_id: i64) -> Result<Vec<StepView>, String> {
    let rows = db::list_circuit_run_steps(run_id).map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|row| StepView {
            node_id: row.node_id,
            status: StepStatus::from_db_str(&row.status),
            outcome: row.outcome.as_deref().and_then(GraphStepOutcome::from_db_str),
            error: row.error_message,
            agent_node_id: row.agent_node_id,
            attempt: row.attempt,
        })
        .collect())
}

/// Resolve the agent node id the per-tick observer should check existence
/// for. Returns the step's direct `agent_node_id` when set (the
/// `SpawnAgentNode` case — spawn owns the agent it created), or — for
/// steps that act on an upstream spawned agent (`InjectPty`,
/// `LlmTurnClassifier`, `SetNodeStatus`, `CloseAgentNode`) — the resolved
/// lineage agent via [`resolve_target_agent`] (or the borrowed `$source`
/// context binding). Returns `None` when
/// neither applies (not a piloted step, or no spawn in lineage).
///
/// Used by [`observe`] and the matching startup-recovery pass to detect
/// orphaned injection/classifier steps whose piloted agent row has been
/// deleted. Without this seam, a `running` `InjectPty` whose target
/// agent_node vanished mid-run would stay `running` forever, holding a
/// `circuit_run_capacity` slot indefinitely.
fn observed_agent_for_step(
    step: &StepView,
    graph: &CircuitGraph,
    steps: &[StepView],
    source_agent_id: Option<i64>,
) -> Option<i64> {
    let piloted = matches!(
        graph.node(&step.node_id).map(|node| &node.kind),
        Some(
            CircuitNodeKind::SpawnAgentNode { .. }
                | CircuitNodeKind::InjectPty { .. }
                | CircuitNodeKind::LlmTurnClassifier { .. }
                | CircuitNodeKind::AwaitAgentTurn { .. }
                | CircuitNodeKind::ReviewVerdict { .. }
                | CircuitNodeKind::SetNodeStatus { .. }
                | CircuitNodeKind::CloseAgentNode { .. }
        )
    );
    if !piloted {
        return None;
    }
    let source_bound = match graph.node(&step.node_id).map(|node| &node.kind) {
        Some(CircuitNodeKind::InjectPty { target_node_id, .. })
        | Some(CircuitNodeKind::LlmTurnClassifier { target_node_id })
        | Some(CircuitNodeKind::AwaitAgentTurn { target_node_id })
        | Some(CircuitNodeKind::ReviewVerdict { target_node_id }) => {
            target_node_id.as_deref() == Some("$source")
        }
        _ => false,
    };
    if source_bound {
        return source_agent_id;
    }
    step.agent_node_id.or_else(|| {
        crate::autopilot::circuit::stepper::resolve_target_agent(graph, steps, &step.node_id)
    })
}

/// Observe the world and turn it into pure events for this run.
fn observe(_app: &AppHandle, active: &db::ActiveCircuitRun, view: &RunView) -> Vec<CircuitEvent> {
    let mut events = Vec::new();

    // A pending run fires now. Runs exist in Pending only because an
    // actual trigger dispatch minted them — manual Trigger Now (milestone
    // 1) or a freshly-ingested GitHub/interval trigger (milestone 3,
    // `circuit_triggers`) — so the event is trigger-kind agnostic.
    if view.state == RunState::Pending {
        events.push(CircuitEvent::Triggered);
    }

    // Piloted-agent observation for running steps bound to agent nodes.
    //
    // Two observation sources per step:
    //   * **direct** — `SpawnAgentNode` carries the agent it owns on
    //     `step.agent_node_id`. The original code path.
    //   * **lineage** — `InjectPty` / `LlmTurnClassifier` / `SetNodeStatus` /
    //     `CloseAgentNode` act on an upstream spawned agent; their own
    //     `step.agent_node_id` is NULL. If the upstream spawn's agent is
    //     deleted mid-run, the dependent step stays `running` forever
    //     (the legacy draft-only `SpawnAgentNode` filter let this leak —
    //     issue surfaced via `buildmesh` mesh 65, runs 3/5/6).
    //
    // Both paths converge on the same AgentLost event so the stepper
    // cancels the step via `cancel_step` and the run reaches a terminal
    // state through the normal cascade.
    for step in &view.steps {
        if step.status != StepStatus::Running {
            continue;
        }
        let Some(agent_node_id) = observed_agent_for_step(
            step,
            &view.graph,
            &view.steps,
            view.context.source_agent_id(),
        ) else {
            continue;
        };
        let node = db::get_agent_node_by_id(agent_node_id).ok();
        match node {
            // Closed/deleted mid-run → clean cancel.
            None => {
                events.push(CircuitEvent::AgentLost { agent_node_id });
            }
            Some(n) => match n.status {
                SessionStatus::Archived => {
                    events.push(CircuitEvent::AgentLost { agent_node_id });
                }
                SessionStatus::Error => {
                    let tail = crate::autopilot::evaluator::cleaned_turn_tail(agent_node_id);
                    events.push(CircuitEvent::AgentFinished {
                        agent_node_id,
                        success: false,
                        output: Some(tail),
                    });
                }
                // Milestone-1 completion heuristic: the turn detector's
                // `awaiting_input`/`ready` write (issue #1364) or an explicit
                // `completed` means the piloted agent finished its work.
                // Keystrokes never write these statuses, so manual PTY
                // interaction cannot produce a false completion. Known
                // limitation: an author-authored SetNodeStatus(completed)
                // effect on the piloted node would also read as completion
                // here — the milestone-2 LLM classifier gate replaces this
                // heuristic.
                SessionStatus::AwaitingInput | SessionStatus::Ready | SessionStatus::Completed => {
                    let tail = crate::autopilot::evaluator::cleaned_turn_tail(agent_node_id);
                    events.push(CircuitEvent::AgentFinished {
                        agent_node_id,
                        success: true,
                        output: Some(tail),
                    });
                }
                _ => {}
            },
        }
    }

    // A GitHub mutation is committed as Running before the network call. If
    // the process dies after the remote mutation but before its result lands,
    // the action has no live process to observe. Re-drive it on the next pass;
    // the review blueprint's OpenPr effect first discovers an existing PR, so
    // the crash window is safe to replay.
    for step in &view.steps {
        if step.status == StepStatus::Running
            && matches!(
                view.graph.node(&step.node_id).map(|node| &node.kind),
                Some(CircuitNodeKind::GithubAction { .. })
            )
        {
            events.push(CircuitEvent::GithubActionRetry {
                node_id: step.node_id.clone(),
            });
        }
    }

    // CloseAgentNode is committed complete before its destructive effect is
    // executed. Replay it while the completed step still points at an agent;
    // this closes the crash window where a reviewer row could survive a
    // restart after the step commit but before deletion.
    observe_close_agent_retries(view, &mut events);

    // Injection readiness: any running InjectPty step whose target
    // process is now live fires its AgentReady event.
    for step in &view.steps {
        if step.status == StepStatus::Running {
            if let Some(CircuitNodeKind::InjectPty { .. }) =
                view.graph.node(&step.node_id).map(|n| &n.kind)
            {
                if let Some(agent_node_id) =
                    view.resolve_target_agent(&step.node_id)
                {
                    if crate::agent::process::PROCESS_REGISTRY.is_alive(&agent_node_id) {
                        events.push(CircuitEvent::AgentReady {
                            node_id: step.node_id.clone(),
                        });
                    }
                }
            }
        }
    }

    // Milestone-2 gate observation (#1207). Skipped while paused — a
    // parked run must not burn classifier calls or run verification
    // commands; the gates re-evaluate after Resume.
    if view.state == RunState::Running {
        observe_gates(active, view, &mut events, _app);
    }

    // Issue-triggered review blueprints always include a CollaboratorCheck so
    // an untrusted issue can park visibly for approval. The trigger pass has
    // already made the network-backed trust decision and records `auto` in
    // the run context for trusted authors; turn that durable decision into
    // the same event the Probe's Approve button emits.
    if view.state == RunState::Running
        && view.context.get("autopilot.collaborator_gate") == Some("auto")
    {
        for step in &view.steps {
            if step.status == StepStatus::Blocked
                && matches!(
                    view.graph.node(&step.node_id).map(|node| &node.kind),
                    Some(CircuitNodeKind::CollaboratorCheck { require_approval: true })
                )
            {
                events.push(CircuitEvent::CollaboratorApproved {
                    node_id: step.node_id.clone(),
                });
            }
        }
    }

    // Collaborator approvals queued since the last pass.
    for node_id in drain_approvals_for(active.run.id) {
        events.push(CircuitEvent::CollaboratorApproved { node_id });
    }

    // Capacity snapshot for scheduling. Every failure here fails CLOSED
    // (zero capacity — the run parks in pending_slot until the next
    // pass), but loudly: a silent permanent queue would look exactly
    // like a busy mesh.
    let mesh_limit = match db::get_mesh_by_id(active.run.mesh_id) {
        Ok(m) => i64::from(m.autopilot_concurrency_limit),
        Err(e) => {
            tracing::warn!(
                "circuits: could not read mesh {} for capacity snapshot, failing closed: {}",
                active.run.mesh_id,
                e
            );
            0
        }
    };
    let circuit_running =
        db::count_running_circuit_steps(active.run.circuit_id).unwrap_or_else(|e| {
            tracing::warn!("circuits: running-step count failed, failing closed: {}", e);
            i64::MAX
        });
    let legacy_mesh_active = db::count_active_autopilot_nodes(active.run.mesh_id)
        .unwrap_or_else(|e| {
            tracing::warn!(
                "circuits: legacy active-agent count failed, failing closed: {}",
                e
            );
            i64::MAX
        });
    let circuit_mesh_active =
        db::count_active_circuit_agent_nodes(active.run.mesh_id).unwrap_or_else(|e| {
            tracing::warn!("circuits: active-agent count failed, failing closed: {}", e);
            i64::MAX
        });
    let retained_mesh_active =
        db::count_retained_circuit_agent_nodes(active.run.mesh_id).unwrap_or_else(|e| {
            tracing::warn!(
                "circuits: retained-agent count failed, failing closed: {}",
                e
            );
            i64::MAX
        });
    let mesh_active = legacy_mesh_active
        .saturating_add(circuit_mesh_active)
        .saturating_add(retained_mesh_active);
    let global_free_slots = match crate::preferences::autopilot_pool_size() {
        None => i64::MAX,
        Some(pool) => {
            let legacy_total = db::count_active_autopilot_nodes_total();
            let circuit_total = db::count_active_circuit_agent_nodes_total();
            let retained_total = db::count_retained_circuit_agent_nodes_total();
            match (legacy_total, circuit_total, retained_total) {
                (Ok(legacy), Ok(circuits), Ok(retained)) => {
                    i64::from(pool).saturating_sub(
                        legacy.saturating_add(circuits).saturating_add(retained),
                    )
                }
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                    tracing::warn!(
                        "circuits: global Autopilot pool count failed, failing closed: {}",
                        e
                    );
                    0
                }
            }
        }
    };
    let reserved_for_run = db::circuit_agent_slots_reserved(active.run.id).unwrap_or(0);
    let owned_by_run = db::count_active_circuit_agent_nodes_for_run(active.run.id).unwrap_or(0);
    let lease_free = reserved_for_run.saturating_sub(owned_by_run);
    events.push(CircuitEvent::Tick(Capacity {
        circuit_free_slots: active.circuit_concurrency_limit - circuit_running,
        mesh_agent_free_slots: (mesh_limit - mesh_active)
            .min(global_free_slots)
            .min(lease_free),
    }));

    events
}

/// Add replay events for close effects whose target association is still
/// present. Once the effect clears the target SpawnAgentNode association,
/// this returns no event on the next observation pass.
fn observe_close_agent_retries(view: &RunView, events: &mut Vec<CircuitEvent>) {
    for step in &view.steps {
        if step.status == StepStatus::Completed {
            if let Some(CircuitNodeKind::CloseAgentNode { .. }) =
                view.graph.node(&step.node_id).map(|node| &node.kind)
            {
                // A completed close belongs to its review round, not a
                // later reviewer attached to the same spawn step.
                if view.resolve_target_agent(&step.node_id).is_some_and(|id|
                    view.steps.iter().any(|target| target.agent_node_id == Some(id)
                        && target.attempt > step.attempt)) {
                    continue;
                }
                if view
                    .resolve_target_agent(&step.node_id)
                    .is_some()
                {
                    events.push(CircuitEvent::CloseAgentRetry {
                        node_id: step.node_id.clone(),
                    });
                }
            }
        }
    }
}

/// Gate observation (#1207): for each Running gate step, perform the
/// impure part of the gate (LLM classification / deterministic command)
/// and feed the result back as a pure event in THIS pass — so a gate
/// decision lands without waiting another tick.
fn observe_gates(
    active: &db::ActiveCircuitRun,
    view: &RunView,
    events: &mut Vec<CircuitEvent>,
    app: &AppHandle,
) {
    for step in &view.steps {
        if step.status != StepStatus::Running {
            continue;
        }
        match view.graph.node(&step.node_id).map(|n| &n.kind) {
            Some(CircuitNodeKind::AwaitAgentTurn { .. } | CircuitNodeKind::LlmTurnClassifier { .. } | CircuitNodeKind::ReviewVerdict { .. }) => {
                if let Some((agent_node_id, classification, output)) =
                    classify_step_turn(active, view, &step.node_id)
                {
                    if matches!(classification, Some(crate::autopilot::evaluator::Classification::Blocked))
                    {
                        let issue = view
                            .context
                            .get("issue.number")
                            .and_then(|number| number.parse::<i64>().ok())
                            .unwrap_or(0);
                        let _ = app.emit(
                            "autopilot-blocked",
                            crate::autopilot::pipeline::AutopilotBlockedPayload {
                                node_id: agent_node_id,
                                issue,
                            },
                        );
                    }
                    events.push(CircuitEvent::TurnClassified {
                        node_id: step.node_id.clone(),
                        classification,
                        output: Some(output),
                    });
                }
            }
            Some(CircuitNodeKind::DeterministicVerification { command }) => {
                let resolved = view.context.resolve(command);
                let mesh_path = db::get_mesh_by_id(active.run.mesh_id)
                    .map(|m| m.path)
                    .unwrap_or_default();
                let green = run_verification_command(&mesh_path, &resolved);
                tracing::info!(
                    "circuits: verification '{}' → {} for run {}",
                    resolved,
                    if green { "green" } else { "red" },
                    active.run.id
                );
                events.push(CircuitEvent::VerificationResult {
                    node_id: step.node_id.clone(),
                    green,
                });
            }
            _ => {}
        }
    }
}

/// Classify the specific branch's upstream piloted agent's latest turn, or
/// `None` when there is nothing to classify yet. Fires only on a FRESH yield:
/// the agent must be sitting at its input prompt (`awaiting_input`/`completed`)
/// AND have produced PTY output more recently than the last evaluation started.
fn classify_step_turn(
    active: &db::ActiveCircuitRun,
    view: &RunView,
    node_id: &str,
) -> Option<(i64, Option<crate::autopilot::evaluator::Classification>, String)> {
    use crate::autopilot::evaluator;
    let agent_node_id = view.resolve_target_agent(node_id)?;
    if !crate::agent::process::PROCESS_REGISTRY.is_alive(&agent_node_id) {
        return None;
    }
    let yielded = matches!(
        db::get_agent_node_by_id(agent_node_id).map(|n| n.status),
        Ok(SessionStatus::AwaitingInput)
            | Ok(SessionStatus::Ready)
            | Ok(SessionStatus::Completed)
    );
    if !yielded {
        return None;
    }
    let initial_source = matches!(view.graph.node(node_id).map(|n| &n.kind), Some(CircuitNodeKind::AwaitAgentTurn { .. }));
    let initial_output = if initial_source {
        db::get_agent_node_by_id(agent_node_id).ok()
            .and_then(|node| crate::coordinator::enrichment::digest_enrichment(&node))
            .and_then(|tail| match tail {
                crate::services::transcript_reader::TranscriptTail::Available { last_assistant_message, .. } => last_assistant_message,
                _ => None,
            })
    } else { None };
    let mut output = initial_output.unwrap_or_else(|| evaluator::cleaned_turn_tail(agent_node_id));
    let fresh_output = match (
        evaluator::millis_since_last_output(agent_node_id),
        evaluator::millis_since_last_evaluation(agent_node_id),
    ) {
        (Some(output), Some(eval)) => output < eval,
        (Some(_), None) => true,
        _ => false,
    };
    if initial_source {
        if view.context.get(&format!("node.{}.evaluated_output", node_id)) == Some(output.as_str()) {
            return None;
        }
    } else if !fresh_output {
        return None;
    }
    // Evaluator backend env: the mesh's Autopilot provider side-channel
    // (never the node's own model — the #824 lesson).
    let backend_provider = db::get_mesh_by_id(active.run.mesh_id)
        .ok()
        .map(|mesh| crate::services::autopilot::configured_autopilot_provider(&mesh))
        .unwrap_or_else(|| "claude".to_string());
    let backend_env = crate::session_naming::naming_backend_env(&backend_provider);
    evaluator::note_evaluation(agent_node_id);
    let classification = if initial_source {
        let verdict = if output.trim().is_empty() { None } else {
            evaluator::classify_with_prompt(agent_node_id, &backend_env, &evaluator::classify_prompt(&output))
        };
        if verdict.is_none() {
            output = "Task completion could not be verified. Confirm the source task is finished using the approval step in this Circuit, or cancel and continue the agent.".into();
        }
        Some(verdict.unwrap_or(evaluator::Classification::Blocked))
    } else if matches!(view.graph.node(node_id).map(|n| &n.kind), Some(CircuitNodeKind::ReviewVerdict { .. })) {
        Some(evaluator::classify_review(agent_node_id, &backend_env)
            .unwrap_or(evaluator::Classification::Blocked))
    } else {
        evaluator::classify(agent_node_id, &backend_env)
    };
    tracing::info!(
        "circuits: turn classifier for run {} step {} agent {} → {:?}",
        active.run.id,
        node_id,
        agent_node_id,
        classification
    );
    Some((agent_node_id, classification, output))
}

/// Run a DeterministicVerification command in the mesh directory and
/// report green (exit 0) / red. Bounded wait (2 minutes), then kill and
/// call it red — a hung check must not wedge the worker thread.
fn run_verification_command(mesh_path: &str, command: &str) -> bool {
    let (program, prefix): (&str, &[&str]) =
        if cfg!(windows) { ("cmd", &["/C"]) } else { ("sh", &["-c"]) };
    let mut cmd = crate::process_util::command_no_window(program);
    cmd.args(prefix).arg(command).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if !mesh_path.is_empty() {
        cmd.current_dir(mesh_path);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("circuits: verification '{command}' failed to spawn: {}", e);
            return false;
        }
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    tracing::warn!("circuits: verification '{command}' timed out after 120s");
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Err(e) => {
                tracing::warn!("circuits: verification '{command}' wait failed: {}", e);
                let _ = child.kill();
                return false;
            }
        }
    }
}

///// Determine the target (issue vs PR number) for a GitHub action.
/// If the action is CloseIssue, it explicitly requires an issue trigger.
/// If this node has an upstream OpenPr node in its lineage, it targets that PR (pr.number).
/// Otherwise, it falls back to issue.number if present, then pr.number.
fn determine_github_target(
    view: &RunView,
    node_id: &str,
    action: crate::autopilot::circuit::model::GithubActionKind,
) -> Result<(&'static str, i64), String> {
    use crate::autopilot::circuit::model::GithubActionKind;
    if action == GithubActionKind::CloseIssue {
        let num = view
            .context
            .get("issue.number")
            .and_then(|n| n.parse::<i64>().ok())
            .ok_or_else(|| "CloseIssue requires an issue-triggered run with issue.number".to_string())?;
        return Ok(("issue", num));
    }

    let has_upstream_open_pr = view.has_upstream_node_of_kind(node_id, |kind| {
        matches!(kind, CircuitNodeKind::GithubAction { action: GithubActionKind::OpenPr, .. })
    });

    if has_upstream_open_pr {
        if let Some(pr_num) = view.context.get("pr.number").and_then(|n| n.parse::<i64>().ok()) {
            return Ok(("pr", pr_num));
        }
    }

    if let Some(issue_num) = view.context.get("issue.number").and_then(|n| n.parse::<i64>().ok()) {
        Ok(("issue", issue_num))
    } else if let Some(pr_num) = view.context.get("pr.number").and_then(|n| n.parse::<i64>().ok()) {
        Ok(("pr", pr_num))
    } else {
        Err("GitHub action has no issue/pr context — the circuit needs a GitHub trigger upstream of this node".to_string())
    }
}

/// Reconcile the implementation branch with GitHub before emitting the durable
/// result. Inject the external observations so replay and lookup failures can
/// be exercised without a process-wide database or GitHub writes.
fn ensure_open_pr(
    view: &RunView,
    node_id: &str,
    policy: Option<crate::autopilot::circuit::model::OpenPrPolicy>,
    observe: impl FnOnce(i64) -> Result<crate::autopilot::pipeline::WrapupState, String>,
    find: impl FnOnce(&str) -> Result<Option<crate::services::github::PullRequest>, String>,
    create: impl FnOnce(&str, &str) -> Result<crate::services::github::PullRequest, String>,
) -> Result<CircuitEvent, String> {
    let agent_node_id = view.resolve_open_pr_agent(node_id)
        .ok_or_else(|| "OpenPr requires a spawned agent earlier in this run".to_string())?;
    let wrapup = observe(agent_node_id)?;
    let reasons = crate::autopilot::pipeline::wrapup_reasons(&wrapup);
    if !reasons.is_empty() {
        return Err(format!("autopilot wrap-up verification failed: {}", reasons.join("; ")));
    }
    let head = wrapup
        .branch
        .clone()
        .ok_or_else(|| "the implementation worktree has no checked-out branch".to_string())?;
    let title = view.context.get("issue.title")
        .or_else(|| view.context.get("pr.title"))
        .unwrap_or("Circuit run").to_string();
    // A replay after GitHub accepted creation but before the ledger commit
    // must discover that PR, never create a second one.
    let pr = match find(&head)? {
        Some(pr) => pr,
        None if policy.is_some_and(|policy| policy.requires_existing()) => {
            return Err("the implementation agent did not raise an open pull request for its branch".to_string());
        }
        None => create(&head, &title)?,
    };
    let head_ref = if pr.head_ref.trim().is_empty() { head } else { pr.head_ref };
    let title = if pr.title.trim().is_empty() { title } else { pr.title };
    Ok(CircuitEvent::GithubActionResult {
        node_id: node_id.to_string(), success: true, pr_number: Some(pr.number),
        pr_url: Some(pr.html_url), pr_head_ref: Some(head_ref),
        pr_title: if title.is_empty() { None } else { Some(title) }, error: None,
    })
}

/// Perform one `CallGithub` effect (milestone 3, issue #1208): resolve
/// the target repo from the mesh's `origin`, execute the mutation through
/// the shared [`crate::services::github::GitHubClient`] seam, and advance
/// the stepper with the result so context updates (e.g. `pr.*`) commit atomically
/// before downstream nodes cascade.
fn call_github_effect(
    app: &AppHandle,
    active: &db::ActiveCircuitRun,
    view: &mut RunView,
    node_id: &str,
    action: crate::autopilot::circuit::model::GithubActionKind,
    label: Option<&str>,
    comment: Option<&str>,
) -> Result<(), String> {
    use crate::autopilot::circuit::model::GithubActionKind;
    use crate::services::github::GitHubClient;

    let mesh = db::get_mesh_by_id(active.run.mesh_id).map_err(|e| e.to_string())?;
    let (owner, repo) = crate::commands::pr::resolve_github_owner_repo(&mesh)?;
    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    let resolved_comment = comment.map(|c| view.context.resolve(c));
    let open_pr_policy = match view.graph.node(node_id).map(|node| &node.kind) {
        Some(CircuitNodeKind::GithubAction { open_pr_policy, .. }) => *open_pr_policy,
        _ => None,
    };

    let action_res: Result<CircuitEvent, String> = (|| {
        match action {
            GithubActionKind::AddLabel => {
                let target = determine_github_target(view, node_id, action)?;
                let label = label.ok_or_else(|| "AddLabel requires a label".to_string())?;
                client
                    .add_issue_label(&owner, &repo, target.1, &view.context.resolve(label))
                    .map_err(|e| e.to_string())?;
                Ok(CircuitEvent::GithubActionResult {
                    node_id: node_id.to_string(),
                    success: true,
                    pr_number: None,
                    pr_url: None,
                    pr_head_ref: None,
                    pr_title: None,
                    error: None,
                })
            }
            GithubActionKind::RemoveLabel => {
                let target = determine_github_target(view, node_id, action)?;
                let label = label.ok_or_else(|| "RemoveLabel requires a label".to_string())?;
                client
                    .remove_issue_label(&owner, &repo, target.1, &view.context.resolve(label))
                    .map_err(|e| e.to_string())?;
                Ok(CircuitEvent::GithubActionResult {
                    node_id: node_id.to_string(),
                    success: true,
                    pr_number: None,
                    pr_url: None,
                    pr_head_ref: None,
                    pr_title: None,
                    error: None,
                })
            }
            GithubActionKind::PostComment => {
                let target = determine_github_target(view, node_id, action)?;
                let body = resolved_comment
                    .filter(|c| !c.trim().is_empty())
                    .ok_or_else(|| "PostComment requires a non-empty comment template".to_string())?;
                client
                    .add_issue_comment(&owner, &repo, target.1, &body)
                    .map_err(|e| e.to_string())?;
                Ok(CircuitEvent::GithubActionResult {
                    node_id: node_id.to_string(),
                    success: true,
                    pr_number: None,
                    pr_url: None,
                    pr_head_ref: None,
                    pr_title: None,
                    error: None,
                })
            }
            GithubActionKind::CloseIssue => {
                let target = determine_github_target(view, node_id, action)?;
                if target.0 != "issue" {
                    return Err("CloseIssue requires an issue-triggered run".to_string());
                }
                client
                    .close_issue(&owner, &repo, target.1)
                    .map_err(|e| e.to_string())?;
                Ok(CircuitEvent::GithubActionResult {
                    node_id: node_id.to_string(),
                    success: true,
                    pr_number: None,
                    pr_url: None,
                    pr_head_ref: None,
                    pr_title: None,
                    error: None,
                })
            }
            GithubActionKind::OpenPr => {
                if open_pr_policy.is_some_and(|policy| policy.requires_existing())
                    && crate::services::autopilot::configured_action_on_success(active.run.mesh_id)
                        == "none"
                {
                    return Err(
                        "this OpenPr action requires a pull-request wrap-up policy"
                            .to_string(),
                    );
                }
                let body = resolved_comment.unwrap_or_default();
                ensure_open_pr(view, node_id, open_pr_policy,
                    |agent_node_id| {
                        let agent_node = db::get_agent_node_by_id(agent_node_id).map_err(|e| e.to_string())?;
                        if !agent_node.use_worktree {
                            return Err("OpenPr requires a worktree-backed agent (its commits have no branch)".to_string());
                        }
                        Ok(crate::autopilot::pipeline::observe_wrapup_git_state(&agent_node))
                    },
                    |head| client.find_open_pr_for_branch(&owner, &repo, head)
                        .map_err(|e| format!("could not verify the pull request for {owner}/{repo} branch {head}: {e}")),
                    |head, title| {
                        let base = crate::commands::git::get_default_branch_blocking(mesh.path.clone())?;
                        client.create_pull_request_details(&owner, &repo, title, &body, head, &base)
                            .map_err(|e| e.to_string())
                    },
                )
            }
        }
    })();

    let event = match action_res {
        Ok(ev) => ev,
        Err(err) => CircuitEvent::GithubActionResult {
            node_id: node_id.to_string(),
            success: false,
            pr_number: None,
            pr_url: None,
            pr_head_ref: None,
            pr_title: None,
            error: Some(err),
        },
    };

    let transition = advance(view, &event);
    if !transition.step_writes.is_empty() || transition.run_state_changed {
        let ops = transition
            .step_writes
            .iter()
            .map(|w| db::CircuitStepOp {
                node_id: w.node_id.clone(),
                status: w.status.as_db_str().to_string(),
                outcome: w.outcome.map(|o| o.map(|v| v.as_db_str().to_string())),
                error: w.error.clone(),
                agent_node_id: None,
                attempt: w.attempt,
                fresh_attempt: w.fresh_attempt,
            })
            .collect::<Vec<_>>();
        let run_state = if transition.run_state_changed {
            Some(view.state.as_db_str())
        } else {
            None
        };
        db::commit_circuit_advance(
            active.run.id,
            run_state,
            Some(&view.context.to_json()?),
            &ops,
        )
        .map_err(|e| format!("commit failed: {}", e))?;
    }

    if !transition.effects.is_empty() {
        execute_effects(app, active, view, &transition.effects)?;
    }

    tracing::info!(
        "circuits: run {} executed GitHub {:?} on {}/{}",
        active.run.id,
        action,
        owner,
        repo
    );
    Ok(())
}

/// Find the SpawnAgentNode row whose association owns a CloseAgentNode
/// target. Explicit targets already name that spawn step; omitted targets
/// use the same resolved agent as the close effect and find its owning step.
fn close_target_spawn_step_id(
    view: &RunView,
    close_node_id: &str,
    target_node_id: Option<&str>,
    target_agent_id: i64,
) -> String {
    let fallback_spawn_step_id = target_node_id.unwrap_or(close_node_id);
    target_node_id
        .map(str::to_owned)
        .or_else(|| {
            view.steps
                .iter()
                .find(|step| {
                    step.agent_node_id == Some(target_agent_id)
                        && matches!(
                            view.graph.node(&step.node_id).map(|node| &node.kind),
                            Some(CircuitNodeKind::SpawnAgentNode { .. })
                        )
                })
                .map(|step| step.node_id.clone())
        })
        .unwrap_or_else(|| fallback_spawn_step_id.to_string())
}

/// Execute one transition's effects against the real world. Takes the
/// view mutably: a spawn attaches its new agent node id to the in-memory
/// step (and the DB) so later effects in the same pass — and the
/// observation loop next pass — resolve targets from the view instead of
/// re-querying SQLite.
fn execute_effects(
    app: &AppHandle,
    active: &db::ActiveCircuitRun,
    view: &mut RunView,
    effects: &[crate::autopilot::circuit::stepper::Effect],
) -> Result<(), String> {
    use crate::autopilot::circuit::stepper::Effect;
    let effect_batch = begin_circuit_effect_batch(active.run.id);
    let run_state = db::get_circuit_run(active.run.id)
        .map_err(|e| e.to_string())?
        .map(|run| run.state);
    for effect in effects {
        if effect_batch.is_cancelled() {
            tracing::info!(
                "circuits: cancellation invalidated remaining effects for run {}",
                active.run.id
            );
            return Ok(());
        }
        let accepts_effect = run_state.as_deref().is_some_and(|state| {
            effect_allowed_in_state(state, view.state == RunState::Completed, effect)
        });
        if !accepts_effect {
            tracing::info!(
                "circuits: stopped effects for terminal/deleted run {}",
                active.run.id
            );
            return Ok(());
        }
        match effect {
            Effect::SpawnAgentNode { node_id } => {
                spawn_step_agent(app, active.run.id, active.run.mesh_id, view, node_id)?;
            }
            Effect::InjectPty { node_id, prompt, .. } => {
                match view.resolve_target_agent(node_id) {
                    Some(target) => {
                        // Mirrors `observe`'s agent-existence check: treat
                        // both "row deleted" AND "row archived" as lost.
                        // An archived row can't accept a PTY write, and
                        // returning Err here makes `drive_run` persist
                        // `status: "failed"` directly via
                        // `commit_circuit_advance` — no AgentLost event,
                        // no stepper cascade. That's intentional: the
                        // stepper only emits AgentLost via observation,
                        // and a missing target here means the row is
                        // already gone, so a direct write is honest.
                        let agent_alive = db::get_agent_node_by_id(target)
                            .ok()
                            .filter(|n| n.status != SessionStatus::Archived)
                            .is_some();
                        if !agent_alive {
                            let reason = format!(
                                "target agent {} for step {} was lost before prompt injection",
                                target, node_id
                            );
                            tracing::warn!("circuits: run {}: {}", active.run.id, reason);
                            return Err(reason);
                        }
                        crate::autopilot::evaluator::note_turn_start(target);
                        crate::autopilot::pipeline::write_prompt_to_pty(target, prompt, app)
                            .map_err(|e| format!("PTY injection failed: {}", e))?;
                        let _ = db::update_agent_node_status(target, SessionStatus::Running);
                        tracing::info!(
                            "circuits: injected prompt into agent {} for run {}",
                            target,
                            active.run.id
                        );
                    }
                    None => {
                        return Err(format!(
                            "circuits: run {} had no piloted agent in lineage to inject into (node {})",
                            active.run.id,
                            node_id
                        ));
                    }
                }
            }
            Effect::SetNodeStatus { node_id, status, .. } => {
                let agent_node_id = view
                    .resolve_target_agent(node_id)
                    .ok_or_else(|| format!("SetNodeStatus target agent not found in lineage for node {}", node_id))?;
                // Same orphan check as `Effect::InjectPty` — mirror the
                // per-tick observer's "deleted OR archived" semantics so a
                // row that vanishes between observe and execute is caught
                // here. Returns Err; `drive_run` persists `status: "failed"`
                // directly via `commit_circuit_advance` (no AgentLost
                // cascade — see InjectPty's note above).
                let agent_alive = db::get_agent_node_by_id(agent_node_id)
                    .ok()
                    .filter(|n| n.status != SessionStatus::Archived)
                    .is_some();
                if !agent_alive {
                    let reason = format!(
                        "target agent {} for step {} was lost before status write",
                        agent_node_id, node_id
                    );
                    tracing::warn!("circuits: run {}: {}", active.run.id, reason);
                    return Err(reason);
                }
                let kind = SessionStatus::from_db_str(status);
                db::update_agent_node_status(agent_node_id, kind)
                    .map_err(|e| format!("status write failed: {}", e))?;
            }
            Effect::CloseAgentNode { node_id, target_node_id } => {
                let target = view
                    .resolve_target_agent(node_id)
                    .ok_or_else(|| {
                        format!(
                            "CloseAgentNode target agent not found in lineage for node {}",
                            node_id
                        )
                    })?;
                // Closing is intentionally idempotent.  A previous worker
                // pass may have killed/deleted the row after committing the
                // close step but before the effect was retried.
                match db::get_agent_node_by_id(target) {
                    Ok(_) => {
                        crate::services::agent_node::delete(target, true)
                            .map_err(|e| format!("agent node close failed: {}", e))?;
                    }
                    Err(rusqlite::Error::QueryReturnedNoRows) => {}
                    Err(e) => {
                        return Err(format!("agent node lookup failed during close: {}", e));
                    }
                }
                let spawn_step_id = close_target_spawn_step_id(
                    view,
                    node_id,
                    target_node_id.as_deref(),
                    target,
                );
                db::clear_circuit_step_agent_node(active.run.id, &spawn_step_id)
                    .map_err(|e| format!("circuit close association cleanup failed: {}", e))?;
                if let Some(step) = view.step_mut(&spawn_step_id) {
                    step.agent_node_id = None;
                }
            }
            Effect::Notify { message } => {
                let _ = app.emit(
                    "circuit-notification",
                    CircuitNotificationPayload {
                        run_id: active.run.id,
                        message: message.clone(),
                        severity: notification_severity(message),
                    },
                );
            }
            Effect::CallGithub { node_id, action, label, comment } => {
                call_github_effect(app, active, view, node_id, *action, label.as_deref(), comment.as_deref())?;
            }
        }
    }
    Ok(())
}

/// Cancellation commits the terminal run state before retiring external
/// resources. The transition's effects take one durable-state snapshot before
/// execution and check an in-memory cancellation token before each effect;
/// terminal transitions retain only their synchronous cleanup and notification
/// effects, while InjectPty is never allowed after completion.
fn effect_allowed_in_state(state: &str, completing_transition: bool, effect: &crate::autopilot::circuit::stepper::Effect) -> bool {
    use crate::autopilot::circuit::stepper::Effect;
    matches!(state, "pending" | "running" | "paused")
        // Synchronous terminal actions are emitted by the same transition
        // that completes the run, and must survive its commit-before-effects.
        // InjectPty is intentionally absent: it starts new work after the
        // run has durably finished and would leave an untracked command.
        || (state == "completed" && completing_transition && matches!(effect,
            Effect::Notify { .. } | Effect::SetNodeStatus { .. } | Effect::CloseAgentNode { .. }))
}

fn run_accepts_effects(run_id: i64) -> Result<bool, String> {
    Ok(matches!(
        db::get_circuit_run(run_id).map_err(|error| error.to_string())?,
        Some(run) if matches!(run.state.as_str(), "pending" | "running" | "paused")
    ))
}

/// Pure seam of [`spawn_step_agent`] — translates a
/// [`CircuitNodeKind::SpawnAgentNode`] into the inputs the impure wrapper
/// threads into `create_pending` (for the `provider` column) and
/// `SpawnRequest::with_explicit(...)` (for cascade layer-1 model/effort/extra_args).
///
/// Mirrors the existing impure-wrapper / pure-core pattern this file uses
/// for `reconcile_spawn_step` (line ~947) so the cascade + capability-mask
/// contract can be tested without a Tauri runtime, AppHandle, or DB.
/// Issue #1358 / slice 3 of #1355 — `provider` / `model` / `effort` /
/// `extra_args` flow off the v2 AST here.
fn resolve_circuit_spawn_inputs(
    kind: &CircuitNodeKind,
) -> Result<ResolvedCircuitSpawn, String> {
    let CircuitNodeKind::SpawnAgentNode {
        prompt,
        name,
        provider,
        model,
        effort,
        extra_args,
    } = kind
    else {
        return Err(format!(
            "node is not a spawn node (got {:?})",
            std::mem::discriminant(kind)
        ));
    };
    // Provider: preserve the user-authored string for the `agent_nodes.provider`
    // row column. An unknown id stays as-is — the row carries the
    // user-authored value, and `Provider::from_db_str`'s Anthropic
    // fallback in `spawn_with_intent` handles legacy / mistyped ids.
    let provider_str = provider.clone();
    let prompt = prompt.clone();
    let name = name.clone();
    let explicit = ExplicitSpawnOverrides {
        model: model
            .as_deref()
            .and_then(non_empty_trim)
            .map(str::to_string),
        effort: effort
            .as_deref()
            .and_then(non_empty_trim)
            .map(str::to_string),
        extra_args: extra_args
            .as_deref()
            .and_then(non_empty_trim)
            .map(str::to_string),
    };
    Ok(ResolvedCircuitSpawn {
        prompt,
        name,
        provider_str,
        explicit,
    })
}

/// Mirrors `cascade_inputs_for`'s whitespace trim so the seam collapses
/// "   " / "\t\n" / "" to `None` before reaching the cascade (issue
/// #1148 AC #32). Inline rather than reaching into `crate::agent::spawn`
/// — this is the seam boundary for the cascade, not a place to deepen
/// the dependency surface.
fn non_empty_trim(s: &str) -> Option<&str> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// The output of [`resolve_circuit_spawn_inputs`]: the prompt + name
/// carried through verbatim, the optional per-step provider string for
/// `create_pending`, the layer-1 cascade override for
/// `SpawnRequest::with_explicit(...)`. The orchestrator doesn't
/// surface a resolved `Provider` here because `spawn_with_intent`
/// recomputes it from the `agent_nodes.provider` row it just wrote —
/// carrying a duplicate would be speculative generality.
#[derive(Debug)]
struct ResolvedCircuitSpawn {
    /// Author-authored prompt, carried verbatim — Mustache resolution
    /// happens in the wrapper against `view.context`.
    prompt: String,
    /// Author-authored agent node name, carried verbatim.
    name: Option<String>,
    /// User-authored provider string for the `agent_nodes.provider`
    /// column. `None` = fall through to the mesh's default at spawn.
    provider_str: Option<String>,
    /// Per-step cascade layer-1 override; passed to
    /// `SpawnRequest::with_explicit(...)`.
    explicit: ExplicitSpawnOverrides,
}

/// The SpawnAgentNode effect: create the pending row (stage-1), wire it
/// to the step, then schedule stage-2 in the background — mirroring the
/// autopilot launch order minus the GitHub ledger.
fn circuit_spawn_intent(
    delivery: crate::autopilot::launch::InitialPromptDelivery,
    prompt: &str,
) -> crate::agent::spawn::SpawnIntent {
    use crate::agent::spawn::SpawnIntent;
    use crate::autopilot::launch::InitialPromptDelivery;

    match delivery {
        InitialPromptDelivery::Prefill => SpawnIntent::Loop {
            initial_prompt: prompt.to_string(),
        },
        InitialPromptDelivery::Fresh | InitialPromptDelivery::InjectAfterSpawn => {
            SpawnIntent::Fresh
        }
    }
}

fn deliver_circuit_initial_prompt(
    app: &AppHandle,
    node_id: i64,
    prompt: &str,
    delivery: crate::autopilot::launch::InitialPromptDelivery,
) {
    use crate::autopilot::launch::InitialPromptDelivery;

    let result = match delivery {
        InitialPromptDelivery::Prefill => Ok(()),
        InitialPromptDelivery::InjectAfterSpawn => {
            crate::autopilot::pipeline::write_prompt_to_pty(node_id, prompt, app)
        }
        InitialPromptDelivery::Fresh => Ok(()),
    };

    if let Err(error) = result {
        tracing::error!(
            "circuits: fallback prompt injection for agent {} failed: {}",
            node_id,
            error
        );
        let _ = crate::agent::session_lifecycle::on_error(
            &crate::agent::session_lifecycle::AppSessionLifecycleSink { app },
            node_id,
        );
    }
}

fn schedule_circuit_initial_prompt(
    app: &AppHandle,
    node_id: i64,
    prompt: &str,
    delivery: crate::autopilot::launch::InitialPromptDelivery,
) {
    if delivery == crate::autopilot::launch::InitialPromptDelivery::Prefill {
        crate::autopilot::launch::watch_and_submit_for_circuit(app.clone(), node_id, prompt);
    }
}

async fn run_accepts_effects_async(run_id: i64) -> bool {
    tauri::async_runtime::spawn_blocking(move || run_accepts_effects(run_id).unwrap_or(false))
        .await
        .unwrap_or(false)
}

async fn abort_circuit_spawn_async(run_id: i64, node_id: i64) {
    let _ = tauri::async_runtime::spawn_blocking(move || abort_circuit_spawn(run_id, node_id))
        .await;
}

fn spawn_circuit_agent_in_background(
    app: &AppHandle,
    run_id: i64,
    node_id: i64,
    permit: CircuitSpawnPermit,
    explicit: ExplicitSpawnOverrides,
    worktree_policy: crate::agent::spawn::WorktreePolicy,
    prompt: String,
    delivery: crate::autopilot::launch::InitialPromptDelivery,
) {
    let app_for_spawn = app.clone();
    tauri::async_runtime::spawn(async move {
        // Keep the permit alive until every post-launch cancellation check
        // and compensation path has completed. Merely accepting it as an
        // unused parameter would drop it when this function returns.
        let _permit = permit;
        // The node is attached before this task is queued. Re-check the
        // durable run state inside the task as well as in the worker effect
        // loop: cancellation may have won while the task was waiting for a
        // runtime worker. A cancelled run must never start a new process.
        if !run_accepts_effects_async(run_id).await {
            abort_circuit_spawn_async(run_id, node_id).await;
            return;
        }
        let intent = circuit_spawn_intent(delivery, &prompt);
        if let Err(error) = crate::agent::spawn::spawn_with_intent(
            &app_for_spawn,
            crate::agent::spawn::SpawnRequest::new(node_id, intent, Default::default())
                .with_explicit(explicit)
                .with_worktree_policy(worktree_policy),
        )
        .await
        {
            if !run_accepts_effects_async(run_id).await {
                abort_circuit_spawn_async(run_id, node_id).await;
            }
            tracing::error!("circuits: agent node {} failed: {}", node_id, error);
            return;
        }
        // Cancellation can race with the process launch itself. Retire the
        // process and clear the step association after the launch completes
        // if the durable state flipped while the async spawn was in flight.
        // This compensating check also handles a ledger deletion that raced
        // before the task acquired the DB row.
        if !run_accepts_effects_async(run_id).await {
            abort_circuit_spawn_async(run_id, node_id).await;
            return;
        }
        schedule_circuit_initial_prompt(&app_for_spawn, node_id, &prompt, delivery);
        if !run_accepts_effects_async(run_id).await {
            abort_circuit_spawn_async(run_id, node_id).await;
            return;
        }
        deliver_circuit_initial_prompt(&app_for_spawn, node_id, &prompt, delivery);
    });
}

/// Retire a circuit spawn that lost a cancellation/delete race. The row may
/// already have been removed by the command layer, so process-registry cleanup
/// is deliberately attempted even when the normal Agent Node delete cannot
/// reload the row.
fn abort_circuit_spawn(run_id: i64, node_id: i64) {
    crate::agent::process::PROCESS_REGISTRY.kill_session(node_id);
    let retired = match db::get_agent_node_by_id(node_id) {
        Ok(_) => crate::services::agent_node::delete(node_id, true)
            .map_err(|error| error.to_string()),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(()),
        Err(error) => Err(error.to_string()),
    };
    if let Err(error) = retired {
        // Keep the step association intact: the command-side cleanup retry
        // needs this durable owner id if an OS/worktree lock is transient.
        tracing::warn!(
            "circuits: could not retire aborted spawn {} for run {}: {}",
            node_id, run_id, error
        );
        return;
    }
    let _ = db::clear_circuit_step_agent_node_by_agent_id(run_id, node_id);
}

fn spawn_step_agent(
    app: &AppHandle,
    run_id: i64,
    mesh_id: i64,
    view: &mut RunView,
    node_id: &str,
) -> Result<(), String> {
    use crate::agent::spawn::WorktreePolicy;
    let kind = view
        .graph
        .node(node_id)
        .map(|n| n.kind.clone())
        .ok_or_else(|| format!("node {} not in blueprint", node_id))?;
    // Pure seam (issue #1358): translate the AST kind into the
    // provider column value + cascade layer-1 overrides that the spawn
    // pipeline consumes downstream. Whitespace-only model/effort/extra_args
    // collapse to `None` here so the cascade falls through to the mesh or
    // application layer (mirrors `cascade_inputs_for`'s trim behaviour).
    let ResolvedCircuitSpawn {
        prompt,
        name,
        mut provider_str,
        mut explicit,
    } = resolve_circuit_spawn_inputs(&kind)?;

    // The built-in review graph is shared by all node-started runs in a mesh.
    // Resolve the source's provider/model/effort from this run's context so a
    // later invocation cannot inherit configuration from the run that first
    // created the canonical preset row.
    if view.context.get("source.review_preset") == Some("1") {
        if let Some(provider) = view
            .context
            .get("source.provider")
            .and_then(non_empty_trim)
        {
            provider_str = Some(provider.to_string());
        }
        explicit.model = view
            .context
            .get("source.model")
            .and_then(non_empty_trim)
            .map(str::to_string);
        explicit.effort = view
            .context
            .get("source.effort")
            .and_then(non_empty_trim)
            .map(str::to_string);
    }

    let resolved_prompt = view.context.resolve(&prompt);
    let source_issue = view
        .context
        .get("issue.number")
        .and_then(|number| number.parse::<i64>().ok());
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let provider = provider_str
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| crate::services::autopilot::configured_autopilot_provider(&mesh));
    let prompt_delivery =
        crate::autopilot::launch::initial_prompt_delivery(&provider, &resolved_prompt);
    let worktree_policy = if source_issue.is_some()
        || view.context.get("source.review_preset") == Some("1")
    {
        WorktreePolicy::ForceBranched
    } else {
        WorktreePolicy::RespectMesh
    };
    let use_worktree_override = match worktree_policy {
        WorktreePolicy::ForceBranched => Some(true),
        WorktreePolicy::RespectMesh => None,
    };

    // Issue-triggered circuit runs share the legacy Autopilot trust boundary:
    // resolve the same harness/provider chain and reject an incompatible mesh
    // before a pending Agent Node row is created. Manual circuits remain a
    // general-purpose graph feature and are intentionally not subject to the
    // Autopilot compatibility gate.
    if source_issue.is_some() {
        let verdict = crate::autopilot::compatibility::compute_for_mesh(
            Some(provider.as_str()),
            mesh.default_provider.as_deref(),
            crate::preferences::default_provider().as_deref(),
            mesh.use_worktree,
        );
        if !verdict.allowed {
            return Err(format!(
                "Autopilot circuit cannot spawn on mesh {}: incompatible provider/worktree configuration ({:?})",
                mesh_id, verdict.reasons
            ));
        }
    }

    // If step already has an agent node attached (e.g. from an earlier loop iteration/retry)
    if let Some(existing_agent_id) = view.step(node_id).and_then(|s| s.agent_node_id) {
        if crate::agent::process::PROCESS_REGISTRY.is_alive(&existing_agent_id) {
            tracing::info!(
                "circuits: submitting new turn to live agent {} for step {} (run {})",
                existing_agent_id,
                node_id,
                run_id
            );
            let _ = db::update_agent_node_status(existing_agent_id, SessionStatus::Running);
            crate::autopilot::evaluator::note_turn_start(existing_agent_id);
            crate::autopilot::pipeline::write_prompt_to_pty(existing_agent_id, &resolved_prompt, app)
                .map_err(|e| format!("PTY write failed on retry: {}", e))?;
            return Ok(());
        }

        // Process is dead/exited: reuse its worktree path/branch to spawn a fresh process
        if let Ok(old_node) = db::get_agent_node_by_id(existing_agent_id) {
            tracing::info!(
                "circuits: respawning agent for step {} in existing worktree {}",
                node_id,
                old_node.path
            );
            let Some(spawn_permit) = begin_circuit_spawn(run_id)? else {
                return Ok(());
            };
            let new_node = crate::services::agent_node::create_pending_with_worktree_override(
                mesh_id,
                &old_node.path,
                &old_node.branch,
                Some(provider.as_str()),
                source_issue,
                name.as_deref(),
                use_worktree_override,
            )
            .map_err(|e| e.to_string())?;

            if let Err(error) = crate::agent::session_lifecycle::on_created(
                &crate::agent::session_lifecycle::AppSessionLifecycleSink { app },
                new_node.id,
            ) {
                let _ = crate::services::agent_node::delete(new_node.id, true);
                return Err(error.to_string());
            }

            if !db::set_circuit_step_agent_node(run_id, node_id, new_node.id)
                .map_err(|error| format!("could not attach new agent to step: {}", error))?
            {
                // Deletion can win the race after create_pending. If its
                // cascade removed the run, retire the unattached node instead
                // of leaking a process/worktree outside the circuit ledger.
                let _ = crate::services::agent_node::delete(new_node.id, true);
                return Err("could not attach new agent to step: step row no longer exists".to_string());
            }
            view.attach_agent_node(node_id, new_node.id);
            if !run_accepts_effects(run_id)? {
                view.step_mut(node_id).map(|step| step.agent_node_id = None);
                abort_circuit_spawn(run_id, new_node.id);
                return Ok(());
            }
            crate::autopilot::evaluator::register_circuit(new_node.id);
            crate::autopilot::evaluator::note_turn_start(new_node.id);
            let _ = app.emit(
                "node-created",
                crate::commands::agent::NodeCreatedPayload { id: new_node.id },
            );
            spawn_circuit_agent_in_background(
                app,
                run_id,
                new_node.id,
                spawn_permit,
                explicit,
                worktree_policy,
                resolved_prompt,
                prompt_delivery,
            );
            return Ok(());
        }
    }

    let branch = crate::commands::git::get_default_branch_blocking(mesh.path.clone())
        .unwrap_or_else(|_| "main".to_string());

    let Some(spawn_permit) = begin_circuit_spawn(run_id)? else {
        return Ok(());
    };
    let node = crate::services::agent_node::create_pending_with_worktree_override(
        mesh.id,
        &mesh.path,
        &branch,
        // Issue #1358: per-node provider override flows here.
        Some(provider.as_str()),
        source_issue,
        name.as_deref(),
        use_worktree_override,
    )
    .map_err(|e| e.to_string())?;

    if let Err(error) = crate::agent::session_lifecycle::on_created(
        &crate::agent::session_lifecycle::AppSessionLifecycleSink { app },
        node.id,
    ) {
        let _ = crate::services::agent_node::delete(node.id, true);
        return Err(error.to_string());
    }

    if !db::set_circuit_step_agent_node(run_id, node_id, node.id)
        .map_err(|error| format!("could not attach agent to step: {}", error))?
    {
        let _ = crate::services::agent_node::delete(node.id, true);
        return Err("could not attach agent to step: step row no longer exists".to_string());
    }
    view.attach_agent_node(node_id, node.id);
    if !run_accepts_effects(run_id)? {
        view.step_mut(node_id).map(|step| step.agent_node_id = None);
        abort_circuit_spawn(run_id, node.id);
        return Ok(());
    }

    // Track output times for this piloted node (the PTY submit watcher
    // and future classifiers read them).
    crate::autopilot::evaluator::register_circuit(node.id);
    crate::autopilot::evaluator::note_turn_start(node.id);

    let _ = app.emit(
        "node-created",
        crate::commands::agent::NodeCreatedPayload { id: node.id },
    );
    tracing::info!(
        "circuits: spawned agent node {} for run {} (step {})",
        node.id,
        run_id,
        node_id
    );

    // Stage-2 in the background — same two-stage contract as every
    // other spawn path. An empty prompt starts fresh; a non-empty prompt
    // uses prefill when supported and otherwise is injected after spawn.
    // Issue #1358: per-step model / effort / extra_args ride the explicit
    // layer through to `spawn_with_intent`, where capability masking occurs.
    spawn_circuit_agent_in_background(
        app,
        run_id,
        node.id,
        spawn_permit,
        explicit,
        worktree_policy,
        resolved_prompt,
        prompt_delivery,
    );

    Ok(())
}



// ---------------------------------------------------------------------------
// Startup reconciliation (milestone 3, issue #1208).
//
// Observation repairs most crash states by construction, but one wedge
// survives it: a Running spawn step whose agent attach never landed
// (process died between the stepper's commit and stage-1). Nothing in
// the world maps to an event for that step, so it would sit Running
// forever. The reconcile pass runs ONCE per launch — before the loop —
// and resolves every running run against live process + git state.
// ---------------------------------------------------------------------------

/// What startup reconciliation decides for one Running spawn step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpawnReconciliation {
    /// The step's world state is recoverable as-is (node exists, not
    /// archived, worktree intact) — resume machinery / observation
    /// carries on from here.
    Leave,
    /// The piloted agent is unrecoverable (row gone, archived, or its
    /// git worktree directory vanished) — cancel the step, fail the run.
    Lost,
    /// The commit-crash gap: a Running spawn step with no attached agent.
    /// There is nothing to observe or resume; fail the run loudly.
    NeverAttached,
}

/// The slice of agent-node state the pure decision needs. Built by the
/// impure wrapper so tests need no DB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReconcileNodeState {
    archived: bool,
    /// `Some(false)` = the node's worktree directory no longer exists on
    /// disk (git-state check); `None` = root-repo spawn (no worktree to
    /// lose) or path unreadable.
    worktree_dir_exists: Option<bool>,
}

/// Pure core of [`startup_reconcile_pass`]: classify one Running spawn
/// step observed at app launch.
fn reconcile_spawn_step(
    attached_agent: Option<i64>,
    node: Option<ReconcileNodeState>,
) -> SpawnReconciliation {
    let Some(_agent_node_id) = attached_agent else {
        return SpawnReconciliation::NeverAttached;
    };
    match node {
        None => SpawnReconciliation::Lost,
        Some(n) if n.archived => SpawnReconciliation::Lost,
        Some(n) => match n.worktree_dir_exists {
            Some(false) => SpawnReconciliation::Lost,
            _ => SpawnReconciliation::Leave,
        },
    }
}

/// One-shot per-launch sweep over `running` circuit runs. Maps the
/// spec's three verdicts (issue #1208) onto what observation leaves
/// behind: `Leave` = **resume** (the node row is intact and auto-resume
/// re-spawns it; observation then carries the run forward), `Lost` /
/// `NeverAttached` = **fail**. There is deliberately no in-place
/// "retry": re-running a half-attached spawn would double-spawn into a
/// worktree whose state we can't verify, so an unrecoverable step fails
/// loudly instead.
pub fn startup_reconcile_pass(app: &AppHandle) {
    let runs = match db::list_active_circuit_runs() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("circuits: startup reconcile could not list active runs: {}", e);
            return;
        }
    };
    for active in runs {
        if active.run.state != "running" {
            continue; // pending runs start via the normal trigger path
        }
        if !should_drive_circuit_run(active.circuit_enabled, &active.run.trigger_identity) {
            continue; // parked mid-flight; re-enabled circuits resume normally
        }
        let graph = match CircuitGraph::from_json(&active.circuit_graph_json) {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!("circuits: run {} unreadable graph_json: {}", active.run.id, e);
                continue;
            }
        };
        let steps = match load_steps(active.run.id) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("circuits: run {} step load failed: {}", active.run.id, e);
                continue;
            }
        };
        let context = match CircuitContext::from_json(&active.run.context_json) {
            Ok(context) => context,
            Err(_) => continue,
        };
        let view = RunView { run_id: active.run.id, graph, context, steps, state: RunState::from_db_str(&active.run.state) };
        for step in view.steps.iter().filter(|s| s.status == StepStatus::Running) {
            let Some(node) = view.graph.node(&step.node_id) else {
                continue;
            };
            match &node.kind {
                CircuitNodeKind::SpawnAgentNode { .. } => {
                    let node_state = step.agent_node_id.and_then(|id| {
                        db::get_agent_node_by_id(id).ok().map(|n| ReconcileNodeState {
                            archived: n.status == SessionStatus::Archived,
                            worktree_dir_exists: if n.use_worktree {
                                Some(std::path::Path::new(&n.path).exists())
                            } else {
                                None
                            },
                        })
                    });
                    match reconcile_spawn_step(step.agent_node_id, node_state) {
                        SpawnReconciliation::Leave => {}
                        SpawnReconciliation::Lost => {
                            let reason =
                                "piloted agent was lost while the app was offline".to_string();
                            tracing::warn!("circuits: run {}: {}", active.run.id, reason);
                            let _ = fail_run_step(
                                app,
                                &active.run.id,
                                &step.node_id,
                                "cancelled",
                                &reason,
                            );
                        }
                        SpawnReconciliation::NeverAttached => {
                            let reason = "the app shut down before the spawn attached an \
                                          agent node — restarting the step is unsafe, \
                                          failing the run"
                                .to_string();
                            tracing::warn!("circuits: run {}: {}", active.run.id, reason);
                            let _ = fail_run_step(
                                app,
                                &active.run.id,
                                &step.node_id,
                                "failed",
                                &reason,
                            );
                        }
                    }
                }
                // Orphan-detection for non-spawn steps whose target agent
                // vanished while the app was offline. The legacy pass
                // skipped these with a "Nothing to decide" comment — that
                // was wrong when the lineage agent is gone: auto-resume
                // has nothing to respawn, and the step stays `running`
                // holding a `circuit_run_capacity` slot until the user
                // cancels the run by hand.
                //
                // Step types without a target lineage (`Notify`, `Join`,
                // `RetryLimit`, `AnyCompleted`, classifier-only markers,
                // `GithubAction`) are NOT piloted — there is no agent to
                // lose, so the wildcard arm must skip them. The helper
                // returns `None` for those step kinds and we leave the
                // step alone.
                CircuitNodeKind::InjectPty { .. }
                | CircuitNodeKind::LlmTurnClassifier { .. }
                | CircuitNodeKind::AwaitAgentTurn { .. }
                | CircuitNodeKind::ReviewVerdict { .. }
                | CircuitNodeKind::SetNodeStatus { .. }
                | CircuitNodeKind::CloseAgentNode { .. } => {
                    let observed =
                        observed_agent_for_step(
                            step,
                            &view.graph,
                            &view.steps,
                            view.context.source_agent_id(),
                        )
                        .and_then(|id| {
                            // A present-but-archived lineage target is
                            // also lost from the run's perspective.
                            db::get_agent_node_by_id(id)
                                .ok()
                                .filter(|n| n.status != SessionStatus::Archived)
                                .map(|_| id)
                        });
                    if observed.is_none() {
                        let reason = format!(
                            "target agent lineage for step {} was lost while the app was offline",
                            step.node_id
                        );
                        tracing::warn!("circuits: run {}: {}", active.run.id, reason);
                        let _ = fail_run_step(
                            app,
                            &active.run.id,
                            &step.node_id,
                            "cancelled",
                            &reason,
                        );
                    }
                }
                _ => {
                    // Non-piloted step type (`Notify`, `Join`, `RetryLimit`,
                    // `AnyCompleted`, `GithubAction`, etc.) — nothing to
                    // orphan-detect. The legacy comment said "Nothing to
                    // decide"; that's still true here.
                }
            }
        }
    }
}

/// Persist a terminal write for one step + flip the run Failed, then
/// refresh the Probe tab. Shared by both reconcile verdicts.
fn fail_run_step(
    app: &AppHandle,
    run_id: &i64,
    node_id: &str,
    status: &str,
    error: &str,
) -> Result<(), String> {
    db::commit_circuit_advance(
        *run_id,
        Some(crate::autopilot::circuit::stepper::RunState::Failed.as_db_str()),
        None,
        &[db::CircuitStepOp {
            node_id: node_id.to_string(),
            status: status.to_string(),
            outcome: Some(Some(status.to_string())),
            error: Some(error.to_string()),
            agent_node_id: None,
            attempt: 1,
            fresh_attempt: false,
        }],
    )
    .map_err(|e| e.to_string())?;
    let _ = app.emit(
        "circuit-run-updated",
        CircuitRunUpdatedPayload { run_id: *run_id, state: "failed".to_string() },
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Lost-turn watchdog (milestone 3, issue #1208).
//
// The turn webhook (`/api/attention/{id}`) is best-effort HTTP — a lost
// POST leaves a FINISHED piloted agent looking `running` forever, and
// the run wedges with it (the legacy pipeline learned this as #874).
// Once the node's PTY has been quiet for [`LOST_TURN_QUIET_MS`] and its
// status still says `running`, synthesize the turn: mark it awaiting
// input through the normal lifecycle seam (which arms attention
// autoclear, so a false positive self-heals the moment output resumes)
// and let the next observation pass advance the run.
// ---------------------------------------------------------------------------

/// Quiet window before the watchdog synthesizes a missed turn. The spec
/// pins 60s — deliberately tighter than the legacy pipeline's 180s LLM-
/// classified watchdog, because autoclear makes false positives cheap.
pub(crate) const LOST_TURN_QUIET_MS: u128 = 60_000;

/// Pure eligibility predicate: alive process, quiet past the window,
/// status still claiming to be mid-turn.
fn should_synthesize_turn(is_alive: bool, quiet_ms: Option<u128>, status: SessionStatus) -> bool {
    is_alive
        && quiet_ms.is_some_and(|q| q >= LOST_TURN_QUIET_MS)
        && status == SessionStatus::Running
}

/// Fast-tick pass: recover every quiet piloted node bound to a running
/// circuit run. Cheap by construction — only steps already known to be
/// Running with an attached agent are evaluated.
fn lost_turn_watchdog_pass(app: &AppHandle) {
    let runs = match db::list_active_circuit_runs() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("circuits: watchdog could not list active runs: {}", e);
            return;
        }
    };
    for active in runs {
        if active.run.state != "running"
            || !should_drive_circuit_run(active.circuit_enabled, &active.run.trigger_identity)
        {
            continue;
        }
        let steps = match load_steps(active.run.id) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let (Ok(graph), Ok(context)) = (CircuitGraph::from_json(&active.circuit_graph_json), CircuitContext::from_json(&active.run.context_json)) else { continue; };
        let view = RunView { run_id: active.run.id, graph, context, steps, state: RunState::Running };
        let mut observed = HashSet::new();
        for step in view.steps.iter().filter(|s| s.status == StepStatus::Running) {
            let Some(agent_node_id) = observed_agent_for_step(
                step,
                &view.graph,
                &view.steps,
                view.context.source_agent_id(),
            ) else {
                continue;
            };
            if !observed.insert(agent_node_id) { continue; }
            let Ok(node) = db::get_agent_node_by_id(agent_node_id) else {
                continue;
            };
            let quiet_ms = crate::autopilot::evaluator::millis_since_last_output(agent_node_id);
            if !should_synthesize_turn(
                crate::agent::process::PROCESS_REGISTRY.is_alive(&agent_node_id),
                quiet_ms,
                node.status,
            ) {
                continue;
            }
            tracing::warn!(
                "circuits: run {} piloted agent {} quiet {}ms with status 'running' — \
                 synthesizing the turn webhook may have lost",
                active.run.id,
                agent_node_id,
                quiet_ms.unwrap_or(0)
            );
            // The normal mark-attention seam: lifecycle write + event +
            // autoclear arming, exactly as if the hook had landed.
            crate::commands::attention::mark_attention(agent_node_id, app);
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitEvents.ts")]
pub struct CircuitRunUpdatedPayload {
    #[ts(as = "i32")]
    pub run_id: i64,
    pub state: String,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitEvents.ts")]
pub struct CircuitNotificationPayload {
    #[ts(as = "i32")]
    pub run_id: i64,
    pub message: String,
    /// `success` for approval, `warning` for blocked/limit notices, and
    /// `info` for ordinary workflow updates.
    pub severity: String,
}

fn notification_severity(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("not approved")
        || lower.contains("not been approved")
        || lower.contains("rejected")
    {
        "warning".into()
    } else if lower.contains("approved for") {
        "success".into()
    } else if lower.contains("attention")
        || lower.contains("limit")
        || lower.contains("failed")
    {
        "warning".into()
    } else {
        "info".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autopilot::circuit::model::{CircuitNode, StepOutcome};

    #[test]
    fn circuit_completion_allows_its_terminal_actions_but_not_stale_effects_or_spawns() {
        use crate::autopilot::circuit::stepper::Effect;
        let notify = Effect::Notify { message: "approved".into() };
        let set_status = Effect::SetNodeStatus {
            node_id: "source-status".into(),
            status: "completed".into(),
            target_node_id: Some("$source".into()),
        };
        let close = Effect::CloseAgentNode { node_id: "close".into(), target_node_id: Some("reviewer".into()) };
        let spawn = Effect::SpawnAgentNode { node_id: "reviewer".into() };
        let inject = Effect::InjectPty { node_id: "feedback".into(), target_node_id: Some("$source".into()), prompt: "fix".into() };
        assert!(effect_allowed_in_state("completed", true, &notify));
        assert!(effect_allowed_in_state("completed", true, &set_status));
        assert!(effect_allowed_in_state("completed", true, &close));
        assert!(!effect_allowed_in_state("completed", true, &spawn));
        assert!(!effect_allowed_in_state("completed", true, &inject));
        for effect in [notify, set_status, close, spawn, inject] {
            assert!(!effect_allowed_in_state("completed", false, &effect));
            assert!(!effect_allowed_in_state("cancelled", true, &effect));
            assert!(!effect_allowed_in_state("failed", true, &effect));
        }
    }

    #[test]
    fn notification_severity_does_not_call_unapproved_findings_success() {
        assert_eq!(notification_severity("Review approved for Fix parser"), "success");
        assert_eq!(
            notification_severity("Latest fixes have not been approved; inspect the report"),
            "warning"
        );
        assert_eq!(notification_severity("Review not approved for Fix parser"), "warning");
        assert_eq!(notification_severity("Review needs attention"), "warning");
    }

    #[test]
    fn cancellation_marker_invalidates_batches_until_durable_ack() {
        let run_id = 9_876_543_210_i64;
        let permit = begin_circuit_effect_batch(run_id);
        assert!(!permit.is_cancelled());

        mark_circuit_run_cancelled(run_id);
        assert!(permit.is_cancelled());

        // A batch admitted while the command is waiting must inherit the
        // already-cancelled token rather than starting fresh work.
        let blocked = begin_circuit_effect_batch(run_id);
        assert!(blocked.is_cancelled());
        drop(blocked);

        // The marker is retained while the original batch is in flight and
        // only becomes removable after the durable cancellation is acknowledged.
        finish_circuit_run_cancellation(run_id);
        drop(permit);

        let fresh = begin_circuit_effect_batch(run_id);
        assert!(!fresh.is_cancelled());
        drop(fresh);
    }

    #[test]
    fn circuit_old_close_cannot_retire_next_review_round() {
        let view = RunView {
            run_id: 1, graph: CircuitGraph::agent_review("claude", None, None, 3), state: RunState::Running,
            context: CircuitContext::new(), steps: vec![
                StepView { node_id: "reviewer".into(), agent_node_id: Some(101), attempt: 2,
                    status: StepStatus::Running, outcome: None, error: None },
                StepView { node_id: "close_reviewer".into(), agent_node_id: None, attempt: 1,
                    status: StepStatus::Completed, outcome: Some(StepOutcome::Completed), error: None },
            ],
        };
        let mut events = vec![];
        observe_close_agent_retries(&view, &mut events);
        assert!(events.is_empty());
    }

    // -- draft-first drive gate (issue #1356) ----------------------------------

    #[test]
    fn disabled_circuits_still_drive_manual_trigger_now_runs() {
        assert!(should_drive_circuit_run(true, "interval:1"));
        assert!(should_drive_circuit_run(true, "manual:1"));
        assert!(
            should_drive_circuit_run(false, "manual:1724000000000"),
            "Trigger Now is the dry-run seam on a draft circuit"
        );
        assert!(
            !should_drive_circuit_run(false, "interval:1"),
            "background interval runs stay parked while disabled"
        );
        assert!(!should_drive_circuit_run(false, "issue:42:buildmesh:run"));
    }

    // ------------------------------------------------------------------
    // Circuit-run admission gate (issue #1467).
    //
    // These tests pin the pure gate helper [`may_admit_run`] in three
    // shapes — empty mesh / under-cap mesh / full mesh — using the
    // process-global DB with the same `--test-threads=1` discipline as
    // the rest of `db::circuit_tests`. The DB-layer contracts
    // (`count_active_circuit_runs` / `release_circuit_run`) are pinned
    // separately in `db/circuit_tests.rs`; here we verify the gate
    // composes correctly with state transitions on real rows.
    // ------------------------------------------------------------------

    /// `running` and `paused` runs always pass the gate — they already
    /// hold a slot from their `pending` admission.
    #[test]
    fn may_admit_run_running_and_paused_unconditional_pass() {
        let mesh = crate::models::Mesh {
            id: 9_999_001,
            circuit_run_capacity: 1,
            ..zero_test_mesh()
        };
        let running = active_row_with_state(9_999_001, 9_999_010, "running");
        assert!(may_admit_run(&running, &mesh));
        let paused = active_row_with_state(9_999_001, 9_999_011, "paused");
        assert!(may_admit_run(&paused, &mesh));
    }

    /// The defer path: a `pending` run on a mesh whose admitted-run
    /// count equals the configured `circuit_run_capacity` returns
    /// `false`. Initialized against a real temp DB so the test
    /// exercises the production read path through
    /// `db::count_active_circuit_runs` (no shadow helpers).
    #[test]
    fn may_admit_run_pending_saturated_mesh_defers() {
        let path = init_temp_db_at("may_admit_defer");
        let mesh = crate::db::create_mesh("may-admit-defer", "/tmp/may-admit-defer").unwrap();
        // Two admitted runs saturate the expected review-flow capacity.
        crate::db::set_mesh_circuit_run_capacity(mesh.id, 2).unwrap();
        let c1 = crate::db::create_autopilot_circuit(mesh.id, "c1", "", 4, "{}").unwrap();
        let c2 = crate::db::create_autopilot_circuit(mesh.id, "c2", "", 4, "{}").unwrap();
        let c3 = crate::db::create_autopilot_circuit(mesh.id, "c3", "", 4, "{}").unwrap();

        let mesh_row = crate::db::get_mesh_by_id(mesh.id).unwrap();

        // Mesh starts below cap and admits the first run.
        let pending_run = crate::db::create_circuit_run(c1.id, mesh.id, "", "{}").unwrap();
        let pending_row = crate::db::list_active_circuit_runs().unwrap().into_iter()
            .find(|r| r.run.id == pending_run)
            .expect("pending run should be in list_active_circuit_runs");
        assert!(
            may_admit_run(&pending_row, &mesh_row),
            "below cap — must admit",
        );

        // Admit the second run, then verify the third stays pending.
        crate::db::set_circuit_run_state(pending_run, "running").unwrap();
        let pending_run_2 = crate::db::create_circuit_run(c2.id, mesh.id, "", "{}").unwrap();
        let pending_row_2 = crate::db::list_active_circuit_runs().unwrap().into_iter()
            .find(|r| r.run.id == pending_run_2)
            .expect("second pending run must be in the active list");

        assert!(
            may_admit_run(&pending_row_2, &mesh_row),
            "the second run must admit below cap 2"
        );
        crate::db::set_circuit_run_state(pending_run_2, "running").unwrap();
        let pending_run_3 = crate::db::create_circuit_run(c3.id, mesh.id, "", "{}").unwrap();
        let pending_row_3 = crate::db::list_active_circuit_runs().unwrap().into_iter()
            .find(|r| r.run.id == pending_run_3)
            .expect("third pending run must be in the active list");

        assert_eq!(
            crate::db::count_active_circuit_runs(mesh.id).unwrap(),
            2,
            "two running runs consume both run-admission slots"
        );
        assert_eq!(
            mesh_row.circuit_run_capacity, 2,
            "sanity: cap is 2, so two admitted runs fill it"
        );
        assert!(
            !may_admit_run(&pending_row_3, &mesh_row),
            "at cap — must defer to next pass (FIFO)",
        );

        // Terminal the running run via `commit_circuit_advance`'s
        // idempotent terminal-state branch — the third pending run
        // now admits on the next observation pass.
        crate::db::commit_circuit_advance(pending_run, Some("completed"), None, &[]).unwrap();
        assert_eq!(
            crate::db::count_active_circuit_runs(mesh.id).unwrap(),
            1,
            "terminal commit frees one admitted-run slot"
        );
        assert!(
            may_admit_run(&pending_row_3, &mesh_row),
            "after terminal — third pending must admit (FIFO promotion)",
        );

        std::fs::remove_file(&path).ok();
    }

    /// Temp-dir DB init, used by the run-admission integration tests
    /// in this module. Mirrors the pattern in `db::circuit_tests`
    /// (process-global DB, `--test-threads=1`).
    fn init_temp_db_at(tag: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "buildmesh_circuit_worker_test_{}_{}.db",
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        crate::db::init(&path).unwrap();
        path
    }

    /// The structural pin: `may_admit_run` short-circuits for `running`
    /// and `paused` without touching the DB, so a no-DB-init unit test
    /// can still verify the helper's contract for those branches.
    /// (The `pending` branch reads the count; the integration path
    /// through `run_pass` is what init's the DB, tested separately by
    /// the `db::circuit_tests::count_active_circuit_runs_*` suite.)
    #[test]
    fn may_admit_run_signature_compiles_for_running_state() {
        let run = active_row_with_state(1, 2, "running");
        let mesh = zero_test_mesh();
        assert!(may_admit_run(&run, &mesh));
    }

    #[test]
    fn agent_reservation_keeps_the_host_cap_hard_for_downstream_spawns() {
        let mesh = crate::models::Mesh {
            autopilot_concurrency_limit: 2,
            ..zero_test_mesh()
        };

        // A two-agent blueprint can claim both mesh slots when no other
        // process or run lease is using them.
        assert!(agent_reservation_fits(&mesh, 2, 0, 0, 0, 0, Some(2)));
        // A peer run's reservation consumes capacity even before its second
        // process has been created, so retained agents cannot fan out past
        // the configured host limit.
        assert!(!agent_reservation_fits(&mesh, 2, 1, 0, 0, 0, Some(2)));
        assert!(!agent_reservation_fits(&mesh, 1, 0, 1, 0, 0, Some(1)));
        // Legacy (non-circuit) agents remain part of both host and global
        // accounting while leases cover circuit agents.
        assert!(!agent_reservation_fits(&mesh, 1, 0, 0, 2, 0, Some(2)));
    }

    /// Test helper: an `ActiveCircuitRun` with only `mesh_id`, `id`,
    /// and `state` populated — `may_admit_run` reads only those three
    /// fields on the `pending`-vs-other branch, so the rest can stay
    /// empty for the structural pin above.
    fn active_row_with_state(mesh_id: i64, run_id: i64, state: &'static str) -> db::ActiveCircuitRun {
        db::ActiveCircuitRun {
            run: crate::models::AutopilotCircuitRun {
                id: run_id,
                circuit_id: 1,
                mesh_id,
                source_agent_node_id: None,
                trigger_identity: String::new(),
                state: state.to_string(),
                context_json: "{}".to_string(),
                created_at: String::new(),
                updated_at: String::new(),
            },
            circuit_enabled: true,
            circuit_concurrency_limit: 1,
            circuit_graph_json: "{}".to_string(),
            circuit_name: String::new(),
        }
    }

    /// Test helper: a `Mesh` with empty placeholders for every field
    /// except the one the test exercises. `may_admit_run` reads
    /// `mesh_id` for log output and `circuit_run_capacity` for the
    /// (caller-resolved) cap — the helper itself only receives the
    /// mesh as a reference for the cache invalidation contract and the
    /// future extension to read more fields.
    fn zero_test_mesh() -> crate::models::Mesh {
        crate::models::Mesh {
            id: 0,
            name: String::new(),
            path: String::new(),
            layout: "grid".into(),
            position: 0,
            created_at: chrono::Utc::now(),
            build_command: None,
            run_command: None,
            model: None,
            effort: None,
            use_worktree: true,
            worktree_mode: None,
            default_provider: None,
            base_ref: "origin/main".into(),
            scratchpad: String::new(),
            sandbox: false,
            pre_spawn_pool_size: 0,
            color: None,
            autopilot_enabled: false,
            autopilot_trigger_label: None,
            autopilot_concurrency_limit: 2,
            autopilot_provider: None,
            autopilot_action_on_success: None,
            root_build_command: None,
            root_run_command: None,
            autopilot_mode: crate::models::AutopilotMode::IssueDriven,
            loop_initial_prompt: None,
            loop_suffix_prompt: None,
            loop_max_iterations: None,
            loop_interval_seconds: 0,
            loop_consecutive_failures: 0,
            harness_overrides: std::collections::HashMap::new(),
            circuit_run_capacity: 2,
            worktree_directory: None,
        }
    }

    #[test]
    fn close_agent_retry_is_not_observed_after_close_clears_spawn_association() {
        let mut view = RunView {
            run_id: 42,
            graph: CircuitGraph::issue_driven_autopilot_review("buildmesh:run"),
            state: RunState::Running,
            context: CircuitContext::new(),
            steps: vec![
                StepView {
                    node_id: "reviewer".into(),
                    status: StepStatus::Completed,
                    outcome: Some(GraphStepOutcome::Completed),
                    error: None,
                    agent_node_id: Some(701),
                    attempt: 1,
                },
                StepView {
                    node_id: "close_reviewer".into(),
                    status: StepStatus::Completed,
                    outcome: Some(GraphStepOutcome::Completed),
                    error: None,
                    agent_node_id: None,
                    attempt: 1,
                },
            ],
        };
        let mut events = Vec::new();

        observe_close_agent_retries(&view, &mut events);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            CircuitEvent::CloseAgentRetry { node_id } if node_id == "close_reviewer"
        ));

        // This is the in-memory half of the real CloseAgentNode effect. The
        // database half clears the same `reviewer` spawn step below it.
        let spawn_step_id = close_target_spawn_step_id(
            &view,
            "close_reviewer",
            Some("reviewer"),
            701,
        );
        assert_eq!(spawn_step_id, "reviewer");
        view.step_mut(&spawn_step_id).unwrap().agent_node_id = None;

        events.clear();
        observe_close_agent_retries(&view, &mut events);
        assert!(
            events.is_empty(),
            "a completed close must not be replayed after its target association is cleared"
        );
    }

    // -- startup reconciliation -------------------------------------------------

    fn node_state(archived: bool, worktree_dir_exists: Option<bool>) -> ReconcileNodeState {
        ReconcileNodeState { archived, worktree_dir_exists }
    }

    #[test]
    fn a_running_spawn_step_without_an_attached_agent_is_the_commit_crash_gap() {
        // The one state observation can never repair: nothing in the
        // world maps to an event, so it must fail loudly at startup.
        assert_eq!(
            reconcile_spawn_step(None, None),
            SpawnReconciliation::NeverAttached
        );
        assert_eq!(
            reconcile_spawn_step(None, Some(node_state(false, Some(true)))),
            SpawnReconciliation::NeverAttached,
            "even a healthy-looking node row doesn't help — no attach ever landed"
        );
    }

    #[test]
    fn a_missing_or_archived_piloted_agent_is_lost() {
        assert_eq!(
            reconcile_spawn_step(Some(7), None),
            SpawnReconciliation::Lost,
            "node row deleted while offline"
        );
        assert_eq!(
            reconcile_spawn_step(Some(7), Some(node_state(true, Some(true)))),
            SpawnReconciliation::Lost,
            "archived while offline"
        );
    }

    #[test]
    fn a_vanished_worktree_directory_counts_as_lost_git_state() {
        assert_eq!(
            reconcile_spawn_step(Some(7), Some(node_state(false, Some(false)))),
            SpawnReconciliation::Lost,
            "resume would spawn into a nonexistent worktree"
        );
    }

    #[test]
    fn recoverable_states_leave_the_step_alone() {
        assert_eq!(
            reconcile_spawn_step(Some(7), Some(node_state(false, Some(true)))),
            SpawnReconciliation::Leave,
            "intact worktree → auto-resume carries on"
        );
        assert_eq!(
            reconcile_spawn_step(Some(7), Some(node_state(false, None))),
            SpawnReconciliation::Leave,
            "root-repo spawn has no worktree to lose"
        );
        assert_eq!(
            reconcile_spawn_step(Some(7), Some(node_state(false, None))),
            SpawnReconciliation::Leave
        );
    }

    // -- orphan-detection regression (mesh 65 / runs 3, 5, 6) -----------------
    //
    // The legacy pass skipped non-SpawnAgentNode steps with a "Nothing to
    // decide" comment. Live incident 2026-09-02 showed the assumption was
    // wrong: when a piloted agent row is deleted while the run is in
    // flight, an `InjectPty` / `LlmTurnClassifier` / `CloseAgentNode` /
    // `SetNodeStatus` step that waits on it stays `running` forever,
    // holding a `circuit_run_capacity` slot. The tests below pin the fix
    // on both observation paths (per-tick `observe` and the one-shot
    // `startup_reconcile_pass`).

    /// Build a `RunView` whose `implementer` spawn step has the given
    /// `agent_node_id` (None = unattached, mirroring a deleted row) and
    /// whose `follow_feedback` step is `Running` with NULL `agent_node_id`
    /// (the canonical shape of an orphaned inject step on the review
    /// blueprint). Used by the orphan-detection regression tests below.
    fn review_blueprint_view_with_orphan_inject(orphan_target_id: Option<i64>) -> RunView {
        RunView {
            run_id: 7,
            graph: CircuitGraph::issue_driven_autopilot_review("buildmesh:run"),
            state: RunState::Running,
            context: CircuitContext::new(),
            steps: vec![
                StepView {
                    node_id: "trigger".into(),
                    status: StepStatus::Completed,
                    outcome: Some(GraphStepOutcome::Completed),
                    error: None,
                    agent_node_id: None,
                    attempt: 1,
                },
                StepView {
                    node_id: "implementer".into(),
                    status: StepStatus::Completed,
                    outcome: Some(GraphStepOutcome::Completed),
                    error: None,
                    agent_node_id: orphan_target_id,
                    attempt: 1,
                },
                StepView {
                    node_id: "follow_feedback".into(),
                    status: StepStatus::Running,
                    outcome: None,
                    error: None,
                    agent_node_id: None,
                    attempt: 1,
                },
            ],
        }
    }

    #[test]
    fn observed_agent_for_step_resolves_lineage_for_running_inject_step() {
        let view = review_blueprint_view_with_orphan_inject(Some(42));
        let follow = view
            .step("follow_feedback")
            .expect("fixture includes follow_feedback");
        assert_eq!(
            observed_agent_for_step(follow, &view.graph, &view.steps, view.context.source_agent_id()),
            Some(42),
            "InjectPty lineage walks back to implementer's agent_node_id"
        );
    }

    #[test]
    fn observed_agent_for_step_returns_none_when_lineage_spawn_has_no_agent() {
        let view = review_blueprint_view_with_orphan_inject(None);
        let follow = view.step("follow_feedback").unwrap();
        assert_eq!(
            observed_agent_for_step(follow, &view.graph, &view.steps, view.context.source_agent_id()),
            None,
            "no spawn attachment → no observed agent"
        );
    }

    #[test]
    fn observed_agent_for_step_resolves_source_binding_from_run_context() {
        let graph = CircuitGraph {
            version: 1,
            blueprint: None,
            nodes: vec![CircuitNode {
                id: "inject".into(),
                kind: CircuitNodeKind::InjectPty {
                    prompt: "follow-up".into(),
                    target_node_id: Some("$source".into()),
                },
            }],
            edges: vec![],
        };
        let step = StepView {
            node_id: "inject".into(),
            status: StepStatus::Running,
            outcome: None,
            error: None,
            agent_node_id: None,
            attempt: 1,
        };
        let mut context = CircuitContext::new();
        context.set("source.agent_id", "77");
        assert_eq!(
            observed_agent_for_step(&step, &graph, &[step.clone()], context.source_agent_id()),
            Some(77),
            "borrowed source bindings must remain visible to lifecycle observation"
        );
    }

    #[test]
    fn observed_agent_for_step_prefers_direct_id_when_present() {
        let graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        let step = StepView {
            node_id: "reviewer".into(),
            status: StepStatus::Running,
            outcome: None,
            error: None,
            agent_node_id: Some(99),
            attempt: 1,
        };
        // Empty steps slice is fine here — the early-return on direct id
        // short-circuits before any lineage walk.
        assert_eq!(observed_agent_for_step(&step, &graph, &[], None), Some(99));
    }

    #[test]
    fn observed_agent_for_step_returns_none_for_non_piloted_steps() {
        let graph = CircuitGraph::walking_skeleton("{{issue.prefill}}");
        graph.validate().unwrap();
        let step = StepView {
            node_id: graph.nodes[0].id.clone(),
            status: StepStatus::Running,
            outcome: None,
            error: None,
            agent_node_id: None,
            attempt: 1,
        };
        assert_eq!(observed_agent_for_step(&step, &graph, &[], None), None);
    }

    #[test]
    fn observed_agent_for_step_ignores_open_pr_worktree_observation() {
        let view = open_pr_run();
        let open_pr = view.step("open_pr").unwrap();
        assert_eq!(
            observed_agent_for_step(open_pr, &view.graph, &view.steps, view.context.source_agent_id()),
            None,
            "OpenPr inspects a worktree but never observes or pilots the agent process"
        );
    }

    #[test]
    fn observed_agent_for_step_walks_lineage_when_target_node_id_is_none() {
        // Regression for review feedback — the helper previously short-
        // circuited on `target_node_id = None` because the `?` on the
        // `Option<&str>` match arms extracted the inner `&str` instead of
        // passing the option through. The default AST representation for
        // any step relying on upstream BFS lineage uses `None`, so this
        // case is the COMMON one — every non-explicit target resolves via
        // BFS.
        use crate::autopilot::circuit::model::{
            CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind,
        };
        let graph = CircuitGraph {
            version: 1,
            blueprint: None,
            nodes: vec![
                CircuitNode { id: "trigger".into(), kind: CircuitNodeKind::Manual },
                CircuitNode {
                    id: "spawn".into(),
                    kind: CircuitNodeKind::SpawnAgentNode {
                        prompt: "fix it".into(),
                        name: None,
                        provider: None,
                        model: None,
                        effort: None,
                        extra_args: None,
                    },
                },
                CircuitNode {
                    id: "inject".into(),
                    kind: CircuitNodeKind::InjectPty {
                        prompt: "follow-up".into(),
                        target_node_id: None,
                    },
                },
            ],
            edges: vec![
                CircuitEdge {
                    from: "trigger".into(),
                    to: "spawn".into(),
                    condition: Default::default(),
                },
                CircuitEdge {
                    from: "spawn".into(),
                    to: "inject".into(),
                    condition: Default::default(),
                },
            ],
        };
        let steps = vec![
            StepView {
                node_id: "trigger".into(),
                status: StepStatus::Completed,
                outcome: Some(GraphStepOutcome::Completed),
                error: None,
                agent_node_id: None,
                attempt: 1,
            },
            StepView {
                node_id: "spawn".into(),
                status: StepStatus::Completed,
                outcome: Some(GraphStepOutcome::Completed),
                error: None,
                agent_node_id: Some(42),
                attempt: 1,
            },
            StepView {
                node_id: "inject".into(),
                status: StepStatus::Running,
                outcome: None,
                error: None,
                agent_node_id: None,
                attempt: 1,
            },
        ];
        let inject = steps.iter().find(|s| s.node_id == "inject").unwrap();
        assert_eq!(
            observed_agent_for_step(inject, &graph, &steps, None),
            Some(42),
            "InjectPty with target_node_id=None must walk upstream BFS, not short-circuit"
        );
    }

    #[test]
    fn observed_agent_for_step_returns_none_for_healthy_step_with_no_lineage() {
        // Defensive: a Running step whose kind has no lineage arm AND
        // whose own `agent_node_id` is None (e.g. a `Notify` step)
        // must return None — startup_reconcile's `_` arm then leaves it
        // alone rather than falsely cancelling.
        use crate::autopilot::circuit::model::{
            CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind,
        };
        let graph = CircuitGraph {
            version: 1,
            blueprint: None,
            nodes: vec![
                CircuitNode { id: "trigger".into(), kind: CircuitNodeKind::Manual },
                CircuitNode {
                    id: "notify".into(),
                    kind: CircuitNodeKind::Notify { message: "done".into() },
                },
            ],
            edges: vec![CircuitEdge {
                from: "trigger".into(),
                to: "notify".into(),
                condition: Default::default(),
            }],
        };
        let steps = vec![StepView {
            node_id: "notify".into(),
            status: StepStatus::Running,
            outcome: None,
            error: None,
            agent_node_id: None,
            attempt: 1,
        }];
        let notify = steps.first().unwrap();
        assert_eq!(
            observed_agent_for_step(notify, &graph, &steps, None),
            None,
            "Notify has no lineage arm → no agent to check → helper returns None"
        );
    }

    // -- lost-turn watchdog eligibility -------------------------------------------

    #[test]
    fn watchdog_needs_alive_quiet_and_still_running() {
        let running = SessionStatus::Running;
        assert!(should_synthesize_turn(true, Some(LOST_TURN_QUIET_MS), running));
        // Below the window: the agent may legitimately be mid-tool-call.
        assert!(!should_synthesize_turn(
            true,
            Some(LOST_TURN_QUIET_MS - 1),
            running
        ));
        // No output timing known (never registered): conservative no-op.
        assert!(!should_synthesize_turn(true, None, running));
        // Dead process: AgentLost observation owns that path.
        assert!(!should_synthesize_turn(false, Some(LOST_TURN_QUIET_MS * 10), running));
        // Already yielded (awaiting/completed/idle...): observation owns it.
        for status in [
            SessionStatus::AwaitingInput,
            SessionStatus::Completed,
            SessionStatus::Error,
        ] {
            assert!(!should_synthesize_turn(true, Some(LOST_TURN_QUIET_MS), status));
        }
    }

    // -- approvals sweep (issue #1263) -----------------------------------------

    /// Build a minimal `ActiveCircuitRun` fixture with only `run.id`
    /// populated — the sweep only reads `r.run.id`, every other field is
    /// a placeholder. Avoids sprawling struct literals across the three
    /// tests below.
    fn active_run(id: i64) -> db::ActiveCircuitRun {
        db::ActiveCircuitRun {
            run: crate::models::AutopilotCircuitRun {
                id,
                circuit_id: 0,
                mesh_id: 0,
                source_agent_node_id: None,
                trigger_identity: String::new(),
                state: String::new(),
                context_json: String::new(),
                created_at: String::new(),
                updated_at: String::new(),
            },
            circuit_enabled: true,
            circuit_concurrency_limit: 0,
            circuit_graph_json: String::new(),
            circuit_name: String::new(),
        }
    }

    // -- Sweep test isolation ----------------------------------------------
    //
    // `APPROVALS` is a process-global `Lazy<Mutex<Vec<...>>>` shared by
    // every parallel `cargo test` worker. Tests cannot rely on the queue
    // being empty or on `clear_approvals_queue()` for isolation — another
    // test running concurrently may mutate it between our ops. The fix:
    // unique run-id namespaces per test, asserts only about OUR entries,
    // and a per-test tidying pass so our entries don't leak.
    // This mirrors the PLANNER_TEST_MESH constant pattern used by
    // services::autopilot::tests for the same reason.

    #[test]
    fn approvals_sweep_drops_entries_for_vanished_runs() {
        const RUN_A: i64 = 910_001;
        const RUN_B: i64 = 910_002;
        const RUN_C: i64 = 910_003;
        request_circuit_approval(RUN_A, "node-a".into());
        request_circuit_approval(RUN_B, "node-b".into());
        request_circuit_approval(RUN_C, "node-c".into());

        // RUN_B vanished (deleted/completed) between the click and this
        // pass. The sweep must drop ONLY its entry; RUN_A and RUN_C
        // survive. Use a 910_xxx namespace so other parallel tests' ops
        // don't touch our entries (and ours don't touch theirs).
        let active = vec![active_run(RUN_A), active_run(RUN_C)];
        sweep_stale_approvals(&active);

        // Scope the lock so it drops before the trailing tidy (the mutex
        // is not reentrant — holding the guard across another lock would
        // deadlock).
        let queue = lock_circuit_worker_static(&APPROVALS);
        assert!(
            !queue.iter().any(|(r, _)| *r == RUN_B),
            "the vanished run's approval must be evicted"
        );
        assert!(
            queue.iter().any(|(r, _)| *r == RUN_A),
            "live run A's approval must survive the sweep"
        );
        assert!(
            queue.iter().any(|(r, _)| *r == RUN_C),
            "live run C's approval must survive the sweep"
        );
        // Tidy our test's entries so they don't leak into other tests.
        drop(queue);
        lock_circuit_worker_static(&APPROVALS)
            .retain(|(r, _)| *r != RUN_A && *r != RUN_C);
    }

    // ---- Poison-recovery regression (issue #1224) ----
    //
    // Both worker statics used to be locked with `.unwrap()` — a single
    // panic while holding either guard permanently poisoned the mutex and
    // every subsequent call re-panicked. The recovery helper
    // `lock_circuit_worker_static` calls `into_inner()` instead so the
    // circuit poller keeps waking and draining approvals after any
    // one-off failure. These tests are the regression pin: poison each
    // static inside `catch_unwind`, then re-lock and prove normal
    // push/drain/wake still works.
    fn poison_circuit_worker_static<T>(mutex: &'static Mutex<T>) {
        let _guard = mutex.lock().expect("first lock must succeed (test setup)");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("intentional circuit-worker poison for issue #1224 regression test");
        }));
        assert!(result.is_err(), "test fixture must panic to poison the mutex");
    }

    #[test]
    fn approvals_recovers_from_poison() {
        poison_circuit_worker_static(&APPROVALS);
        // Production path: `request_circuit_approval` is the entrypoint
        // the IPC layer calls when a user clicks Approve. Walk through
        // it so the regression test mirrors real traffic.
        let run_id = i64::MAX - 401;
        let node_id = "issue-1224-poison-node".to_string();
        request_circuit_approval(run_id, node_id.clone());
        // Drain must find the entry that was pushed after the panic —
        // if the recovery shape regressed, the Vec would be empty.
        let drained = drain_approvals_for(run_id);
        assert_eq!(
            drained,
            vec![node_id],
            "approval pushed after mutex poison must survive into the drain (issue #1224)"
        );
        // Re-drain proves the queue is empty, not just hidden by the
        // recovery.
        assert!(
            drain_approvals_for(run_id).is_empty(),
            "drain must be idempotent after recovery"
        );
    }

    #[test]
    fn wake_condvar_recovers_from_poison() {
        // WAKE holds a `Mutex<()>` paired with a `Condvar`. The lock
        // itself guards nothing — only the wait/notify handshake —
        // so poisoning it freezes the worker thread on the next
        // `wait_timeout`. The recovery shape is the same as for
        // APPROVALS; this test pins that path.
        let (lock, _cvar) = &*WAKE;
        poison_circuit_worker_static(lock);
        // After the panic, the OLD `.lock().unwrap()` form would now
        // return `Err(Poisoned)`. The recovery helper must hand back
        // a usable guard that can be passed to `wait_timeout` (the
        // real production call site — issue #1207 milestone 2).
        let guard = lock_circuit_worker_static(lock);
        // We deliberately do NOT call `wait_timeout` here — that would
        // block the test on the real condvar. Proving the helper
        // returns a guard is enough to assert the recovery path is
        // live; the `circuit_worker_smoke` integration test exercises
        // the full wait/notify handshake end-to-end.
        drop(guard);
    }

    // -- blueprint contract: per-blueprint worker seam (#1469) -----------
    //
    // The blueprint contract matrix (`autopilot::circuit::blueprint_contract`)
    // pins the walking skeleton as the canonical minimal preset. The
    // worker-seam helpers below are the impure-side equivalents: the
    // seam observes per-blueprint state and turns it into pure events.
    // These tests pin that the seam treats the walking skeleton
    // correctly — no gates/retries/closes to mishandle — and that the
    // review blueprint's close_retry observer stops firing once its
    // target's association clears (the otherwise-loop-forever trap).

    #[test]
    fn walking_skeleton_close_retry_observer_emits_nothing() {
        // The walking skeleton has no CloseAgentNode —
        // `observe_close_agent_retries` must scan all steps and find
        // zero Completed close steps. Pins the negative contract: a
        // refactor that accidentally treats every Completed step as a
        // close retry would otherwise emit spurious retries for the
        // walking skeleton.
        let mut view = RunView {
            run_id: 42,
            graph: CircuitGraph::walking_skeleton("do the thing"),
            state: RunState::Running,
            context: CircuitContext::new(),
            steps: vec![
                StepView {
                    node_id: "spawn".into(),
                    status: StepStatus::Completed,
                    outcome: Some(GraphStepOutcome::Completed),
                    error: None,
                    agent_node_id: Some(900),
                    attempt: 1,
                },
                StepView {
                    node_id: "inject".into(),
                    status: StepStatus::Completed,
                    outcome: Some(GraphStepOutcome::Completed),
                    error: None,
                    agent_node_id: None,
                    attempt: 1,
                },
                StepView {
                    node_id: "notify".into(),
                    status: StepStatus::Completed,
                    outcome: Some(GraphStepOutcome::Completed),
                    error: None,
                    agent_node_id: None,
                    attempt: 1,
                },
            ],
        };
        let mut events = Vec::new();
        observe_close_agent_retries(&view, &mut events);
        assert!(
            events.is_empty(),
            "walking skeleton has no CloseAgentNode; observer must emit nothing: {events:?}"
        );

        // Sanity: even a stray CloseAgentNode with no resolvable target
        // agent stays silent. The observer requires `resolve_target_agent`
        // to be Some, and the walking skeleton's spawn has no agent
        // lineage for a stray close to target.
        view.steps.push(StepView {
            node_id: "ghost_close".into(),
            status: StepStatus::Completed,
            outcome: Some(GraphStepOutcome::Completed),
            error: None,
            agent_node_id: None,
            attempt: 1,
        });
        observe_close_agent_retries(&view, &mut events);
        assert!(
            events.is_empty(),
            "a close with no resolvable target agent must NOT emit a retry"
        );
    }

    #[test]
    fn walking_skeleton_spawn_recovery_treats_missing_agent_as_never_attached() {
        // The contract pins walking skeleton's only spawned node as a
        // single-slot spawn. On startup recovery, an unattached Running
        // spawn step is the commit-crash gap — the worker seam fails
        // it loudly (the only state observation nothing can repair).
        assert_eq!(
            reconcile_spawn_step(None, None),
            SpawnReconciliation::NeverAttached
        );
        // Even if the worktree is healthy-looking, an unattached spawn
        // step is still NeverAttached — there is no row to resume into.
        assert_eq!(
            reconcile_spawn_step(
                None,
                Some(ReconcileNodeState {
                    archived: false,
                    worktree_dir_exists: Some(true),
                })
            ),
            SpawnReconciliation::NeverAttached
        );
    }

    #[test]
    fn review_blueprint_close_retry_observer_stops_after_target_clears() {
        // Companion to `walking_skeleton_close_retry_observer_emits_nothing`:
        // the review blueprint DOES have a `close_reviewer` step, and the
        // contract pins that the observer only re-emits the retry while
        // the spawn's agent_node_id is still attached. After the seam's
        // DB half clears the association, the observer MUST stop firing
        // (otherwise it would loop forever closing an already-closed
        // node).
        let mut view = RunView {
            run_id: 42,
            graph: CircuitGraph::issue_driven_autopilot_review("buildmesh:run"),
            state: RunState::Running,
            context: CircuitContext::new(),
            steps: vec![
                StepView {
                    node_id: "reviewer".into(),
                    status: StepStatus::Completed,
                    outcome: Some(GraphStepOutcome::Completed),
                    error: None,
                    agent_node_id: Some(701),
                    attempt: 1,
                },
                StepView {
                    node_id: "close_reviewer".into(),
                    status: StepStatus::Completed,
                    outcome: Some(GraphStepOutcome::Completed),
                    error: None,
                    agent_node_id: None,
                    attempt: 1,
                },
            ],
        };
        let mut events = Vec::new();
        observe_close_agent_retries(&view, &mut events);
        assert_eq!(
            events.len(),
            1,
            "review blueprint's close_reviewer MUST emit one retry while its target agent is attached"
        );
        assert!(
            matches!(&events[0], CircuitEvent::CloseAgentRetry { node_id } if node_id == "close_reviewer"),
            "emitted retry must be for close_reviewer: got {:?}",
            events[0]
        );

        // Simulate the DB-half clearing the reviewer step's agent association
        // (the worker's real CloseAgentNode effect path).
        for step in &mut view.steps {
            if step.node_id == "reviewer" {
                step.agent_node_id = None;
            }
        }
        events.clear();
        observe_close_agent_retries(&view, &mut events);
        assert!(
            events.is_empty(),
            "after the reviewer association clears, close_retry must NOT re-emit (would loop forever)"
        );
    }

    // -- worker panic isolation (issue #1235) -----------------------------
    //
    // Headline regression for the circuits worker: a single panic inside
    // `run_pass` (e.g. a serde edge case on a freshly-loaded graph)
    // unwinds the worker thread and the circuits stop advancing forever.
    // The fix wraps each per-pass body in `run_worker_pass`. The test
    // below drives the same call shape as `start_circuit_worker`'s loop
    // body and asserts the second + third tick still run after the
    // first panicked.

    #[test]
    fn circuit_worker_loop_survives_a_panicking_drive_pass() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let successes = AtomicUsize::new(0);
        for tick in 0..3 {
            let successes_ref = &successes;
            crate::process_util::run_worker_pass("circuits:drive", move || {
                if tick == 0 {
                    panic!("fault injection: first drive pass panics");
                }
                successes_ref.fetch_add(1, Ordering::SeqCst);
            });
        }
        assert_eq!(
            successes.load(Ordering::SeqCst),
            2,
            "ticks 1 + 2 must succeed after tick 0 panicked"
        );
    }

    fn open_pr_run() -> RunView {
        let graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        let steps = graph.nodes.iter().map(|node| StepView {
            node_id: node.id.clone(),
            status: if node.id == "open_pr" { StepStatus::Running } else { StepStatus::Completed },
            outcome: None, error: None,
            agent_node_id: if node.id == "implementer" { Some(700) } else { None },
            attempt: 1,
        }).collect();
        RunView { run_id: 42, graph, state: RunState::Running,
            context: CircuitContext::new(), steps }
    }

    fn pushed_implementation(agent: i64) -> Result<crate::autopilot::pipeline::WrapupState, String> {
        assert_eq!(agent, 700);
        Ok(crate::autopilot::pipeline::WrapupState {
            dirty: false, pushed: true, branch: Some("renamed-implementation".into()), pr_url: None, pr_number: None,
            pr_required: false, repo_error: None,
        })
    }

    fn implementation_pr() -> crate::services::github::PullRequest {
        serde_json::from_value(serde_json::json!({
            "number": 314, "html_url": "https://github.com/example/repo/pull/314",
            "title": "Implementation", "head": { "ref": "renamed-implementation" }
        })).unwrap()
    }

    fn set_open_pr_policy(
        run: &mut RunView,
        policy: Option<crate::autopilot::circuit::model::OpenPrPolicy>,
    ) {
        if let Some(node) = run.graph.nodes.iter_mut().find(|node| node.id == "open_pr") {
            if let CircuitNodeKind::GithubAction { open_pr_policy, .. } = &mut node.kind {
                *open_pr_policy = policy;
            }
        }
    }

    #[test]
    fn open_pr_acknowledges_existing_pr_and_publishes_review_context() {
        let mut run = open_pr_run();
        let event = ensure_open_pr(&run, "open_pr", Some(crate::autopilot::circuit::model::OpenPrPolicy::RequireExisting), pushed_implementation,
            |head| { assert_eq!(head, "renamed-implementation"); Ok(Some(implementation_pr())) },
            |_, _| panic!("existing PR must not be created again"),
        ).unwrap();
        let transition = advance(&mut run, &event);
        assert!(transition.step_writes.iter().any(|write| write.node_id == "open_pr" && write.status == StepStatus::Completed));
        assert_eq!(run.context.get("pr.number"), Some("314"));
        assert_eq!(run.context.get("pr.url"), Some("https://github.com/example/repo/pull/314"));
        assert_eq!(run.context.get("pr.head_ref"), Some("renamed-implementation"));
    }

    #[test]
    fn open_pr_review_requires_agent_pr_without_creating_one() {
        let error = ensure_open_pr(&open_pr_run(), "open_pr", Some(crate::autopilot::circuit::model::OpenPrPolicy::RequireExisting), pushed_implementation,
            |_| Ok(None), |_, _| panic!("review blueprint delegates creation to its agent"),
        ).unwrap_err();
        assert!(error.contains("did not raise an open pull request"));
    }

    #[test]
    fn open_pr_lookup_error_does_not_create_or_report_missing_pr() {
        let mut run = open_pr_run();
        set_open_pr_policy(&mut run, None);
        let error = ensure_open_pr(&run, "open_pr", None, pushed_implementation,
            |_| Err("GitHub unavailable".into()), |_, _| panic!("unknown is not absent"),
        ).unwrap_err();
        assert_eq!(error, "GitHub unavailable");
    }

    #[test]
    fn open_pr_replay_after_creation_discovers_pr_without_duplicate() {
        let mut run = open_pr_run();
        set_open_pr_policy(&mut run, None);
        let created = std::cell::RefCell::new(None);
        let creates = std::cell::Cell::new(0);
        // Simulate losing the first result before its ledger commit: replay
        // the same Running snapshot against the now-existing remote PR.
        for _ in 0..2 {
            let event = ensure_open_pr(&run, "open_pr", None, pushed_implementation,
                |_| Ok(created.borrow().clone()),
                |head, _| {
                    assert_eq!(head, "renamed-implementation");
                    creates.set(creates.get() + 1);
                    let pr = implementation_pr();
                    let url = pr.clone();
                    *created.borrow_mut() = Some(pr);
                    Ok(url)
                },
            ).unwrap();
            assert!(matches!(event, CircuitEvent::GithubActionResult { success: true, pr_number: Some(314), .. }));
        }
        assert_eq!(creates.get(), 1);
    }

    #[test]
    fn open_pr_failed_git_observation_does_not_query_github() {
        let error = ensure_open_pr(&open_pr_run(), "open_pr", Some(crate::autopilot::circuit::model::OpenPrPolicy::RequireExisting),
            |_| Err("worktree unavailable".into()),
            |_| panic!("branch identity is unknown"), |_, _| panic!("branch identity is unknown"),
        ).unwrap_err();
        assert_eq!(error, "worktree unavailable");
    }

    #[test]
    fn determine_github_target_routes_correctly() {
        use crate::autopilot::circuit::model::{CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, GithubActionKind};
        use crate::autopilot::circuit::stepper::{RunState, RunView};

        let graph = CircuitGraph {
            version: 1,
            blueprint: None,
            nodes: vec![
                CircuitNode { id: "t".into(), kind: CircuitNodeKind::Manual },
                CircuitNode {
                    id: "pre_label".into(),
                    kind: CircuitNodeKind::GithubAction {
                        action: GithubActionKind::AddLabel,
                        open_pr_policy: None,
                        label: Some("in-progress".into()),
                        comment: None,
                    },
                },
                CircuitNode {
                    id: "open_pr".into(),
                    kind: CircuitNodeKind::GithubAction {
                        action: GithubActionKind::OpenPr,
                        open_pr_policy: Some(crate::autopilot::circuit::model::OpenPrPolicy::RequireExisting),
                        label: None,
                        comment: None,
                    },
                },
                CircuitNode {
                    id: "post_label".into(),
                    kind: CircuitNodeKind::GithubAction {
                        action: GithubActionKind::AddLabel,
                        open_pr_policy: None,
                        label: Some("approved".into()),
                        comment: None,
                    },
                },
            ],
            edges: vec![
                CircuitEdge { from: "t".into(), to: "pre_label".into(), condition: Default::default() },
                CircuitEdge { from: "pre_label".into(), to: "open_pr".into(), condition: Default::default() },
                CircuitEdge { from: "open_pr".into(), to: "post_label".into(), condition: Default::default() },
            ],
        };

        let mut ctx = CircuitContext::new();
        ctx.set("issue.number", "42");
        ctx.set("pr.number", "1361");

        let view = RunView {
            run_id: 1,
            graph,
            state: RunState::Running,
            context: ctx,
            steps: vec![],
        };

        // Before OpenPr, AddLabel targets the issue (#42)
        let pre_target = determine_github_target(&view, "pre_label", GithubActionKind::AddLabel).unwrap();
        assert_eq!(pre_target, ("issue", 42));

        // After OpenPr, AddLabel targets the created PR (#1361)
        let post_target = determine_github_target(&view, "post_label", GithubActionKind::AddLabel).unwrap();
        assert_eq!(post_target, ("pr", 1361));

        // CloseIssue explicitly targets the issue (#42)
        let close_target = determine_github_target(&view, "post_label", GithubActionKind::CloseIssue).unwrap();
        assert_eq!(close_target, ("issue", 42));
    }

    // -----------------------------------------------------------------------
    // `resolve_circuit_spawn_inputs` (issue #1358 / slice 3 of #1355)
    //
    // The pure seam that translates a `CircuitNodeKind::SpawnAgentNode` into
    // the inputs the worker's impure wrapper threads into `create_pending`
    // (for the `provider` column) and `SpawnRequest::with_explicit(...)`
    // (for cascade layer-1 model/effort/extra_args). Mirrors the existing
    // impure-wrapper / pure-core pattern this file uses for
    // `reconcile_spawn_step` (line ~947) so the cascade + capability-mask
    // contract can be tested without a Tauri runtime, AppHandle, or DB.
    // -----------------------------------------------------------------------

    fn spawn_kind(
        provider: Option<&str>,
        model: Option<&str>,
        effort: Option<&str>,
        extra_args: Option<&str>,
    ) -> CircuitNodeKind {
        CircuitNodeKind::SpawnAgentNode {
            prompt: "implement the fix".to_string(),
            name: Some("implementer".to_string()),
            provider: provider.map(str::to_string),
            model: model.map(str::to_string),
            effort: effort.map(str::to_string),
            extra_args: extra_args.map(str::to_string),
        }
    }

    #[test]
    fn circuit_first_turn_prefills_when_the_harness_supports_it() {
        use crate::autopilot::launch::{initial_prompt_delivery, InitialPromptDelivery};
        assert_eq!(
            initial_prompt_delivery("claude", "implement the issue"),
            InitialPromptDelivery::Prefill
        );
        assert_eq!(
            initial_prompt_delivery("codex:custom-provider", "review the PR"),
            InitialPromptDelivery::Prefill
        );
    }

    #[test]
    fn circuit_first_turn_falls_back_to_pty_injection_without_prefill() {
        use crate::autopilot::launch::{initial_prompt_delivery, InitialPromptDelivery};
        assert_eq!(
            initial_prompt_delivery("kimi", "implement the issue"),
            InitialPromptDelivery::InjectAfterSpawn
        );
        assert_eq!(
            initial_prompt_delivery("dsh", "review the PR"),
            InitialPromptDelivery::InjectAfterSpawn
        );
    }

    #[test]
    fn circuit_empty_first_turn_stays_fresh() {
        use crate::autopilot::launch::{initial_prompt_delivery, InitialPromptDelivery};
        assert_eq!(
            initial_prompt_delivery("claude", "  \n\t"),
            InitialPromptDelivery::Fresh
        );
    }

    /// A node-authored `provider: Some("codex")` flows through into the
    /// row column AND into the resolved Provider the capability mask uses.
    /// Without this seam the worker would never honour a per-step
    /// provider override.
    #[test]
    fn circuit_spawn_resolves_provider_override() {
        let kind = spawn_kind(Some("codex"), None, None, None);
        let resolved = resolve_circuit_spawn_inputs(&kind).expect("valid spawn");
        assert_eq!(resolved.provider_str.as_deref(), Some("codex"));
    }

    /// The cascade layer-1 (explicit) override slot must carry the per-node
    /// `model`. Empty / whitespace-only inputs collapse to absent so the
    /// cascade falls through (issue #1148 AC #32).
    #[test]
    fn circuit_spawn_passes_model_through_explicit_override() {
        let kind = spawn_kind(Some("anthropic"), Some("opus-4-1"), None, None);
        let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
        assert_eq!(resolved.explicit.model.as_deref(), Some("opus-4-1"));
        assert_eq!(resolved.explicit.effort, None);
    }

    /// `effort` and `extra_args` ride the same explicit slot — pin both so
    /// the seam doesn't accidentally drop one of them on its way through.
    #[test]
    fn circuit_spawn_passes_effort_and_extra_args_through_explicit_override() {
        let kind = spawn_kind(
            Some("anthropic"),
            None,
            Some("high"),
            Some("--dangerously-skip-permissions"),
        );
        let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
        assert_eq!(resolved.explicit.effort.as_deref(), Some("high"));
        assert_eq!(
            resolved.explicit.extra_args.as_deref(),
            Some("--dangerously-skip-permissions")
        );
    }

    /// Unknown provider strings stay as-is in the seam so the row
    /// carries the user-authored value. The Anthropic fallback
    /// (`Provider::from_db_str("...")` returning `Anthropic` for unknown
    /// ids) runs downstream in `spawn_with_intent` against the row —
    /// the seam does not parse it. Pin that contract so a future
    /// refactor can't normalize the value too early and break the row's
    /// author-visible identity.
    #[test]
    fn circuit_spawn_preserves_unknown_provider_string() {
        let kind = spawn_kind(Some("not-a-real-thing"), None, None, None);
        let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
        assert_eq!(resolved.provider_str.as_deref(), Some("not-a-real-thing"));
    }

    /// Whitespace-only model / effort / extra_args collapse to absent so
    /// the cascade falls through.
    #[test]
    fn circuit_spawn_whitespace_overrides_collapse_to_absent() {
        let kind = spawn_kind(None, Some("   "), Some("\t\n"), Some("   \t  "));
        let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
        assert!(resolved.explicit.model.is_none());
        assert!(resolved.explicit.effort.is_none());
        assert!(resolved.explicit.extra_args.is_none());
    }

    /// `name` is pass-through (no cascade layer owns it).
    #[test]
    fn circuit_spawn_name_passes_through_unchanged() {
        let kind = spawn_kind(Some("claude_code"), None, None, None);
        let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
        assert_eq!(resolved.name.as_deref(), Some("implementer"));
    }

    /// A non-SpawnAgentNode kind is a hard error — the seam narrows
    /// before destructuring.
    #[test]
    fn circuit_spawn_rejects_non_spawn_kind() {
        let inject = CircuitNodeKind::InjectPty {
            prompt: "hi".into(),
            target_node_id: None,
        };
        let err = resolve_circuit_spawn_inputs(&inject).unwrap_err();
        assert!(
            err.contains("not a spawn"),
            "rejection message must name the kind: {err}"
        );
    }

    /// `provider: None` leaves the AST override empty. The worker then
    /// resolves the effective provider through the same explicit -> mesh ->
    /// application default chain as legacy Autopilot before creating the
    /// row, so this pure resolver remains free of database access.
    #[test]
    fn circuit_spawn_default_provider_is_none_when_unset() {
        let kind = spawn_kind(None, None, None, None);
        let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
        assert!(
            resolved.provider_str.is_none(),
            "None -> resolve the mesh/application default at spawn time"
        );
    }
}
