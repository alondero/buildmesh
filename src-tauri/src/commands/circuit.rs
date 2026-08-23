//! Autopilot Circuits IPC surface (spec #1205 / walking skeleton #1206).
//!
//! Minimal milestone-1 contract: list / create / enable / delete
//! circuits, Trigger Now, and read back runs with their step ledger. The
//! walking-skeleton blueprint is built server-side
//! ([`CircuitGraph::walking_skeleton`]) so the AST stays canonical in
//! Rust — the throwaway Probe-tab authoring only sends a name and a
//! prompt.

use tauri::command;

use crate::autopilot::circuit::model::CircuitGraph;
use crate::models::{AutopilotCircuit, AutopilotCircuitRun, AutopilotCircuitRunStep};

/// One run plus its step ledger, for the Probe tab's run list.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitRunDetail.ts")]
pub struct CircuitRunDetail {
    pub run: AutopilotCircuitRun,
    pub steps: Vec<AutopilotCircuitRunStep>,
}

#[command]
pub fn list_circuits(mesh_id: i64) -> Result<Vec<AutopilotCircuit>, String> {
    crate::db::list_autopilot_circuits(mesh_id).map_err(|e| e.to_string())
}

/// One circuit plus its recent run ledger — the Probe tab's load unit.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitWithRuns.ts")]
pub struct CircuitWithRuns {
    pub circuit: AutopilotCircuit,
    pub runs: Vec<CircuitRunDetail>,
}

/// Batched single-IPC load for the Circuits Probe tab: every circuit on
/// the mesh with up to `limit` newest runs each (steps included), one
/// command instead of N+1 round-trips.
#[command]
pub fn list_circuits_with_runs(
    mesh_id: i64,
    limit: Option<i64>,
) -> Result<Vec<CircuitWithRuns>, String> {
    let limit = limit.unwrap_or(10).clamp(1, 100);
    crate::db::list_circuits_with_recent_runs(mesh_id, limit)
        .map(|rows| {
            rows.into_iter()
                .map(|(circuit, ledgers)| CircuitWithRuns {
                    circuit,
                    runs: ledgers
                        .into_iter()
                        .map(|l| CircuitRunDetail { run: l.run, steps: l.steps })
                        .collect(),
                })
                .collect()
        })
        .map_err(|e| e.to_string())
}

/// Create a circuit with the canonical walking-skeleton blueprint:
/// Manual trigger → SpawnAgentNode (fresh) → InjectPty(prompt) → Notify.
#[command]
pub fn create_circuit(
    mesh_id: i64,
    name: String,
    description: String,
    concurrency_limit: i64,
    initial_prompt: String,
) -> Result<AutopilotCircuit, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("circuit name must not be empty".to_string());
    }
    let limit = concurrency_limit.clamp(1, 16);
    let graph = CircuitGraph::walking_skeleton(&initial_prompt);
    let graph_json = graph.to_json()?;
    crate::db::create_autopilot_circuit(mesh_id, name, &description, limit, &graph_json)
        .map_err(|e| e.to_string())
}

#[command]
pub fn set_circuit_enabled(circuit_id: i64, enabled: bool) -> Result<(), String> {
    crate::db::set_autopilot_circuit_enabled(circuit_id, enabled).map_err(|e| e.to_string())
}

#[command]
pub fn delete_circuit(circuit_id: i64) -> Result<(), String> {
    crate::db::delete_autopilot_circuit(circuit_id).map_err(|e| e.to_string())
}

/// Trigger Now: mint a fresh `pending` run with a `manual:<unix-ms>`
/// dedupe identity, seed its `circuit.*` template context, and wake the
/// worker so it starts within milliseconds. Returns the new run id.
///
/// One lock acquisition: the row is inserted with its base context in a
/// single statement. `circuit.run_id` can't be known before the insert,
/// so the worker tops it up on the first pass through its own atomic
/// commit (`drive_run`'s context seeding).
#[command]
pub fn trigger_circuit_now(circuit_id: i64) -> Result<i64, String> {
    let circuit = crate::db::get_autopilot_circuit(circuit_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("circuit {} does not exist", circuit_id))?;
    if !circuit.enabled {
        return Err("circuit is disabled — enable it before triggering".to_string());
    }
    let identity = format!(
        "manual:{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let mut context = crate::autopilot::circuit::context::CircuitContext::new();
    context.with_circuit(circuit.id, &circuit.name, circuit.mesh_id);
    let run_id = crate::db::create_circuit_run(
        circuit.id,
        circuit.mesh_id,
        &identity,
        &context.to_json()?,
    )
    .map_err(|e| e.to_string())?;
    crate::services::circuit_worker::wake_circuit_worker();
    tracing::info!("circuits: manual trigger for circuit {} → run {}", circuit_id, run_id);
    Ok(run_id)
}

/// Recent runs for one circuit, newest first, each with its step ledger.
#[command]
pub fn list_circuit_runs(
    circuit_id: i64,
    limit: Option<i64>,
) -> Result<Vec<CircuitRunDetail>, String> {
    let limit = limit.unwrap_or(20).clamp(1, 100);
    let runs = crate::db::list_circuit_runs(circuit_id, limit).map_err(|e| e.to_string())?;
    runs.into_iter()
        .map(|run| {
            let steps = crate::db::list_circuit_run_steps(run.id).map_err(|e| e.to_string())?;
            Ok(CircuitRunDetail { run, steps })
        })
        .collect()
}
