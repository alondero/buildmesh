//! The Autopilot Circuits worker (spec #1205 / walking skeleton #1206):
//! the impure seam around the pure stepper
//! (`circuit::stepper`).
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
//! 2. **Steps** via [`advance`](circuit::stepper::advance) —
//!    no DB, no I/O;
//! 3. **Commits** the decided writes atomically through the Circuit
//!    evidence transaction; local `SetNodeStatus` changes commit with their
//!    completed step;
//! 4. **Executes** external or transient effects (spawn agent node, inject
//!    PTY prompt, notify UI).
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
//! process and git state (resume / fail). The [`turn_classify::lost_turn_watchdog_pass`]
//! recovers quiet piloted nodes whose turn webhook was missed so
//! multi-hour runs self-heal.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use tauri::{AppHandle, Emitter};

use crate::circuit::capacity;
use crate::circuit::context::CircuitContext;
use crate::circuit::model::{CircuitGraph, CircuitNodeKind, StepOutcome as GraphStepOutcome};
use crate::circuit::stepper::{
    advance, CircuitEvent, RunState, RunView, StepStatus, StepView, Transition,
};
mod admission;
mod github;
#[cfg(test)]
mod github_recovery_tests;
mod jobs;
pub(crate) mod native_hooks;
mod native_pull;
mod observation;
pub(crate) mod observer_policy;
pub(crate) mod readiness;
mod report_contract;
mod restart;
mod spawn;
mod turn_classify;
mod zombie_sweep;
use admission::{global_agent_reservation_fits, may_admit_run, required_agent_slots};
use observation::{agent_lookup_for_observation, observe};
pub use restart::startup_reconcile_pass;
use restart::{recover_run_observers, release_run_evaluators, restore_run_evaluators};
use turn_classify::{quiet_turn_is_current, QuietClassifierFailure, QuietTurnEvidence};
#[cfg(test)]
mod observe_parity_tests;
#[cfg(test)]
mod observer_decision_tests;
use crate::db;
use crate::models::SessionStatus;
use crate::process_util::run_worker_pass;

/// Fast tick — covers interval pacing headroom, slot unblocking latency,
/// and piloted-agent observation lag.
const TICK_INTERVAL: Duration = Duration::from_secs(2);

/// Startup delay so boot-time DB migration finishes before the first
/// pass for active Circuit runs.
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
        if entry.in_flight == 0 && (!entry.cancelled.load(Ordering::Acquire) || entry.finished) {
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
    let entry = active
        .entry(run_id)
        .or_insert_with(|| CircuitEffectCancellation {
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
    let entry = active
        .entry(run_id)
        .or_insert_with(|| CircuitEffectCancellation {
            cancelled: Arc::new(AtomicBool::new(false)),
            finished: false,
            in_flight: 0,
        });
    entry.cancelled.store(true, Ordering::Release);
    entry.finished = false;
    drop(active);
    jobs::cancel_run(run_id);
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

pub(super) struct CircuitSpawnPermit {
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
pub(super) fn begin_circuit_spawn(run_id: i64) -> Result<Option<CircuitSpawnPermit>, String> {
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
pub fn with_circuit_run_spawns_quiesced<T>(
    run_id: i64,
    f: impl FnOnce() -> T,
) -> Result<T, String> {
    let (lock, _) = &*CIRCUIT_SPAWNS;
    let active = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_active, quiesced) = wait_for_spawn_set_to_empty(active, &[run_id]);
    if !quiesced {
        return Err(format!(
            "circuit run {} still has a spawn in flight",
            run_id
        ));
    }
    Ok(f())
}

/// Same barrier for deleting a whole circuit. The caller must disable new
/// trigger ingestion before entering this function; existing worker passes
/// are covered by the mutex and late spawns fail their durable-state check.
pub fn with_circuit_spawns_quiesced<T>(
    circuit_id: i64,
    f: impl FnOnce() -> T,
) -> Result<T, String> {
    let (lock, _) = &*CIRCUIT_SPAWNS;
    let active = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Snapshot run ids only after taking the same mutex used by stage-1
    // spawn admission. A trigger that raced the caller's disable write cannot
    // slip into the protected deletion window between a query and lock
    // acquisition.
    let run_ids =
        db::list_circuit_run_ids_for_cleanup(circuit_id).map_err(|error| error.to_string())?;
    let (_active, quiesced) = wait_for_spawn_set_to_empty(active, &run_ids);
    if !quiesced {
        return Err(format!(
            "circuit {} still has a spawn in flight",
            circuit_id
        ));
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
/// shape matches `db::write_conn()` and `services::circuit_worker::lock_circuit_worker_static`.
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
    let (retained, dropped) = retain_active_approvals(std::mem::take(&mut *queue), active_runs);
    *queue = retained;
    if dropped > 0 {
        tracing::debug!(
            "circuits: dropped {} stale approval(s) for vanished runs",
            dropped
        );
    }
}

/// Keep approvals belonging to active runs. Takes ownership so this policy
/// can be tested without touching the process-wide queue.
fn retain_active_approvals(
    mut approvals: Vec<(i64, String)>,
    active_runs: &[db::ActiveCircuitRun],
) -> (Vec<(i64, String)>, usize) {
    let before = approvals.len();
    approvals.retain(|(run_id, _)| active_runs.iter().any(|run| run.run.id == *run_id));
    let dropped = before - approvals.len();
    (approvals, dropped)
}

/// Wake the circuit worker immediately (manual trigger dispatch).
pub fn wake_circuit_worker() {
    let (_lock, cvar) = &*WAKE;
    cvar.notify_all();
}

/// Start the dedicated Circuit worker thread. Called once from Tauri
/// `setup`. Startup order:
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
                    apply_quiet_classifier_failures(&app, jobs::watchdog(app.clone()));
                });
                // Issue #1793: reap piloted nodes stuck `running` with no
                // session identity or readable report. Self-throttled, so this
                // is a cheap no-op on almost every tick.
                run_worker_pass("circuits:zombie-sweep", || {
                    zombie_sweep::zombie_sweep_pass(&app);
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
fn should_drive_circuit_run(enabled: bool, trigger_identity: &str, review_extended: bool) -> bool {
    enabled || trigger_identity.starts_with("manual:") || review_extended
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
    jobs::retain_runs(&runs);

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
    retry_failed_cleanup(app);
    // Repair leases for active runs created before the reservation table was
    // introduced. This is idempotent and keeps restarts from losing a claim.
    for active in &runs {
        if RunState::is_admitted_db_str(&active.run.state) {
            let required = required_agent_slots(active);
            if required > 0
                && db::circuit_agent_slots_reserved(active.run.id).unwrap_or(0) < required
            {
                let _ = db::reserve_circuit_agent_slots(active.run.id, required);
            }
        }
    }
    let global_pool = crate::preferences::circuit_agent_pool_size();
    // Active circuit runs are represented by their lease. Terminal runs do
    // not hold a lease, but retained implementation PTYs still consume host
    // optional app-wide pool until their agent rows are archived/deleted;
    // include those non-leased agents in the global baseline.
    let retained_total = db::count_retained_circuit_agent_nodes_total().unwrap_or(i64::MAX);
    let unleased_slots = retained_total;
    let mut reserved_circuit_slots =
        db::count_reserved_circuit_agent_slots_total().unwrap_or(i64::MAX);
    for active in runs {
        // In-flight runs always drive to completion even when their circuit
        // is disabled: disabling stops NEW background work (pending
        // admission / trigger minting), it must not wedge admitted runs
        // holding a `circuit_run_capacity` slot forever. Only pending runs
        // gate on the enabled flag (manual Trigger Now stays a dry-run seam
        // on drafts).
        if active.run.state == "pending"
            && !should_drive_circuit_run(
                active.circuit_enabled,
                &active.run.trigger_identity,
                // A review extension or an operator's recovery is explicit work
                // on a run that already started, so a disabled circuit must not
                // park it as if it were new background work.
                CircuitContext::from_json(&active.run.context_json)
                    .ok()
                    .is_some_and(|context| {
                        context.get("review.extended") == Some("1")
                            || context.get("operator.recovered") == Some("1")
                    }),
            )
        {
            record_queue_wait(
                active.run.id,
                db::circuit::evidence::QueueWaitReason::CircuitDisabled,
            );
            continue;
        }
        // Pending runs that the gate deferred re-appear next pass;
        // running/paused runs always proceed (they already hold a slot).
        if active.run.state == "pending" {
            let mesh = mesh_cache
                .entry(active.run.mesh_id)
                .or_insert_with(|| db::get_mesh_by_id(active.run.mesh_id).ok());
            match mesh {
                Some(m) if !may_admit_run(&active, m) => {
                    record_queue_wait(
                        active.run.id,
                        db::circuit::evidence::QueueWaitReason::MeshCapacity {
                            capacity: i64::from(m.circuit_run_capacity),
                        },
                    );
                    continue;
                }
                None => {
                    // Orphan reaper: the mesh row is gone (deleted between
                    // mint and this pass). Leaving the row pending wedges
                    // the queue visualisation forever with a run that can
                    // never admit — terminalise it so the queue reflects
                    // reality. Cancellation is idempotent and also clears
                    // leases/steps.
                    tracing::warn!(
                        "circuits: pending run {} on missing mesh_id={} — cancelling orphan",
                        active.run.id,
                        active.run.mesh_id,
                    );
                    if let Err(e) = db::cancel_circuit_run(active.run.id) {
                        tracing::warn!(
                            "circuits: orphan cancel for run {} failed: {}",
                            active.run.id,
                            e
                        );
                    } else {
                        let _ = app.emit(
                            "circuit-run-updated",
                            CircuitRunUpdatedPayload {
                                run_id: active.run.id,
                                state: "cancelled".into(),
                            },
                        );
                        wake_circuit_worker();
                    }
                    continue;
                }
                Some(_) => {} // admitted by may_admit_run — reserve its lease below
            }
            let required = required_agent_slots(&active);
            if required > 0 {
                let existing = db::circuit_agent_slots_reserved(active.run.id).unwrap_or(0);
                let additional = required.saturating_sub(existing);
                if additional > 0
                    && !global_agent_reservation_fits(
                        additional,
                        reserved_circuit_slots,
                        unleased_slots,
                        global_pool,
                    )
                {
                    record_queue_wait(
                        active.run.id,
                        db::circuit::evidence::QueueWaitReason::AgentCapacity {
                            required: additional,
                            available: global_pool.map(|limit| {
                                i64::from(limit)
                                    .saturating_sub(reserved_circuit_slots)
                                    .saturating_sub(unleased_slots)
                                    .max(0)
                            }),
                        },
                    );
                    tracing::info!(
                        "circuits: global Circuit agent pool held — run {} needs {} reserved agent slot(s)",
                        active.run.id, required
                    );
                    continue;
                }
                if additional > 0 {
                    match db::reserve_circuit_agent_slots(active.run.id, required) {
                        Ok(true) => {
                            reserved_circuit_slots =
                                reserved_circuit_slots.saturating_add(additional);
                        }
                        Ok(false) | Err(_) => {
                            record_queue_wait(
                                active.run.id,
                                db::circuit::evidence::QueueWaitReason::ReservationUnavailable,
                            );
                            continue;
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

fn record_queue_wait(run_id: i64, reason: db::circuit::evidence::QueueWaitReason) {
    if let Err(error) = db::circuit::evidence::record_queue_wait(run_id, reason) {
        tracing::warn!("circuits: could not record queue wait for run {run_id}: {error}");
    }
}

/// Synthesize a ledger step carrying a corrupt-payload failure. The runs
/// table has no error column — cards render the first failed step's
/// message — so every poisoned-row path must write one of these rather
/// than advancing with empty step ops.
fn corrupt_payload_step_op(node_id: &str, reason: &str) -> db::CircuitStepOp {
    db::CircuitStepOp {
        node_id: node_id.to_string(),
        status: "failed".to_string(),
        outcome: Some(Some("failed".to_string())),
        error: Some(Some(reason.to_string())),
        agent_node_id: None,
        attempt: 1,
        fresh_attempt: false,
    }
}

fn drive_run(app: &AppHandle, active: &db::ActiveCircuitRun) -> Result<(), String> {
    // Undrivable-graph fail-closed: a corrupt `graph_json` previously
    // returned Err every 2s forever, holding the run's capacity slot with
    // no state change. Fail the run once with the parse error so the
    // ledger explains itself and the slot frees. The runs table has no
    // error column, so every corrupt-payload path synthesizes a failure
    // step op (`__graph__` / `__context__`) — the card renders the first
    // failed step's message as the run's reason.
    let graph = match CircuitGraph::from_json(&active.circuit_graph_json) {
        Ok(graph) => graph,
        Err(error) => {
            let reason = format!("circuit graph is unreadable: {}", error);
            tracing::warn!("circuits: run {} {}", active.run.id, reason);
            let op = corrupt_payload_step_op("__graph__", &reason);
            // Best-effort: even if the commit fails, return Ok so run_pass
            // does not log-and-retry this poisoned row at full tick rate —
            // the next pass will retry the commit anyway.
            let _ = db::commit_circuit_advance(
                active.run.id,
                Some(crate::circuit::stepper::RunState::Failed.as_db_str()),
                None,
                &[op],
            );
            let _ = app.emit(
                "circuit-run-updated",
                CircuitRunUpdatedPayload {
                    run_id: active.run.id,
                    state: "failed".into(),
                },
            );
            wake_circuit_worker();
            return Ok(());
        }
    };
    let mut context = match CircuitContext::from_json(&active.run.context_json) {
        Ok(context) => context,
        Err(error) => {
            let reason = format!("circuit context is unreadable: {}", error);
            tracing::warn!("circuits: run {} {}", active.run.id, reason);
            let op = corrupt_payload_step_op("__context__", &reason);
            let _ = db::commit_circuit_advance(
                active.run.id,
                Some(crate::circuit::stepper::RunState::Failed.as_db_str()),
                None,
                &[op],
            );
            let _ = app.emit(
                "circuit-run-updated",
                CircuitRunUpdatedPayload {
                    run_id: active.run.id,
                    state: "failed".into(),
                },
            );
            wake_circuit_worker();
            return Ok(());
        }
    };
    let revision =
        db::circuit::evidence::observation_revision(&active.run).map_err(|e| e.to_string())?;
    context.set("evidence.revision", revision.to_string());
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
    // Repair only legacy conflicts whose source is proven by their original
    // history digest. The next ordinary observation commits the reconciliation.
    for step in &view.steps {
        let key = format!("node.{}.evidence.{}", step.node_id, step.attempt);
        let Some(mut evidence) = view.context.get(&key).and_then(|json| {
            serde_json::from_str::<crate::circuit::observation::WorkEvidence>(json).ok()
        }) else {
            continue;
        };
        if evidence
            .conflicts
            .iter()
            .any(|conflict| conflict.status_projection.is_none())
            && db::circuit::evidence::restore_projection_conflicts(
                view.run_id,
                &step.node_id,
                step.attempt,
                &mut evidence,
            )
            .map_err(|error| error.to_string())?
        {
            view.context.set(
                &key,
                serde_json::to_string(&evidence).map_err(|error| error.to_string())?,
            );
        }
    }
    jobs::reconcile(&view);

    if let Some(source) = active.run.source_agent_node_id {
        let lost = agent_lookup_for_observation(source, db::get_agent_node_by_id(source))
            .map_err(|error| error.to_string())?
            .map(|n| {
                matches!(
                    n.status,
                    SessionStatus::Archived | SessionStatus::Lost | SessionStatus::Error
                )
            })
            .unwrap_or(true);
        if lost {
            persist_source_agent_loss(&mut view, source, |view, transition| {
                persist_transition_checked(active.run.id, view, transition)
            })
            .map_err(TransitionPersistFailure::into_message)?;
            close_run_agents(&view);
            crate::circuit::evaluator::unregister(source);
            let _ = app.emit(
                "circuit-run-updated",
                CircuitRunUpdatedPayload {
                    run_id: active.run.id,
                    state: "failed".into(),
                },
            );
            return Ok(());
        }
    }
    restore_run_evaluators(&view);
    recover_run_observers(app, &view);

    for event in observe(app, active, &view) {
        let persisted =
            advance_and_persist_observed_event(&mut view, &event, |view, transition| {
                persist_transition_checked(active.run.id, view, transition)
            });
        let (transition, turn_boundary_changed) = match persisted {
            Ok(result) => result,
            Err(TransitionPersistFailure::AgentStatusEffectRejected {
                expected,
                effects,
                message,
            }) => {
                tracing::warn!(
                    "circuits: run {} SetNodeStatus commit failed: {}; failing the local effect",
                    active.run.id,
                    message
                );
                fail_rejected_agent_status_effects(
                    &mut view,
                    &expected,
                    &effects,
                    &message,
                    |context, steps, expected| {
                        db::circuit::evidence::commit_transition(
                            active.run.id,
                            Some(RunState::Failed.as_db_str()),
                            context,
                            steps,
                            db::circuit::evidence::EvidenceWrite {
                                expected: Some(expected),
                                ..Default::default()
                            },
                        )
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                    },
                )?;
                close_run_agents(&view);
                if let Some(source) = active.run.source_agent_node_id {
                    crate::circuit::evaluator::unregister(source);
                }
                let _ = app.emit(
                    "circuit-run-updated",
                    CircuitRunUpdatedPayload {
                        run_id: active.run.id,
                        state: RunState::Failed.as_db_str().to_string(),
                    },
                );
                return Ok(());
            }
            Err(error) => return Err(error.into_message()),
        };
        // Capture the pre-prompt transcript revision in the same transaction
        // as the pure transition. This keeps the durable turn boundary on the
        // normal commit path; effect execution only performs the in-memory
        // PTY boundary immediately before writing bytes.
        // Commit FIRST (atomically), then execute effects — a crash
        // after the commit is repaired by observation next pass. The
        // (possibly run_id-topped-up) context rides along with every
        // commit so the seeding above lands whenever anything else
        // writes; a pass with no commits simply re-seeds next time.
        let effect_events = match execute_effects(app, active, &mut view, &transition.effects) {
            Ok(events) => events,
            Err(e) => {
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
                        crate::circuit::stepper::Effect::SpawnAgentNode { node_id }
                        | crate::circuit::stepper::Effect::InjectPty { node_id, .. }
                        | crate::circuit::stepper::Effect::ContinueAgentTurn { node_id, .. }
                        | crate::circuit::stepper::Effect::NudgeIdleAgent { node_id, .. }
                        | crate::circuit::stepper::Effect::CloseAgentNode { node_id, .. }
                        | crate::circuit::stepper::Effect::CallGithub { node_id, .. } => {
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
                        error: Some(Some(e.clone())),
                        agent_node_id: None,
                        attempt: 1,
                        fresh_attempt: false,
                    })
                    .collect::<Vec<_>>();
                db::commit_circuit_advance(
                    active.run.id,
                    Some(crate::circuit::stepper::RunState::Failed.as_db_str()),
                    None,
                    &ops,
                )
                .map_err(|commit_err| {
                    format!("effect-failure commit also failed: {}", commit_err)
                })?;
                view.state = RunState::Failed;
                // No emit here: the terminal branch below emits the single
                // "failed" event after `close_run_agents` sweeps the agents.
                Vec::new()
            }
        };

        // Effects may report a durable outcome only after their external work
        // completes. Feed those outcomes back through the stepper and its
        // normal atomic commit path; effect execution never writes run state.
        let exec_app = app.clone();
        let emit_app = app.clone();
        drain_effect_outcomes(
            active.run.id,
            &mut view,
            effect_events,
            &mut |view, effects| execute_effects(&exec_app, active, view, effects),
            &mut |view, outcome, outcome_changed| {
                // A terminal state is emitted once, after the cleanup sweep below,
                // so a run does not fan out two refetches on the way out.
                if !view.state.is_terminal()
                    && (!outcome.step_writes.is_empty()
                        || outcome.run_state_changed
                        || outcome.context_changed
                        || outcome_changed)
                {
                    let _ = emit_app.emit(
                        "circuit-run-updated",
                        CircuitRunUpdatedPayload {
                            run_id: active.run.id,
                            state: view.state.as_db_str().to_string(),
                        },
                    );
                }
            },
        )?;

        // Live ledger: every step transition or state change refreshes
        // the Probe tab, not just terminal ones — otherwise a long agent
        // run renders as a frozen list until it finishes. Terminal states are
        // held back to the single post-cleanup emit in the branch below.
        if !view.state.is_terminal()
            && (!transition.step_writes.is_empty()
                || transition.run_state_changed
                || transition.context_changed
                || turn_boundary_changed)
        {
            let _ = app.emit(
                "circuit-run-updated",
                CircuitRunUpdatedPayload {
                    run_id: active.run.id,
                    state: view.state.as_db_str().to_string(),
                },
            );
        }

        // Failed and cancelled runs retire their owned processes while
        // retaining recovery checkpoints; every terminal run stops piloting
        // the agents it hands back.
        if view.state.is_terminal() {
            close_run_agents(&view);
            // The one terminal emit, after `close_run_agents` has archived the
            // run's remaining agents — so the frontend resync observes the
            // retired rows instead of racing the sweep.
            let _ = app.emit(
                "circuit-run-updated",
                CircuitRunUpdatedPayload {
                    run_id: active.run.id,
                    state: view.state.as_db_str().to_string(),
                },
            );
            break;
        }
    }
    if view.state.is_terminal() {
        if let Some(source) = active.run.source_agent_node_id {
            crate::circuit::evaluator::unregister(source);
        }
    }
    Ok(())
}

/// Persist exactly one pure stepper transition. All durable run/context/step
/// writes, including post-effect delivery outcomes, pass through this seam.
pub(super) fn persist_transition(
    run_id: i64,
    view: &mut RunView,
    transition: &Transition,
) -> Result<bool, String> {
    persist_transition_checked(run_id, view, transition)
        .map_err(TransitionPersistFailure::into_message)
}

fn dispatch_prompt_and_acknowledge(
    submit: impl FnOnce() -> Result<bool, String>,
    acknowledge: impl FnOnce() -> Result<Option<i64>, String>,
) -> Result<Option<i64>, String> {
    if !submit()? {
        return Err("Input ownership changed before prompt submission completed".into());
    }
    acknowledge()
}

/// Commit one effect outcome through the production seam: advance the event,
/// then persist the transition atomically. `drive_run` composes this into its
/// pending-outcome loop; the deterministic coverage drives it directly so a
/// late result meets the same commit path as a live one.
pub(super) fn persist_effect_outcome(
    run_id: i64,
    view: &mut RunView,
    event: &CircuitEvent,
) -> Result<(Transition, bool), String> {
    let outcome = advance(view, event);
    let changed = persist_transition(run_id, view, &outcome)?;
    Ok((outcome, changed))
}

/// Executes the follow-on effects a drained outcome collects.
pub(super) type FollowOnExecutor<'a> = dyn FnMut(&mut RunView, &[crate::circuit::stepper::Effect]) -> Result<Vec<CircuitEvent>, String>
    + 'a;

/// Observes each committed outcome while draining.
pub(super) type OutcomeObserver<'a> = dyn FnMut(&RunView, &Transition, bool) + 'a;

/// Drain outcome events through persistence, executing follow-on effects via
/// the caller's executor and reporting each commit to its observer. App-free:
/// `drive_run` passes `execute_effects` plus its progress emit, so the
/// pending-outcome loop's composition — persist, extend, observe, in that
/// order — lives here once instead of being reproduced by coverage. A
/// rejected commit aborts the drain with `Err` before any follow-on runs.
pub(super) fn drain_effect_outcomes(
    run_id: i64,
    view: &mut RunView,
    initial: Vec<CircuitEvent>,
    execute_follow_ons: &mut FollowOnExecutor<'_>,
    on_committed: &mut OutcomeObserver<'_>,
) -> Result<(), String> {
    let mut pending = std::collections::VecDeque::from(initial);
    while let Some(event) = pending.pop_front() {
        let (outcome, changed) = persist_effect_outcome(run_id, view, &event)?;
        pending.extend(execute_follow_ons(view, &outcome.effects)?);
        on_committed(view, &outcome, changed);
    }
    Ok(())
}

#[derive(Debug)]
enum TransitionPersistFailure {
    FreshnessRejected(String),
    AgentStatusEffectRejected {
        expected: crate::circuit::stepper::TransitionFence,
        effects: Vec<crate::circuit::stepper::Effect>,
        message: String,
    },
    Other(String),
}

impl TransitionPersistFailure {
    fn into_message(self) -> String {
        match self {
            Self::FreshnessRejected(message) | Self::Other(message) => message,
            Self::AgentStatusEffectRejected { message, .. } => message,
        }
    }
}

/// Why a circuit close must leave a node open, when it must.
///
/// A helper (the reviewer) is always closed. The implementation agent is the
/// user's work: even after a merge GitHub confirmed, a close must not delete a
/// Why a circuit close must leave a node open, when it must.
///
/// A helper (the reviewer) is always closed. The implementation agent is the
/// user's work: even after a merge GitHub confirmed, a close must not delete
/// a worktree that still holds uncommitted changes. The caller resolves the
/// safety lookup itself: helpers do not call the worktree helper at all, and
/// an unreadable worktree is treated as "has changes" by the caller before
/// reaching this function, so a DB error is never silently swallowed.
pub(super) fn close_blocker(
    is_helper: bool,
    safety: &crate::git::worktree::WorktreeCloseSafety,
) -> Option<String> {
    if is_helper {
        return None;
    }
    if safety.has_uncommitted {
        Some("its worktree has uncommitted changes".to_string())
    } else {
        None
    }
}

fn failed_effect_step_ops(
    view: &RunView,
    effects: &[crate::circuit::stepper::Effect],
    message: &str,
) -> Vec<db::CircuitStepOp> {
    use crate::circuit::stepper::Effect;
    let mut seen = HashSet::new();
    effects
        .iter()
        .filter_map(|effect| {
            let node_id = match effect {
                Effect::SpawnAgentNode { node_id }
                | Effect::InjectPty { node_id, .. }
                | Effect::ContinueAgentTurn { node_id, .. }
                | Effect::NudgeIdleAgent { node_id, .. }
                | Effect::SetNodeStatus { node_id, .. }
                | Effect::CloseAgentNode { node_id, .. }
                | Effect::CallGithub { node_id, .. } => node_id,
                Effect::Notify { .. } => return None,
            };
            if !seen.insert(node_id.clone()) {
                return None;
            }
            Some(db::CircuitStepOp {
                node_id: node_id.clone(),
                status: "failed".into(),
                outcome: Some(Some("failed".into())),
                error: Some(Some(message.into())),
                agent_node_id: None,
                attempt: view.step(node_id).map_or(1, |step| step.attempt),
                fresh_attempt: false,
            })
        })
        .collect()
}

fn fail_rejected_agent_status_effects(
    view: &mut RunView,
    expected: &crate::circuit::stepper::TransitionFence,
    effects: &[crate::circuit::stepper::Effect],
    message: &str,
    mut persist: impl FnMut(
        &str,
        &[db::CircuitStepOp],
        &crate::circuit::stepper::TransitionFence,
    ) -> Result<(), String>,
) -> Result<(), String> {
    let steps = failed_effect_step_ops(view, effects, message);
    if steps.is_empty() {
        return Err("SetNodeStatus rejection did not identify a failed effect step".into());
    }
    let context = view.context.to_json().map_err(|error| error.to_string())?;
    persist(&context, &steps, expected)?;
    view.state = RunState::Failed;
    for op in steps {
        if let Some(step) = view.step_mut(&op.node_id) {
            step.status = StepStatus::Failed;
            step.outcome = Some(crate::circuit::model::StepOutcome::Failed);
            step.error = Some(message.to_string());
        }
    }
    Ok(())
}

fn effect_persistence_writes(
    view: &RunView,
    effects: &[crate::circuit::stepper::Effect],
) -> Result<
    (
        Vec<db::circuit::evidence::EffectIntent>,
        Vec<db::circuit::evidence::AgentStatusEffect>,
    ),
    TransitionPersistFailure,
> {
    use crate::circuit::stepper::Effect;

    let mut intents = Vec::new();
    let mut agent_status_effects = Vec::new();
    let attempt_for = |node_id: &str| view.step(node_id).map_or(1, |step| step.attempt);
    for effect in effects {
        let intent = match effect {
            Effect::SpawnAgentNode { node_id } => Some(db::circuit::evidence::EffectIntent {
                node_id: node_id.clone(),
                attempt: attempt_for(node_id),
                kind: db::circuit::evidence::EffectKind::Spawn,
            }),
            Effect::InjectPty { node_id, .. } => Some(db::circuit::evidence::EffectIntent {
                node_id: node_id.clone(),
                attempt: attempt_for(node_id),
                kind: db::circuit::evidence::EffectKind::Prompt,
            }),
            Effect::CallGithub { node_id, .. } => Some(db::circuit::evidence::EffectIntent {
                node_id: node_id.clone(),
                attempt: attempt_for(node_id),
                kind: db::circuit::evidence::EffectKind::Github,
            }),
            Effect::SetNodeStatus {
                node_id, status, ..
            } => {
                let agent_node_id = view.resolve_target_agent(node_id).ok_or_else(|| {
                    TransitionPersistFailure::Other(format!(
                        "SetNodeStatus target agent not found in lineage for node {node_id}"
                    ))
                })?;
                agent_status_effects.push(db::circuit::evidence::AgentStatusEffect {
                    node_id: node_id.clone(),
                    attempt: attempt_for(node_id),
                    agent_node_id,
                    status: crate::models::SessionStatus::from_db_str(status),
                });
                None
            }
            // Continuation and nudge prompts have their own pending/claimed
            // delivery state in run context (`*.continuation.delivery`,
            // `*.nudge.delivery`), so they must not claim the step's durable
            // Prompt effect row — an `InjectPty` on the same step already owns
            // it. Close is replayed idempotently while its spawn association
            // remains; notifications are transient.
            Effect::ContinueAgentTurn { .. }
            | Effect::NudgeIdleAgent { .. }
            | Effect::CloseAgentNode { .. }
            | Effect::Notify { .. } => None,
        };
        if let Some(intent) = intent {
            intents.push(intent);
        }
    }
    Ok((intents, agent_status_effects))
}

fn persist_transition_checked(
    run_id: i64,
    view: &mut RunView,
    transition: &Transition,
) -> Result<bool, TransitionPersistFailure> {
    persist_transition_checked_with(
        run_id,
        view,
        transition,
        db::circuit::evidence::commit_transition,
    )
}

fn persist_transition_checked_with(
    run_id: i64,
    view: &mut RunView,
    transition: &Transition,
    commit: impl FnOnce(
        i64,
        Option<&str>,
        &str,
        &[db::CircuitStepOp],
        db::circuit::evidence::EvidenceWrite<'_>,
    ) -> rusqlite::Result<i64>,
) -> Result<bool, TransitionPersistFailure> {
    let turn_boundary_changed = prepare_turn_boundaries(view, &transition.effects)
        .map_err(TransitionPersistFailure::Other)?;
    if !transition.step_writes.is_empty()
        || !transition.observations.is_empty()
        || transition.run_state_changed
        || transition.context_changed
        || turn_boundary_changed
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
        let run_state = transition
            .run_state_changed
            .then_some(view.state.as_db_str());
        let reconciled_effects = transition
            .step_writes
            .iter()
            .filter_map(|write| {
                let key = format!("node.{}.effect_reconciled_attempt", write.node_id);
                (write.status == crate::circuit::stepper::StepStatus::Completed
                    && view
                        .context
                        .get(&key)
                        .and_then(|attempt| attempt.parse::<i32>().ok())
                        == Some(write.attempt))
                .then(|| db::circuit::evidence::ReconciledEffect {
                    intent: db::circuit::evidence::EffectIntent {
                        node_id: write.node_id.clone(),
                        attempt: write.attempt,
                        kind: db::circuit::evidence::EffectKind::Github,
                    },
                    detail: view
                        .context
                        .get(&format!("node.{}.effect_reconciled_detail", write.node_id))
                        .map(str::to_string)
                        .unwrap_or_else(|| "Read-only OpenPr recheck succeeded.".into()),
                })
            })
            .collect::<Vec<_>>();
        for effect in &reconciled_effects {
            view.context.set(
                &format!("node.{}.effect_reconciled_attempt", effect.intent.node_id),
                "0",
            );
            view.context.set(
                &format!("node.{}.effect_reconciled_detail", effect.intent.node_id),
                "0",
            );
        }
        let (intents, agent_status_effects) = effect_persistence_writes(view, &transition.effects)?;
        let rejected_status_effect = if agent_status_effects.is_empty() {
            None
        } else {
            transition
                .expected
                .clone()
                .map(|expected| (expected, transition.effects.clone()))
        };
        let context = view
            .context
            .to_json()
            .map_err(|error| TransitionPersistFailure::Other(error.to_string()))?;
        let revision = commit(
            run_id,
            run_state,
            &context,
            &ops,
            db::circuit::evidence::EvidenceWrite {
                input_guard: transition.input_guard.as_ref(),
                intents: &intents,
                agent_status_effects: &agent_status_effects,
                reconciled_effects: &reconciled_effects,
                observations: &transition.observations,
                classifications: &transition.classifications,
                expected: transition.expected.as_ref(),
            },
        )
        .map_err(|error| {
            let message = format!("commit failed: {error}");
            if db::circuit::evidence::is_observation_freshness_rejection(&error) {
                TransitionPersistFailure::FreshnessRejected(message)
            } else if let Some((expected, effects)) = rejected_status_effect {
                TransitionPersistFailure::AgentStatusEffectRejected {
                    expected,
                    effects,
                    message,
                }
            } else {
                TransitionPersistFailure::Other(message)
            }
        })?;
        view.context.set("evidence.revision", revision.to_string());
    }
    Ok(turn_boundary_changed)
}

fn advance_and_persist_observed_event(
    view: &mut RunView,
    event: &CircuitEvent,
    mut persist: impl FnMut(&mut RunView, &Transition) -> Result<bool, TransitionPersistFailure>,
) -> Result<(Transition, bool), TransitionPersistFailure> {
    let before_observation = view.clone();
    let transition = advance(view, event);
    match persist(view, &transition) {
        Ok(turn_boundary_changed) => Ok((transition, turn_boundary_changed)),
        Err(TransitionPersistFailure::FreshnessRejected(error)) => {
            let Some(fallback_event) =
                native_pull::freshness_rejection_recheck(&before_observation, event)
            else {
                *view = before_observation;
                return Err(TransitionPersistFailure::FreshnessRejected(error));
            };
            *view = before_observation.clone();
            tracing::info!(
                "circuits: run {} native recheck rejected by freshness fence; retaining Unverified state",
                view.run_id
            );
            let fallback_transition = advance(view, &fallback_event);
            match persist(view, &fallback_transition) {
                Ok(turn_boundary_changed) => Ok((fallback_transition, turn_boundary_changed)),
                Err(error) => {
                    *view = before_observation;
                    Err(error)
                }
            }
        }
        Err(error) => {
            *view = before_observation;
            Err(error)
        }
    }
}

fn persist_source_agent_loss(
    view: &mut RunView,
    source_agent_id: i64,
    persist: impl FnMut(&mut RunView, &Transition) -> Result<bool, TransitionPersistFailure>,
) -> Result<(Transition, bool), TransitionPersistFailure> {
    // The run row identifies the borrowed source even for legacy contexts
    // persisted before source.agent_id was added.
    view.context
        .set("source.agent_id", source_agent_id.to_string());
    advance_and_persist_observed_event(
        view,
        &CircuitEvent::AgentLost {
            agent_node_id: source_agent_id,
        },
        persist,
    )
}

/// Retire every agent attached to a failed circuit run. The operation is
/// idempotent with the normal close effect: a missing row simply means a
/// previous cleanup already won the race.
///
/// Deliberately emits nothing itself: both callers emit the terminal
/// `circuit-run-updated` *after* this sweep returns, so the frontend resync
/// observes the archived rows. The periodic `retry_failed_cleanup` sweep has no
/// such caller and emits its own event.
fn close_run_agents(view: &RunView) {
    let eligible = db::list_failed_circuit_agents_for_cleanup().unwrap_or_default();
    let mut agent_ids = HashSet::new();
    for agent_node_id in view.steps.iter().filter_map(|step| step.agent_node_id) {
        if !eligible.contains(&agent_node_id) || !agent_ids.insert(agent_node_id) {
            continue;
        }
        match db::get_agent_node_by_id(agent_node_id) {
            Ok(_) => {
                let Some(claim) = db::claim_circuit_agent_cleanup(agent_node_id).unwrap_or(None)
                else {
                    continue;
                };
                if let Err(error) = archive_failed_circuit_agent(agent_node_id, &claim, |_, _| {}) {
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
    }
    release_run_evaluators(view);
}

/// Load this run's committed steps into the stepper's view shape.
fn retry_failed_cleanup(app: &AppHandle) {
    static LAST_SWEEP: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    let now = chrono::Utc::now().timestamp();
    if now.saturating_sub(LAST_SWEEP.load(Ordering::Relaxed)) < 30 {
        return;
    }
    LAST_SWEEP.store(now, Ordering::Relaxed);
    match db::list_failed_circuit_agents_for_cleanup() {
        Ok(ids) => {
            for id in ids {
                let Some(claim) = db::claim_circuit_agent_cleanup(id).unwrap_or(None) else {
                    continue;
                };
                if let Err(error) = archive_failed_circuit_agent(id, &claim, |run_id, state| {
                    let _ = app.emit(
                        "circuit-run-updated",
                        CircuitRunUpdatedPayload { run_id, state },
                    );
                }) {
                    tracing::warn!("circuits: retrying failed cleanup for agent {id}: {error}");
                }
            }
        }
        Err(error) => tracing::warn!("circuits: cleanup retry query failed: {error}"),
    }
    if let Err(error) = db::clear_finished_circuit_cleanup() {
        tracing::warn!("circuits: could not settle cleanup ledger: {error}");
    }
}

/// A failed attempt is a recovery checkpoint: stop its process, but retain
/// the harness identity, worktree and step association for Archive/Resume.
fn archive_failed_circuit_agent(
    node_id: i64,
    claim: &str,
    on_archived: impl Fn(i64, String),
) -> Result<(), String> {
    if db::claim_circuit_agent_cleanup(node_id)
        .ok()
        .flatten()
        .as_deref()
        != Some(claim)
    {
        return Ok(());
    }
    let result = archive_failed_circuit_agent_with(
        node_id,
        || {
            let owned = db::renew_circuit_agent_cleanup(node_id, claim)
                .map_err(|error| error.to_string())?;
            if !owned {
                return Err("cleanup lease was lost before process termination".into());
            }
            crate::agent::process::kill_agent_blocking(node_id)
        },
        || db::archive_circuit_agent(node_id, claim),
        on_archived,
        |error| {
            // A failed external kill or archive transaction must surrender
            // the lease immediately. The cleanup request remains durable, so
            // the next sweep can retry; a transient OS/SQLite failure must
            // never brick the node behind a permanent claim.
            if let Err(release_error) = db::release_circuit_agent_cleanup(node_id, claim) {
                tracing::warn!(
                    "circuits: could not release failed cleanup lease for agent {} after {}: {}",
                    node_id,
                    error,
                    release_error
                );
            }
        },
    );
    result
}

fn archive_failed_circuit_agent_with<K, A, F>(
    node_id: i64,
    kill: K,
    archive: A,
    on_archived: impl Fn(i64, String),
    on_failure: F,
) -> Result<(), String>
where
    K: FnOnce() -> Result<(), String>,
    A: FnOnce() -> rusqlite::Result<Vec<(i64, String)>>,
    F: FnOnce(&str),
{
    let result: Result<(), String> = (|| {
        kill()?;
        let runs = archive().map_err(|e| e.to_string())?;
        crate::circuit::evaluator::unregister(node_id);
        for (run_id, state) in runs {
            on_archived(run_id, state);
        }
        Ok(())
    })();
    if let Err(error) = &result {
        on_failure(error);
    }
    result
}

fn load_steps(run_id: i64) -> Result<Vec<StepView>, String> {
    let rows = db::list_circuit_run_steps(run_id).map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|row| StepView {
            node_id: row.node_id,
            status: StepStatus::from_db_str(&row.status),
            outcome: row
                .outcome
                .as_deref()
                .and_then(GraphStepOutcome::from_db_str),
            error: row.error_message,
            agent_node_id: row.agent_node_id,
            attempt: row.attempt,
        })
        .collect())
}

/// Persist the assistant revision that existed immediately before a prompt
/// effect is delivered. This is an observation in the impure seam, but its
/// context update is committed together with the step transition before any
/// external effect runs. New spawns do not need a baseline: their launch path
/// records a live turn boundary after the process is attached.
pub(super) fn prepare_turn_boundaries(
    view: &mut RunView,
    effects: &[crate::circuit::stepper::Effect],
) -> Result<bool, String> {
    let mut seen = HashSet::new();
    let mut changed = false;
    for effect in effects {
        let agent_node_id = match effect {
            crate::circuit::stepper::Effect::InjectPty { node_id, .. } => view
                .resolve_target_agent(node_id)
                .filter(|id| crate::agent::process::PROCESS_REGISTRY.is_alive(id)),
            crate::circuit::stepper::Effect::ContinueAgentTurn {
                target_agent_id, ..
            } => crate::agent::process::PROCESS_REGISTRY
                .is_alive(target_agent_id)
                .then_some(*target_agent_id),
            crate::circuit::stepper::Effect::SpawnAgentNode { node_id } => view
                .step(node_id)
                .and_then(|step| step.agent_node_id)
                .filter(|id| crate::agent::process::PROCESS_REGISTRY.is_alive(id)),
            _ => None,
        };
        let Some(agent_node_id) = agent_node_id else {
            continue;
        };
        if !seen.insert(agent_node_id) {
            continue;
        }
        let agent = db::get_agent_node_by_id(agent_node_id).map_err(|error| error.to_string())?;
        let continuation_node = effects.iter().find_map(|effect| match effect {
            crate::circuit::stepper::Effect::ContinueAgentTurn {
                node_id,
                target_agent_id,
                ..
            } if *target_agent_id == agent_node_id => Some(node_id),
            _ => None,
        });
        let revision = continuation_node
            .and_then(|id| {
                view.context
                    .get(&format!("node.{id}.continuation.revision"))
            })
            .map(str::to_string)
            .unwrap_or_else(|| {
                crate::coordinator::enrichment::assistant_report(&agent)
                    .map(|report| report.revision)
                    .unwrap_or_default()
            });
        let key = format!("agent.{agent_node_id}.previous_report_revision");
        if view.context.get(&key) != Some(revision.as_str()) {
            view.context.set(&key, revision);
            changed = true;
        }
    }
    Ok(changed)
}

/// Run a DeterministicVerification command in the mesh directory and
/// report green (exit 0) / red. Bounded wait (2 minutes), then kill and
/// call it red — a hung check must not wedge the worker thread.
fn run_verification_command(
    mesh_path: &str,
    command: &str,
    cancelled: &AtomicBool,
    run_cancelled: &AtomicBool,
) -> bool {
    if cancelled.load(Ordering::Acquire) || run_cancelled.load(Ordering::Acquire) {
        return false;
    }
    let mut cmd = crate::process_util::command_no_window(if cfg!(windows) { "cmd" } else { "sh" });
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // This is shell source, not an argv value. Rust's ordinary escaping
        // can turn nested PowerShell quotes into a successful string literal.
        // /S removes exactly the outer quotes while preserving the source.
        cmd.args(["/D", "/S", "/C"])
            .raw_arg(format!("\"{command}\""));
    }
    #[cfg(not(windows))]
    cmd.args(["-c", command]);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
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
    let job = crate::process_util::JobHandle::contain(child.id());
    let terminate = |child: &mut std::process::Child| {
        if let Some(job) = &job {
            job.terminate();
        }
        crate::process_util::kill_process_tree(child.id());
        #[cfg(unix)]
        {
            let group = format!("-{}", child.id());
            let _ = crate::process_util::command_no_window("kill")
                .args(["-KILL", &group])
                .status();
        }
        let _ = child.kill();
        let _ = child.wait();
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        if cancelled.load(Ordering::Acquire) || run_cancelled.load(Ordering::Acquire) {
            terminate(&mut child);
            return false;
        }
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    tracing::warn!("circuits: verification '{command}' timed out after 120s");
                    terminate(&mut child);
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Err(e) => {
                tracing::warn!("circuits: verification '{command}' wait failed: {}", e);
                terminate(&mut child);
                return false;
            }
        }
    }
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
fn continuation_is_current(
    view: &RunView,
    node_id: &str,
    target: i64,
    status: SessionStatus,
    alive: bool,
    stamp: Option<&str>,
    revision: Option<&str>,
) -> bool {
    continuation_is_current_for(
        view,
        node_id,
        target,
        status,
        alive,
        stamp,
        revision,
        "continuation",
    )
}

/// The same fence, parameterised by the run-context key prefix that owns the
/// delivery. A wake-up keeps its own `nudge.*` bookkeeping, so it must compare
/// against the stamp and revision its own claim recorded — reading the
/// continuation's keys would make every nudge look obsolete the moment the two
/// lifecycles diverged.
fn continuation_is_current_for(
    view: &RunView,
    node_id: &str,
    target: i64,
    status: SessionStatus,
    alive: bool,
    stamp: Option<&str>,
    revision: Option<&str>,
    prefix: &str,
) -> bool {
    alive
        && matches!(
            status,
            SessionStatus::Ready | SessionStatus::Completed | SessionStatus::AwaitingInput
        )
        && view.context.source_agent_id() != Some(target)
        && view.continuation_target(node_id) == Some(target)
        && stamp.is_some()
        && stamp == view.context.get(&format!("node.{node_id}.{prefix}.stamp"))
        && revision.is_some()
        && revision
            == view
                .context
                .get(&format!("node.{node_id}.{prefix}.revision"))
}

/// Dispatch one `CallGithub` effect. App-free: the recheck lookup and the
/// idempotent create path use only the DB and the GitHub client, never the
/// Tauri handle (which unit tests cannot construct — the codebase keeps
/// `AppHandle` concrete everywhere). `execute_effects` delegates here, so
/// the deterministic coverage drives the worker's real dispatch wiring
/// against a controllable endpoint; production passes `None` for the live
/// client.
pub(super) fn execute_call_github_effect(
    active: &db::ActiveCircuitRun,
    view: &mut RunView,
    node_id: &str,
    action: crate::circuit::model::GithubActionKind,
    label: Option<&str>,
    comment: Option<&str>,
    github_client: Option<&crate::services::github::GitHubClient>,
) -> Result<Vec<CircuitEvent>, String> {
    use crate::circuit::model::GithubActionKind;
    let attempt = view.step(node_id).map_or(1, |s| s.attempt);
    if view.context.get(&format!("node.{node_id}.recheck_only")) == Some("1") {
        if action == GithubActionKind::OpenPr {
            let event = match github_client {
                Some(client) => github::reconcile_open_pr_effect_for_worker_with_client(
                    active, view, node_id, client,
                ),
                None => github::reconcile_open_pr_effect_for_worker(active, view, node_id),
            };
            return Ok(vec![event]);
        }
        view.context
            .set(&format!("node.{node_id}.recheck_only"), "0");
        return Ok(vec![CircuitEvent::EffectUncertain {
            node_id: node_id.to_string(),
            attempt,
            reason: "Read-only external-action recheck is unavailable for this GitHub action."
                .into(),
        }]);
    }
    let intent = db::circuit::evidence::EffectIntent {
        node_id: node_id.to_string(),
        attempt,
        kind: db::circuit::evidence::EffectKind::Github,
    };
    let Some(revision) =
        db::circuit::evidence::claim_effect(view.run_id, &intent).map_err(|e| e.to_string())?
    else {
        return Ok(Vec::new());
    };
    view.context.set("evidence.revision", revision.to_string());
    Ok(vec![github::call_github_effect(
        active, view, node_id, action, label, comment,
    )
    .unwrap_or_else(|reason| CircuitEvent::EffectUncertain {
        node_id: node_id.to_string(),
        attempt,
        reason,
    })])
}

/// Apply the worker's dispatch gates to one effect and, while open, dispatch
/// a `CallGithub` effect through [`execute_call_github_effect`]. App-free.
/// `execute_effects` delegates its `CallGithub` arm here — re-checking the
/// same predicates the loop just evaluated, the way the InjectPty arms
/// re-check the batch mid-effect — so the deterministic coverage drives the
/// identical gate-then-dispatch composition against a controllable endpoint
/// instead of reproducing it.
pub(super) fn gated_dispatch_call_github(
    batch: &CircuitEffectBatchPermit,
    run_state: Option<&str>,
    completing_transition: bool,
    active: &db::ActiveCircuitRun,
    view: &mut RunView,
    effect: &crate::circuit::stepper::Effect,
    github_client: Option<&crate::services::github::GitHubClient>,
) -> Result<Vec<CircuitEvent>, String> {
    use crate::circuit::stepper::Effect;
    if batch.is_cancelled() {
        return Ok(Vec::new());
    }
    if !run_state.is_some_and(|state| effect_allowed_in_state(state, completing_transition, effect))
    {
        return Ok(Vec::new());
    }
    match effect {
        Effect::CallGithub {
            node_id,
            action,
            label,
            comment,
        } => execute_call_github_effect(
            active,
            view,
            node_id,
            *action,
            label.as_deref(),
            comment.as_deref(),
            github_client,
        ),
        _ => Ok(Vec::new()),
    }
}

/// Drive one GitHub effect through the full worker composition — batch
/// permit, durable run-state snapshot, gates, dispatch — without the Tauri
/// handle `execute_effects` requires. Test entry point for the deterministic
/// cancellation-ordering coverage; production reaches the same gated dispatch
/// through the loop above. Returns the outcome events plus whether the
/// batch observed cancellation while in flight (the ordering evidence a
/// held lookup needs).
#[cfg(test)]
pub(super) fn run_github_effect_pass(
    active: &db::ActiveCircuitRun,
    view: &mut RunView,
    effect: &crate::circuit::stepper::Effect,
    github_client: Option<&crate::services::github::GitHubClient>,
) -> Result<(Vec<CircuitEvent>, bool), String> {
    let batch = begin_circuit_effect_batch(active.run.id);
    let run_state = db::get_circuit_run(active.run.id)
        .map_err(|e| e.to_string())?
        .map(|run| run.state);
    let completing = matches!(view.state, RunState::Completed | RunState::Failed);
    let outcomes = gated_dispatch_call_github(
        &batch,
        run_state.as_deref(),
        completing,
        active,
        view,
        effect,
        github_client,
    )?;
    Ok((outcomes, batch.is_cancelled()))
}

pub(super) fn execute_effects(
    app: &AppHandle,
    active: &db::ActiveCircuitRun,
    view: &mut RunView,
    effects: &[crate::circuit::stepper::Effect],
) -> Result<Vec<CircuitEvent>, String> {
    use crate::circuit::stepper::Effect;
    let mut outcome_events = Vec::new();
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
            return Ok(outcome_events);
        }
        let accepts_effect = run_state.as_deref().is_some_and(|state| {
            effect_allowed_in_state(
                state,
                matches!(view.state, RunState::Completed | RunState::Failed),
                effect,
            )
        });
        if !accepts_effect {
            tracing::info!(
                "circuits: stopped effects for terminal/deleted run {}",
                active.run.id
            );
            return Ok(outcome_events);
        }
        match effect {
            Effect::SpawnAgentNode { node_id } => {
                let attempt = view.step(node_id).map_or(1, |s| s.attempt);
                let intent = db::circuit::evidence::EffectIntent {
                    node_id: node_id.clone(),
                    attempt,
                    kind: db::circuit::evidence::EffectKind::Spawn,
                };
                let Some(revision) = db::circuit::evidence::claim_effect(active.run.id, &intent)
                    .map_err(|e| e.to_string())?
                else {
                    continue;
                };
                view.context.set("evidence.revision", revision.to_string());
                if let Err(error) =
                    spawn::spawn_step_agent(app, active.run.id, active.run.mesh_id, view, node_id)
                {
                    outcome_events.push(CircuitEvent::EffectUncertain { node_id: node_id.clone(), attempt,
                        reason: format!("Agent dispatch is unverified: {error}. Inspect retained agents before recording an outcome.") });
                }
            }
            Effect::InjectPty {
                node_id, prompt, ..
            } => {
                let requests_result = report_contract::requests_result(view, node_id, prompt);
                let prompt = report_contract::prompt(view, node_id, prompt);
                let attempt = view.step(node_id).map_or(1, |s| s.attempt);
                let intent = db::circuit::evidence::EffectIntent {
                    node_id: node_id.clone(),
                    attempt,
                    kind: db::circuit::evidence::EffectKind::Prompt,
                };
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
                        let Some(target_node) = db::get_agent_node_by_id(target)
                            .ok()
                            .filter(|n| n.status != SessionStatus::Archived)
                        else {
                            let reason = format!(
                                "target agent {} for step {} was lost before prompt injection",
                                target, node_id
                            );
                            tracing::warn!("circuits: run {}: {}", active.run.id, reason);
                            return Err(reason);
                        };
                        if effect_batch.is_cancelled()
                            || db::get_circuit_run(active.run.id)
                                .ok()
                                .flatten()
                                .is_none_or(|r| r.state != "running")
                        {
                            return Ok(outcome_events);
                        }
                        let Some(revision) =
                            db::circuit::evidence::claim_effect(active.run.id, &intent)
                                .map_err(|e| e.to_string())?
                        else {
                            continue;
                        };
                        view.context.set("evidence.revision", revision.to_string());
                        // The full prompt goes to its handoff file; the terminal
                        // receives either the same text or a pointer to it. The
                        // digest and the PTY write both use what is delivered.
                        let staged = crate::circuit::handoff::stage_prompt(
                            active.run.id,
                            node_id,
                            attempt,
                            target_node.env,
                            &prompt,
                            requests_result,
                        );
                        let delivered_prompt = staged.delivered;
                        crate::circuit::handoff::set_agent_turn(
                            active.run.id,
                            target,
                            staged.turn.as_ref(),
                        );
                        // Record the submission before the PTY write. Claude
                        // echoes the prompt back within milliseconds of Enter,
                        // and that echo only proves *this* submission if the
                        // record already exists when the hook is handled
                        // (issue #1898).
                        match db::circuit::evidence::record_prompt_submission(
                            active.run.id,
                            node_id,
                            attempt,
                            target,
                            &delivered_prompt,
                        ) {
                            Ok(revision) => {
                                view.context.set("evidence.revision", revision.to_string())
                            }
                            Err(error) => {
                                tracing::warn!(
                                    "circuits: run {}: could not record prompt submission: {}",
                                    active.run.id,
                                    error
                                );
                                outcome_events.push(CircuitEvent::EffectUncertain { node_id: node_id.clone(), attempt, reason: format!("Prompt submission could not be recorded for correlation: {error}") });
                                continue;
                            }
                        }
                        crate::circuit::evaluator::note_turn_start(target);
                        let delivered = dispatch_prompt_and_acknowledge(
                            || {
                                let input = crate::agent::process::PROCESS_REGISTRY
                                    .input_stamp(target)
                                    .ok_or(
                                        "Terminal input is already owned; prompt was not sent",
                                    )?;
                                crate::circuit::delivery::write_prompt_to_pty_guarded(
                                    &crate::agent::process::PROCESS_REGISTRY,
                                    target,
                                    &delivered_prompt,
                                    app,
                                    Some(&input),
                                )
                            },
                            || {
                                db::circuit::evidence::acknowledge_prompt_delivery(active.run.id, node_id, attempt)
                                .map_err(|error| format!("Prompt was submitted but acknowledgement could not be recorded: {error}"))
                            },
                        );
                        match delivered {
                            Ok(Some(revision)) => {
                                view.context.set("evidence.revision", revision.to_string())
                            }
                            Ok(None) => continue,
                            Err(error) => {
                                outcome_events.push(CircuitEvent::EffectUncertain {
                                    node_id: node_id.clone(),
                                    attempt,
                                    reason: format!("Prompt delivery is unverified: {error}"),
                                });
                                continue;
                            }
                        }
                        let _ = db::update_agent_node_status(target, SessionStatus::Running);
                        outcome_events.push(CircuitEvent::PromptDelivered {
                            node_id: node_id.clone(),
                            attempt,
                        });
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
            Effect::ContinueAgentTurn {
                node_id,
                target_agent_id,
                prompt,
            } => {
                let key = format!("node.{node_id}.continuation.delivery");
                if view.context.get(&key) != Some("claimed") {
                    continue;
                }
                let node = db::get_agent_node_by_id(*target_agent_id).map_err(|e| e.to_string())?;
                let stamp = db::agent_turn_stamp(*target_agent_id).map_err(|e| e.to_string())?;
                let revision =
                    crate::coordinator::enrichment::assistant_report(&node).map(|r| r.revision);
                let valid = continuation_is_current(
                    view,
                    node_id,
                    *target_agent_id,
                    node.status,
                    crate::agent::process::PROCESS_REGISTRY.is_alive(target_agent_id),
                    stamp.as_deref(),
                    revision.as_deref(),
                );
                if !valid {
                    outcome_events.push(CircuitEvent::ContinuationObsolete {
                        node_id: node_id.clone(),
                        attempt: view
                            .step(node_id)
                            .map(|step| step.attempt)
                            .unwrap_or_default(),
                    });
                    continue;
                }
                if effect_batch.is_cancelled()
                    || db::get_circuit_run(active.run.id)
                        .ok()
                        .flatten()
                        .is_none_or(|r| r.state != "running")
                {
                    return Ok(outcome_events);
                }
                let expected = view
                    .context
                    .get(&format!("node.{node_id}.continuation.input"))
                    .ok_or_else(|| "Continuation lacks an input ownership stamp".to_string())?;
                // The agent may owe a result file from an earlier turn. Archive it so the
                // continuation cannot be judged from that stale report.
                let prompt = crate::circuit::handoff::prepare_continuation(
                    active.run.id,
                    *target_agent_id,
                    node.env,
                    prompt,
                );
                match crate::circuit::delivery::write_prompt_to_pty_guarded(
                    &crate::agent::process::PROCESS_REGISTRY,
                    *target_agent_id,
                    &prompt,
                    app,
                    Some(expected),
                ) {
                    Ok(true) => {
                        let _ =
                            db::update_agent_node_status(*target_agent_id, SessionStatus::Running);
                        outcome_events.push(CircuitEvent::ContinuationDelivered {
                            node_id: node_id.clone(),
                            attempt: view
                                .step(node_id)
                                .map(|step| step.attempt)
                                .unwrap_or_default(),
                        });
                    }
                    Ok(false) => outcome_events.push(CircuitEvent::ContinuationObsolete {
                        node_id: node_id.clone(),
                        attempt: view
                            .step(node_id)
                            .map(|step| step.attempt)
                            .unwrap_or_default(),
                    }),
                    Err(error) => {
                        crate::commands::attention::mark_attention(*target_agent_id, app);
                        outcome_events.push(CircuitEvent::ContinuationUncertain {
                            node_id: node_id.clone(),
                            attempt: view
                                .step(node_id)
                                .map(|step| step.attempt)
                                .unwrap_or_default(),
                            error,
                        });
                    }
                }
            }
            Effect::NudgeIdleAgent {
                node_id,
                target_agent_id,
                prompt,
            } => {
                let key = format!("node.{node_id}.nudge.delivery");
                if view.context.get(&key) != Some("claimed") {
                    continue;
                }
                let node = db::get_agent_node_by_id(*target_agent_id).map_err(|e| e.to_string())?;
                let stamp = db::agent_turn_stamp(*target_agent_id).map_err(|e| e.to_string())?;
                let revision =
                    crate::coordinator::enrichment::assistant_report(&node).map(|r| r.revision);
                let attempt = view
                    .step(node_id)
                    .map(|step| step.attempt)
                    .unwrap_or_default();
                // The wake-up is only worth sending to the turn that was
                // observed as stalled. Reuse the continuation fence, bound to
                // this nudge's own claim keys: a turn that has since moved on
                // needs nothing.
                let valid = continuation_is_current_for(
                    view,
                    node_id,
                    *target_agent_id,
                    node.status,
                    crate::agent::process::PROCESS_REGISTRY.is_alive(target_agent_id),
                    stamp.as_deref(),
                    revision.as_deref(),
                    "nudge",
                );
                if !valid {
                    outcome_events.push(CircuitEvent::NudgeObsolete {
                        node_id: node_id.clone(),
                        attempt,
                    });
                    continue;
                }
                if effect_batch.is_cancelled()
                    || db::get_circuit_run(active.run.id)
                        .ok()
                        .flatten()
                        .is_none_or(|r| r.state != "running")
                {
                    return Ok(outcome_events);
                }
                let expected = view
                    .context
                    .get(&format!("node.{node_id}.nudge.input"))
                    .ok_or_else(|| "Wake-up lacks an input ownership stamp".to_string())?;
                // The nudge is a real prompt submission, so it is recorded the
                // same way an injected one is: the receipt it may produce has to
                // bind to a submission Buildmesh itself made.
                if let Err(error) = db::circuit::evidence::record_prompt_submission(
                    active.run.id,
                    node_id,
                    attempt,
                    *target_agent_id,
                    prompt,
                ) {
                    tracing::warn!(
                        "circuits: run {}: could not record wake-up submission: {}",
                        active.run.id,
                        error
                    );
                    outcome_events.push(CircuitEvent::NudgeUncertain {
                        node_id: node_id.clone(),
                        attempt,
                        error: format!(
                            "Wake-up submission could not be recorded for correlation: {error}"
                        ),
                    });
                    continue;
                }
                match crate::circuit::delivery::write_prompt_to_pty_guarded(
                    &crate::agent::process::PROCESS_REGISTRY,
                    *target_agent_id,
                    prompt,
                    app,
                    Some(expected),
                ) {
                    Ok(true) => {
                        let _ =
                            db::update_agent_node_status(*target_agent_id, SessionStatus::Running);
                        outcome_events.push(CircuitEvent::NudgeDelivered {
                            node_id: node_id.clone(),
                            attempt,
                        });
                    }
                    Ok(false) => outcome_events.push(CircuitEvent::NudgeObsolete {
                        node_id: node_id.clone(),
                        attempt,
                    }),
                    Err(error) => {
                        crate::commands::attention::mark_attention(*target_agent_id, app);
                        outcome_events.push(CircuitEvent::NudgeUncertain {
                            node_id: node_id.clone(),
                            attempt,
                            error,
                        });
                    }
                }
            }
            Effect::SetNodeStatus { .. } => {
                // persist_transition commits this local SQLite mutation with
                // the completed step, closing the crash gap before dispatch.
            }
            Effect::CloseAgentNode {
                node_id,
                target_node_id,
            } => {
                let target = view.resolve_target_agent(node_id).ok_or_else(|| {
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
                        // An agent no step identifies as a helper is treated as
                        // the user's work, the safer reading. A failed DB read
                        // here is the safer reading too: leave the agent open
                        // and let the next sweep retry.
                        let is_helper = match db::circuit::agent_is_circuit_helper(target) {
                            Ok(value) => value,
                            Err(error) => {
                                tracing::warn!(
                                    "circuits: run {} could not read helper status for agent {}: {}; leaving the node open",
                                    active.run.id,
                                    target,
                                    error
                                );
                                false
                            }
                        };
                        // Helpers are always closed without inspecting their
                        // worktree. For the implementation agent, a failed
                        // worktree inspection is reported as having changes so
                        // the node stays open until a person looks at it.
                        let mut safety = crate::git::worktree::WorktreeCloseSafety {
                            worktree_path: None,
                            has_uncommitted: false,
                            has_unpushed: false,
                            is_detached: false,
                        };
                        if !is_helper {
                            match crate::services::agent_node::get_worktree_close_safety(target) {
                                Ok(s) => safety = s,
                                Err(error) => {
                                    tracing::warn!(
                                        "circuits: run {} could not inspect the worktree of agent {}: {}; leaving the node open",
                                        active.run.id,
                                        target,
                                        error
                                    );
                                    safety.has_uncommitted = true;
                                }
                            }
                        }
                        if let Some(reason) = close_blocker(is_helper, &safety) {
                            tracing::warn!(
                                "circuits: run {} left agent {} open instead of closing it: {}",
                                active.run.id,
                                target,
                                reason
                            );
                            let _ = app.emit(
                                "circuit-notification",
                                CircuitNotificationPayload {
                                    run_id: active.run.id,
                                    message: format!(
                                        "The implementation agent was left open instead of being closed: {reason}."
                                    ),
                                    severity: "warning".into(),
                                },
                            );
                        } else {
                            crate::services::agent_node::delete(target, true)
                                .map_err(|e| format!("agent node close failed: {}", e))?;
                        }
                    }
                    Err(rusqlite::Error::QueryReturnedNoRows) => {}
                    Err(e) => {
                        return Err(format!("agent node lookup failed during close: {}", e));
                    }
                }
                let spawn_step_id =
                    close_target_spawn_step_id(view, node_id, target_node_id.as_deref(), target);
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
            Effect::CallGithub {
                node_id, action, ..
            } => {
                let outcomes = gated_dispatch_call_github(
                    &effect_batch,
                    run_state.as_deref(),
                    matches!(view.state, RunState::Completed | RunState::Failed),
                    active,
                    view,
                    effect,
                    None,
                )?;
                let pr_is_available = *action == crate::circuit::model::GithubActionKind::OpenPr
                    && outcomes.iter().any(|event| {
                        matches!(
                            event,
                            CircuitEvent::GithubActionResult {
                                success: true,
                                pr_number: Some(_),
                                ..
                            }
                        )
                    });
                if pr_is_available {
                    if let Some(agent_node_id) = view.resolve_target_agent(node_id) {
                        let _ = app.emit(
                            "circuit-pr-ready",
                            CircuitPrReadyPayload {
                                run_id: active.run.id,
                                node_id: agent_node_id,
                            },
                        );
                    }
                }
                outcome_events.extend(outcomes);
            }
        }
    }
    Ok(outcome_events)
}

/// Cancellation commits the terminal run state before retiring external
/// resources. The transition's effects take one durable-state snapshot before
/// execution and check an in-memory cancellation token before each effect;
/// terminal transitions retain only their synchronous cleanup and notification
/// effects, while InjectPty is never allowed after completion.
fn effect_allowed_in_state(
    state: &str,
    completing_transition: bool,
    effect: &crate::circuit::stepper::Effect,
) -> bool {
    use crate::circuit::stepper::Effect;
    RunState::is_live_db_str(state)
        // Synchronous terminal actions are emitted by the same transition
        // that completes the run, and must survive its commit-before-effects.
        // InjectPty is intentionally absent: it starts new work after the
        // run has durably finished and would leave an untracked command.
        // A DB state of "cancelled" blocks everything even when the
        // in-memory transition is completing: cancellation wins over a
        // concurrently-computed completion, whose Notify would otherwise
        // fire about a run the user just cancelled. Cancellation cleanup
        // runs through the lease/retire path, not stepper effects.
        || (matches!(state, s if s == RunState::Completed.as_db_str() || s == RunState::Failed.as_db_str()) && completing_transition && matches!(effect,
            Effect::Notify { .. } | Effect::SetNodeStatus { .. } | Effect::CloseAgentNode { .. }))
}

pub(super) fn run_accepts_effects(run_id: i64) -> Result<bool, String> {
    Ok(matches!(
        db::get_circuit_run(run_id).map_err(|error| error.to_string())?,
        Some(run) if RunState::is_live_db_str(&run.state)
    ))
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
        Some(crate::circuit::stepper::RunState::Failed.as_db_str()),
        None,
        &[db::CircuitStepOp {
            node_id: node_id.to_string(),
            status: status.to_string(),
            outcome: Some(Some(status.to_string())),
            error: Some(Some(error.to_string())),
            agent_node_id: None,
            attempt: 1,
            fresh_attempt: false,
        }],
    )
    .map_err(|e| e.to_string())?;
    let _ = app.emit(
        "circuit-run-updated",
        CircuitRunUpdatedPayload {
            run_id: *run_id,
            state: "failed".to_string(),
        },
    );
    Ok(())
}

fn apply_quiet_classifier_failures(app: &AppHandle, failures: Vec<QuietClassifierFailure>) {
    for failure in failures {
        let permit = begin_circuit_effect_batch(failure.active.run.id);
        failure.target.publish(&permit, &failure.active, || {
            let Ok(node) = db::get_agent_node_by_id(failure.agent) else {
                return;
            };
            if !quiet_turn_is_current(
                &failure.evidence,
                &QuietTurnEvidence {
                    lifecycle: db::agent_turn_stamp(failure.agent).ok().flatten(),
                    input: crate::agent::process::PROCESS_REGISTRY.input_stamp(failure.agent),
                    report: crate::coordinator::enrichment::assistant_report(&node)
                        .map(|report| report.revision),
                },
                crate::agent::process::PROCESS_REGISTRY.is_alive(&failure.agent),
                crate::circuit::evaluator::millis_since_last_output(failure.agent),
                node.status,
            ) {
                return;
            }
            let Some(run) = db::get_circuit_run(failure.active.run.id).ok().flatten() else {
                return;
            };
            let (Ok(graph), Ok(mut context), Ok(steps)) = (
                CircuitGraph::from_json(&failure.active.circuit_graph_json),
                CircuitContext::from_json(&run.context_json),
                load_steps(run.id),
            ) else {
                return;
            };
            let Ok(revision) = db::circuit::evidence::observation_revision(&run) else {
                return;
            };
            context.set("evidence.revision", revision.to_string());
            let mut view = RunView {
                run_id: run.id,
                state: RunState::from_db_str(&run.state),
                graph,
                context,
                steps,
            };
            if !failure.target.matches(&view) || permit.is_cancelled() {
                return;
            }
            let event = CircuitEvent::ClassifierUnavailable {
                node_id: failure.step,
                attempt: failure.attempt,
                error: failure.error,
                observed_at_ms: chrono::Utc::now().timestamp_millis(),
            };
            match advance_and_persist_observed_event(&mut view, &event, |view, transition| {
                persist_transition_checked(run.id, view, transition)
            }) {
                Ok(_) => {
                    let _ = app.emit(
                        "circuit-run-updated",
                        CircuitRunUpdatedPayload {
                            run_id: run.id,
                            state: run.state,
                        },
                    );
                }
                Err(error) => tracing::warn!(
                    "circuits: could not record readiness classifier failure: {}",
                    error.into_message()
                ),
            }
        });
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
    } else if lower.contains("attention") || lower.contains("limit") || lower.contains("failed") {
        "warning".into()
    } else {
        "info".into()
    }
}

#[cfg(test)]
mod tests;
/// A Circuit agent needs human input.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitAgentBlockedPayload.ts")]
pub struct CircuitAgentBlockedPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
    #[ts(as = "i32")]
    pub issue: i64,
}

/// An OpenPr action completed and an open pull request is available for an agent node.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitEvents.ts")]
pub struct CircuitPrReadyPayload {
    #[ts(as = "i32")]
    pub run_id: i64,
    #[ts(as = "i32")]
    pub node_id: i64,
}
