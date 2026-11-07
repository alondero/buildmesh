//! Reads of the Circuit catalog: the circuits on a mesh, their run ledgers,
//! the admission queue and the agent-ownership ledger.
//!
//! These are the operations a second boundary needs — the mobile routes and
//! the companion CLI — so they live here rather than in the desktop command
//! adapter (issue #2673). The wire shapes stay declared in
//! `commands/circuit.rs` (ADR 0047); this module only fills them in.

use crate::db::DbError;
use crate::models::AutopilotCircuit;

use crate::commands::circuit::{
    CircuitAgentOwnership, CircuitProbeSnapshot, CircuitQueueEntry, CircuitRunDetail,
    CircuitWithRuns,
};

/// The circuits on a mesh, the built-in review blueprint included.
pub fn list_circuits(mesh_id: i64) -> Result<Vec<AutopilotCircuit>, DbError> {
    crate::db::list_autopilot_circuits(mesh_id).map_err(DbError::from)
}

/// The circuits on a mesh with their most recent run ledgers.
pub fn list_circuits_with_runs(
    mesh_id: i64,
    limit: Option<i64>,
) -> Result<Vec<CircuitWithRuns>, DbError> {
    let limit = limit.unwrap_or(10).clamp(1, 100);
    crate::db::list_circuits_with_recent_runs(mesh_id, limit).map(map_circuit_rows)
}

/// The mesh-wide admission queue: the pending runs in worker-admission order.
pub fn list_queue(mesh_id: i64) -> Result<Vec<CircuitQueueEntry>, DbError> {
    crate::db::list_queued_circuit_runs(mesh_id)
        .map(map_queue_rows)
        .map_err(DbError::from)
}

/// The Probe tab's single-IPC hydration payload: the ledger, the queue and
/// the latest worker admission observation for each pending run.
pub fn list_probe(mesh_id: i64, limit: Option<i64>) -> Result<CircuitProbeSnapshot, DbError> {
    let limit = limit.unwrap_or(10).clamp(1, 100);
    let (circuits, queue) = crate::db::list_circuit_probe(mesh_id, limit)?;
    let queue_waits = crate::db::circuit::evidence::queue_waits(mesh_id).map_err(DbError::from)?;
    Ok(CircuitProbeSnapshot {
        circuits: map_circuit_rows(circuits),
        queue: map_queue_rows(queue),
        queue_waits,
    })
}

/// Recent runs for one circuit, newest first, each with its step ledger.
pub fn list_runs(circuit_id: i64, limit: Option<i64>) -> Result<Vec<CircuitRunDetail>, DbError> {
    let limit = limit.unwrap_or(20).clamp(1, 100);
    let ledgers = crate::db::circuit::ledger::list_circuit_run_ledgers(circuit_id, limit)?;
    Ok(ledgers
        .into_iter()
        .map(|ledger| CircuitRunDetail {
            run: ledger.run,
            steps: ledger.steps,
            review_round_limit: ledger.review_round_limit,
            review_continuation: Some(ledger.review_continuation),
        })
        .collect())
}

/// The ownership ledger: which visible Agent Node a Circuit Run owns.
///
/// The failure stays typed: the mobile route needs to know whether the read
/// failed because the database was momentarily busy (a `503` it can retry) or
/// because the query is broken, and text cannot answer that (issue #1533).
pub fn list_agent_ownerships() -> Result<Vec<CircuitAgentOwnership>, DbError> {
    crate::db::list_circuit_agent_ownerships()
        .map(|rows| {
            rows.into_iter()
                .map(
                    |(node_id, run_id, circuit_id, circuit_name, state, parent_node_id)| {
                        CircuitAgentOwnership {
                            node_id,
                            run_id,
                            circuit_id,
                            circuit_name,
                            state,
                            parent_node_id,
                        }
                    },
                )
                .collect()
        })
        .map_err(DbError::from)
}

fn map_circuit_rows(
    rows: Vec<(
        crate::models::AutopilotCircuit,
        Vec<crate::db::CircuitRunLedger>,
    )>,
) -> Vec<CircuitWithRuns> {
    rows.into_iter()
        .map(|(circuit, ledgers)| CircuitWithRuns {
            circuit,
            runs: ledgers
                .into_iter()
                .map(|ledger| CircuitRunDetail {
                    run: ledger.run,
                    steps: ledger.steps,
                    review_round_limit: ledger.review_round_limit,
                    review_continuation: Some(ledger.review_continuation),
                })
                .collect(),
        })
        .collect()
}

fn map_queue_rows(
    rows: Vec<(crate::models::AutopilotCircuitRun, String)>,
) -> Vec<CircuitQueueEntry> {
    rows.into_iter()
        .enumerate()
        .map(|(index, (run, circuit_name))| CircuitQueueEntry {
            run,
            circuit_name,
            queue_rank: index as i64 + 1,
        })
        .collect()
}
