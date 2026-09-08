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
    circuit_agent_spawn_claim_inner,
    claim_circuit_agent_cleanup_inner,
    claim_circuit_agent_spawn_inner,
    clear_finished_circuit_cleanup_inner,
    failed_circuit_agents_for_cleanup_inner,
    prune_terminal_circuit_runs_older_than_inner,
    release_circuit_agent_cleanup_inner,
    release_circuit_agent_spawn_inner,
};

use crate::db::SqlResult;
use crate::models::{AutopilotCircuit, AutopilotCircuitRun};

/// Hydrate the Circuits Probe's ledger and queue from one read connection so
/// both views observe the same database snapshot.
pub fn list_circuit_probe(
    mesh_id: i64,
    runs_per_circuit: i64,
) -> SqlResult<(
    Vec<(AutopilotCircuit, Vec<ledger::CircuitRunLedger>)>,
    Vec<(AutopilotCircuitRun, String)>,
)> {
    let db = crate::db::read_conn();
    let circuits = ledger::list_circuits_with_recent_runs_inner(&db, mesh_id, runs_per_circuit)?;
    let queued = queue::list_queued_circuit_runs_inner(&db, mesh_id)?;
    Ok((circuits, queued))
}
