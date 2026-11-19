//! Run admission and capacity observation. Durable admission reservations
//! and live-agent Tick counts intentionally use different occupancy snapshots.
use std::collections::HashMap;
use std::sync::Mutex;

use once_cell::sync::Lazy;

use super::{capacity, db, CircuitEvent, CircuitGraph, RunState};

/// The admission hold a run is in, as the log sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct HeldAdmission {
    /// The mesh whose capacity is full.
    pub mesh_id: i64,
    /// Admitted runs counted on that mesh.
    pub active_runs: i64,
    /// The mesh's run capacity.
    pub capacity: i32,
}

/// The hold each run was last logged in.
///
/// A run held for capacity is re-evaluated on every pass, so logging the
/// hold on every pass wrote the same line again and again: on the hub the
/// worker spun at ~90 passes a second and wrote 66 MB of that one line in
/// 37 minutes (issue #2831). The line belongs to a *state change* — the run
/// was first held, or the reason it is held changed — so it is written
/// then and not on every pass that finds the same hold.
static HELD_RUNS: Lazy<Mutex<HashMap<i64, HeldAdmission>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn lock_held_runs() -> std::sync::MutexGuard<'static, HashMap<i64, HeldAdmission>> {
    match HELD_RUNS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Should this hold be logged?
///
/// A hold is logged when it is new for the run or when it changed. The
/// unchanged hold of a run that is still waiting is not logged again.
pub(super) fn should_log_hold(previous: Option<&HeldAdmission>, current: &HeldAdmission) -> bool {
    previous != Some(current)
}

/// Log a run's admission hold, if the hold is new or has changed.
///
/// Returns whether the line was written, so the rule is observable: the
/// unchanged hold of a run that is still waiting is not written again.
pub(super) fn log_hold(run_id: i64, hold: HeldAdmission) -> bool {
    let mut held = lock_held_runs();
    let previous = held.get(&run_id).copied();
    if !should_log_hold(previous.as_ref(), &hold) {
        return false;
    }
    held.insert(run_id, hold);
    drop(held);
    tracing::info!(
        "circuits: mesh {} held — {} active run(s) >= {} capacity, run {} stays pending",
        hold.mesh_id,
        hold.active_runs,
        hold.capacity,
        run_id,
    );
    true
}

/// Forget the hold of a run that is no longer waiting for capacity.
///
/// Without this a run that was held once would keep its entry for ever,
/// and the run's next hold would look unchanged.
pub(super) fn forget_hold(run_id: i64) {
    lock_held_runs().remove(&run_id);
}

/// Forget the holds of runs that are no longer pending.
///
/// The map only needs the runs that are waiting: a run that left the queue
/// must not keep its entry, or its next hold would look unchanged.
pub(super) fn sweep_holds(runs: &[db::ActiveCircuitRun]) {
    let mut held = lock_held_runs();
    held.retain(|run_id, _| {
        runs.iter()
            .any(|run| run.run.id == *run_id && run.run.state == "pending")
    });
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
pub(super) fn may_admit_run(active: &db::ActiveCircuitRun, mesh: &crate::models::Mesh) -> bool {
    let state = RunState::from_db_str(&active.run.state);
    let mut observed_count = None;
    let admitted = may_admit_run_with(state, mesh.circuit_run_capacity, || {
        let active_runs = match db::count_active_circuit_runs(active.run.mesh_id) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    "circuits: per-mesh active-run count failed for gate, failing closed: {}",
                    e
                );
                return None;
            }
        };
        observed_count = Some(active_runs);
        Some(active_runs)
    });
    if let Some(active_runs) = observed_count.filter(|_| !admitted) {
        log_hold(
            active.run.id,
            HeldAdmission {
                mesh_id: active.run.mesh_id,
                active_runs,
                capacity: mesh.circuit_run_capacity,
            },
        );
    } else {
        // The run holds a slot now, or it is not waiting any more: the next
        // hold it meets is a change and is logged again.
        forget_hold(active.run.id);
    }
    admitted
}

pub(super) fn may_admit_run_with(
    state: RunState,
    mesh_capacity: i32,
    count: impl FnOnce() -> Option<i64>,
) -> bool {
    state != RunState::Pending
        || count().is_some_and(|n| capacity::admit_pending_run(n, mesh_capacity))
}

/// Number of agent process slots declared by a blueprint. This conservative
/// reservation is derived from the durable graph, not transient child
/// associations: a completed implementation step can still retain a live
/// process while a reviewer is spawned.
pub(super) fn required_agent_slots(active: &db::ActiveCircuitRun) -> i64 {
    CircuitGraph::from_json(&active.circuit_graph_json)
        .map(|graph| capacity::declared_agent_slots(&graph))
        .unwrap_or(0)
}

pub(super) fn global_agent_reservation_fits(
    required: i64,
    reserved_circuit_slots: i64,
    unleased_slots: i64,
    global_pool: Option<u32>,
) -> bool {
    capacity::global_agent_reservation_fits(
        required,
        reserved_circuit_slots,
        unleased_slots,
        global_pool,
    )
}

/// Observe the capacity available to one running circuit. The worker injects
/// the app-wide pool setting so this seam can exercise the real DB counters
/// without a Tauri runtime. A run's durable lease is the agent budget; the
/// retired per-mesh automation cap and the retired per-circuit step budget
/// (ADR 0042) are deliberately not read here.
pub(super) fn observe_capacity(
    active: &db::ActiveCircuitRun,
    global_pool: Option<u32>,
) -> CircuitEvent {
    let global_free_slots = match global_pool {
        None => i64::MAX,
        Some(pool) => {
            let circuit_total = db::count_active_circuit_agent_nodes_total();
            let retained_total = db::count_retained_circuit_agent_nodes_total();
            match (circuit_total, retained_total) {
                (Ok(circuits), Ok(retained)) => {
                    capacity::global_agent_free_slots(Some(pool), retained, circuits)
                }
                (Err(e), _) | (_, Err(e)) => {
                    tracing::warn!(
                        "circuits: global Circuit agent-pool count failed, failing closed: {}",
                        e
                    );
                    0
                }
            }
        }
    };
    // A failed count is *unknown*, not zero (issue #1533). `owned_by_run = 0`
    // would make the lease look free and let the run spawn past its
    // reservation; `reserved_for_run = 0` alone would under-admit. Fail
    // closed instead: an unknown count admits nothing, and the next tick
    // re-reads the counters.
    let (reserved_for_run, owned_by_run) = match (
        db::circuit_agent_slots_reserved(active.run.id),
        db::count_active_circuit_agent_nodes_for_run(active.run.id),
    ) {
        (Ok(reserved), Ok(owned)) => (reserved, owned),
        (Err(e), _) | (_, Err(e)) => {
            tracing::warn!(
                "circuits: capacity counts for run {} failed, failing closed: {}",
                active.run.id,
                e
            );
            (0, 0)
        }
    };
    observe_capacity_with(CapacityCounts {
        reserved_for_run,
        owned_by_run,
        global_free_slots,
    })
}

pub(super) struct CapacityCounts {
    pub reserved_for_run: i64,
    pub owned_by_run: i64,
    pub global_free_slots: i64,
}

pub(super) fn observe_capacity_with(counts: CapacityCounts) -> CircuitEvent {
    CircuitEvent::Tick(capacity::tick_capacity(
        counts.reserved_for_run,
        counts.owned_by_run,
        counts.global_free_slots,
    ))
}

/// Transfer retained pending agents into a durable run reservation before
/// admission. The caller's totals include earlier admissions in this pass.
pub(super) fn reserve_pending_agents(
    active: &db::ActiveCircuitRun,
    reserved_circuit_slots: &mut i64,
    unleased_slots: &mut i64,
    global_pool: Option<u32>,
) -> bool {
    let required = required_agent_slots(active);
    if required > 0 {
        let retained_by_run = match db::count_unleased_pending_circuit_agent_nodes(active.run.id) {
            Ok(count) => count,
            Err(error) => {
                tracing::warn!(
                    "circuits: retained agent count failed for run {}: {}",
                    active.run.id,
                    error
                );
                return false;
            }
        };
        let other_unleased = unleased_slots.saturating_sub(retained_by_run);
        // A failed reservation read is *unknown*, not zero (issue
        // #1533): `existing = 0` would reserve `required` slots a
        // second time. Skip the reservation for this run; the next
        // pass re-reads the count.
        let existing = match db::circuit_agent_slots_reserved(active.run.id) {
            Ok(existing) => existing,
            Err(e) => {
                tracing::warn!(
                    "circuits: reservation read failed for run {}, skipping the reservation: {}",
                    active.run.id,
                    e
                );
                return false;
            }
        };
        let additional = required.saturating_sub(existing);
        if !global_agent_reservation_fits(
            additional,
            *reserved_circuit_slots,
            other_unleased,
            global_pool,
        ) {
            super::record_queue_wait(
                active.run.id,
                db::circuit::evidence::QueueWaitReason::AgentCapacity {
                    required: additional,
                    available: global_pool.map(|limit| {
                        i64::from(limit)
                            .saturating_sub(*reserved_circuit_slots)
                            .saturating_sub(other_unleased)
                            .max(0)
                    }),
                },
            );
            tracing::info!(
                "circuits: global Circuit agent pool held — run {} needs {} reserved agent slot(s)",
                active.run.id,
                required
            );
            return false;
        }
        if additional > 0 {
            match db::reserve_circuit_agent_slots(active.run.id, required) {
                Ok(true) => {}
                Ok(false) | Err(_) => {
                    super::record_queue_wait(
                        active.run.id,
                        db::circuit::evidence::QueueWaitReason::ReservationUnavailable,
                    );
                    return false;
                }
            }
        }
        *reserved_circuit_slots = reserved_circuit_slots.saturating_add(additional);
        *unleased_slots = other_unleased;
    }
    true
}
