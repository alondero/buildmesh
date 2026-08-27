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

/// The trigger vocabulary of [`create_circuit`] (issue #1208). Generated
/// to `src/types/generated/CircuitTriggerKind.ts` — the TS side imports
/// this type rather than hand-declaring the union (issue #359 rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitTriggerKind.ts")]
#[serde(rename_all = "snake_case")]
pub enum CircuitTriggerKind {
    /// Fire-by-hand only (Trigger Now).
    Manual,
    /// Fire on a fixed cadence (`interval_seconds`, cooldown-paced).
    Interval,
    /// Fire when an open issue gains `trigger_label`.
    GithubIssueLabel,
    /// Fire when an open PR gains `trigger_label`.
    GithubPrLabel,
}

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

/// One circuit row by id — the canvas editor overlay's load unit (#1209).
#[command]
pub fn get_circuit(circuit_id: i64) -> Result<AutopilotCircuit, String> {
    crate::db::get_autopilot_circuit(circuit_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("circuit {} does not exist", circuit_id))
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

/// Create a circuit with the canonical blueprint:
/// `<trigger>` → SpawnAgentNode (fresh) → InjectPty(prompt) → Notify.
///
/// Milestone 3 (issue #1208) added the trigger vocabulary: `trigger_kind`
/// selects the root node (see [`CircuitTriggerKind`]). GitHub triggers
/// require a non-empty `trigger_label`; interval circuits take
/// `interval_seconds` (clamped to 60s–7d so a typo can't become a hot
/// loop). A GitHub-labelled circuit requests an immediate poll so its
/// first run can start without waiting out the 120s cadence.
// Tauri IPC commands carry every primitive as a separate wire parameter;
// collapsing into a struct would change the wire shape and is out of scope.
#[allow(clippy::too_many_arguments)]
#[command]
#[allow(clippy::too_many_arguments)] // Tauri command surface — one arg per UI field
pub fn create_circuit(
    mesh_id: i64,
    name: String,
    description: String,
    concurrency_limit: i64,
    initial_prompt: String,
    trigger_kind: Option<CircuitTriggerKind>,
    trigger_label: Option<String>,
    interval_seconds: Option<i64>,
) -> Result<AutopilotCircuit, String> {
    use crate::autopilot::circuit::model::CircuitNodeKind;

    let name = name.trim();
    if name.is_empty() {
        return Err("circuit name must not be empty".to_string());
    }
    let limit = concurrency_limit.clamp(1, 16);
    let kind = match trigger_kind.unwrap_or(CircuitTriggerKind::Manual) {
        CircuitTriggerKind::Manual => CircuitNodeKind::Manual,
        CircuitTriggerKind::Interval => {
            let secs = interval_seconds.unwrap_or(300).clamp(60, 7 * 24 * 3_600);
            CircuitNodeKind::Interval { interval_seconds: secs }
        }
        CircuitTriggerKind::GithubIssueLabel | CircuitTriggerKind::GithubPrLabel => {
            let label = trigger_label
                .as_deref()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .ok_or_else(|| "GitHub triggers require a non-empty trigger label".to_string())?;
            if trigger_kind == Some(CircuitTriggerKind::GithubPrLabel) {
                CircuitNodeKind::GithubPullRequestLabel { label: label.to_string() }
            } else {
                CircuitNodeKind::GithubIssueLabel { label: label.to_string() }
            }
        }
    };
    let graph = CircuitGraph::triggered_skeleton(&initial_prompt, kind.clone());
    let graph_json = graph.to_json()?;
    let circuit =
        crate::db::create_autopilot_circuit(mesh_id, name, &description, limit, &graph_json)
            .map_err(|e| e.to_string())?;
    if matches!(
        kind,
        CircuitNodeKind::GithubIssueLabel { .. } | CircuitNodeKind::GithubPullRequestLabel { .. }
    ) {
        // On-demand poll capability: a freshly created labelled circuit
        // ingests on the very next worker tick.
        crate::services::circuit_triggers::request_github_poll();
    }
    Ok(circuit)
}

#[command]
pub fn set_circuit_enabled(circuit_id: i64, enabled: bool) -> Result<(), String> {
    crate::db::set_autopilot_circuit_enabled(circuit_id, enabled).map_err(|e| e.to_string())
}

/// Save a blueprint edited in the canvas editor (issue #1209): the
/// whole `graph_json` is replaced after validating it parses into the
/// AST *and* passes semantic checks (unique ids, resolvable edges,
/// acyclic — see [`CircuitGraph::validate`]), so an editor bug can
/// never persist a graph the stepper would walk forever. The worker
/// wakes so trigger passes see the new topology immediately.
#[command]
pub fn update_circuit_graph(circuit_id: i64, graph_json: String) -> Result<(), String> {
    let graph = CircuitGraph::from_json(&graph_json)?;
    graph.validate()?;
    crate::db::update_autopilot_circuit_graph(circuit_id, &graph_json).map_err(|e| {
        if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
            format!("circuit {} does not exist", circuit_id)
        } else {
            e.to_string()
        }
    })?;
    crate::services::circuit_worker::wake_circuit_worker();
    tracing::info!("circuits: graph saved for circuit {}", circuit_id);
    Ok(())
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

// ---------------------------------------------------------------------------
// Human-in-the-loop (#1207): graceful pause/resume + collaborator approval.
// ---------------------------------------------------------------------------

/// Gracefully pause one active run: the graph stops advancing while the
/// current steps finish. Idempotent-friendly (pausing a paused run is a
/// no-op error only if the run isn't active).
#[command]
pub fn pause_circuit_run(run_id: i64) -> Result<(), String> {
    let run = crate::db::get_circuit_run(run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("run {} does not exist", run_id))?;
    if run.state != "running" {
        return Err(format!("only running runs can be paused (run {} is {})", run_id, run.state));
    }
    crate::db::set_circuit_run_state(run_id, "paused").map_err(|e| e.to_string())?;
    crate::services::circuit_worker::wake_circuit_worker();
    tracing::info!("circuits: run {} paused", run_id);
    Ok(())
}

/// Resume one paused run; automation picks up exactly where it stopped.
#[command]
pub fn resume_circuit_run(run_id: i64) -> Result<(), String> {
    let run = crate::db::get_circuit_run(run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("run {} does not exist", run_id))?;
    if run.state != "paused" {
        return Err(format!("only paused runs can be resumed (run {} is {})", run_id, run.state));
    }
    crate::db::set_circuit_run_state(run_id, "running").map_err(|e| e.to_string())?;
    crate::services::circuit_worker::wake_circuit_worker();
    tracing::info!("circuits: run {} resumed", run_id);
    Ok(())
}

/// Approve a CollaboratorCheck gate parked in `blocked` on this run.
#[command]
pub fn approve_circuit_step(run_id: i64, node_id: String) -> Result<(), String> {
    let steps = crate::db::list_circuit_run_steps(run_id).map_err(|e| e.to_string())?;
    let step = steps
        .iter()
        .find(|s| s.node_id == node_id)
        .ok_or_else(|| format!("run {} has no step {}", run_id, node_id))?;
    if step.status != "blocked" {
        return Err(format!(
            "step {} is not waiting for approval (status {})",
            node_id, step.status
        ));
    }
    crate::services::circuit_worker::request_circuit_approval(run_id, node_id);
    Ok(())
}
