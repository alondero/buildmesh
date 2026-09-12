//! Autopilot Circuits persistence (spec #1205 / walking skeleton #1206):
//! accessors for the three ledger tables added in schema v34.
//!
//! - [`ledger`] — blueprint/run/step CRUD and [`ledger::commit_circuit_advance`].
//! - [`queue`] — queued list / neighbour move / reorder.
//! - [`leases`] — reservations, ownership claims, cleanup, retention.
//!
//! Locking discipline (see `db::mod` hard rules): every public fn takes
//! one reader or writer connection exactly once and never calls another public
//! accessor from inside. [`ledger::commit_circuit_advance`] is the engine's
//! single atomic commit point: run-state, context, and all step writes
//! of one stepper transition land in ONE transaction so a crash can
//! never leave a half-applied decision.

pub mod ledger;
pub mod queue;
pub mod leases;

pub use ledger::*;
pub use queue::*;
pub use leases::*;

pub(crate) use ledger::delete_circuits_for_mesh_inner;

#[cfg(test)]
pub(crate) use leases::{
    archive_circuit_agent_inner,
    circuit_agent_slots_reserved_inner,
    circuit_agent_spawn_claim_inner,
    claim_circuit_agent_cleanup_inner,
    claim_circuit_agent_spawn_inner,
    clear_finished_circuit_cleanup_inner,
    count_active_circuit_agent_nodes_total_inner,
    count_retained_circuit_agent_nodes_total_inner,
    failed_circuit_agents_for_cleanup_inner,
    list_circuit_agent_ownerships_inner,
    prune_terminal_circuit_runs_older_than_inner,
    release_circuit_agent_cleanup_inner,
    release_circuit_agent_spawn_inner,
    reserve_circuit_agent_slots_locked,
};

#[cfg(test)]
pub(crate) use ledger::{
    cancel_circuit_run_locked,
    cancel_circuit_runs_locked,
    commit_circuit_advance_locked,
    count_active_circuit_runs_inner,
    count_running_circuit_steps_inner,
    create_autopilot_circuit_inner,
    create_circuit_run_locked,
    create_node_circuit_run_locked,
    delete_autopilot_circuit_locked,
    get_autopilot_circuit_inner,
    get_circuit_run_inner,
    list_active_circuit_runs_inner,
    list_circuit_run_ids_for_cleanup_inner,
    list_circuit_run_steps_inner,
    list_circuit_runs_inner,
    list_circuits_with_recent_runs_inner,
    list_circuit_trigger_identities_inner,
    list_enabled_circuits_inner,
    list_autopilot_circuits_inner,
    latest_circuit_run_created_at_inner,
    set_autopilot_circuit_enabled_inner,
    set_circuit_run_state_inner,
    set_circuit_step_agent_node_with_parent_inner,
    clear_circuit_step_agent_node_inner,
    transition_circuit_run_state_inner,
    update_autopilot_circuit_graph_inner,
};

#[cfg(test)]
pub(crate) use queue::{
    move_queued_circuit_run_locked,
    move_queued_circuit_run_to_edge_locked,
    reorder_queued_circuit_runs_locked,
};

use crate::db::SqlResult;
use crate::models::{AutopilotCircuit, AutopilotCircuitRun};

type CircuitProbeLedger = Vec<(AutopilotCircuit, Vec<ledger::CircuitRunLedger>)>;
type QueuedCircuitRun = Vec<(AutopilotCircuitRun, String)>;

/// Hydrate the Circuits Probe's ledger and queue from one read connection so
/// both views observe the same database snapshot.
pub fn list_circuit_probe(
    mesh_id: i64,
    runs_per_circuit: i64,
) -> SqlResult<(CircuitProbeLedger, QueuedCircuitRun)> {
    let db = crate::db::read_conn();
    let circuits = ledger::list_circuits_with_recent_runs_inner(&db, mesh_id, runs_per_circuit)?;
    let queued = queue::list_queued_circuit_runs_inner(&db, mesh_id)?;
    Ok((circuits, queued))
}
