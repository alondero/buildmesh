//! Run admission and capacity observation. Durable admission reservations
//! and live-agent Tick counts intentionally use different occupancy snapshots.
use super::{capacity, db, CircuitEvent, CircuitGraph, RunState};

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
        tracing::info!(
            "circuits: mesh {} held — {} active run(s) >= {} capacity, run {} stays pending",
            active.run.mesh_id,
            active_runs,
            mesh.circuit_run_capacity,
            active.run.id,
        );
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
/// without a Tauri runtime. A circuit's durable lease is the agent budget;
/// the retired per-mesh automation cap is deliberately not read here.
pub(super) fn observe_capacity(
    active: &db::ActiveCircuitRun,
    global_pool: Option<u32>,
) -> CircuitEvent {
    let circuit_running =
        db::count_running_circuit_steps(active.run.circuit_id).unwrap_or_else(|e| {
            tracing::warn!("circuits: running-step count failed, failing closed: {}", e);
            i64::MAX
        });
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
    let reserved_for_run = db::circuit_agent_slots_reserved(active.run.id).unwrap_or(0);
    let owned_by_run = db::count_active_circuit_agent_nodes_for_run(active.run.id).unwrap_or(0);
    observe_capacity_with(
        active.circuit_concurrency_limit,
        CapacityCounts {
            circuit_running,
            reserved_for_run,
            owned_by_run,
            global_free_slots,
        },
    )
}

pub(super) struct CapacityCounts {
    pub circuit_running: i64,
    pub reserved_for_run: i64,
    pub owned_by_run: i64,
    pub global_free_slots: i64,
}

pub(super) fn observe_capacity_with(
    concurrency_limit: i64,
    counts: CapacityCounts,
) -> CircuitEvent {
    CircuitEvent::Tick(capacity::tick_capacity(
        concurrency_limit,
        counts.circuit_running,
        counts.reserved_for_run,
        counts.owned_by_run,
        counts.global_free_slots,
    ))
}
