//! Autopilot Circuits IPC surface (spec #1205 / walking skeleton #1206).
//!
//! Minimal milestone-1 contract: list / create / enable / delete
//! circuits, Trigger Now, and read back runs with their step ledger. The
//! walking-skeleton blueprint is built server-side
//! ([`CircuitGraph::walking_skeleton`]) so the AST stays canonical in
//! Rust — the throwaway Probe-tab authoring only sends a name and a
//! prompt.

use tauri::{command, AppHandle, Emitter};

use crate::autopilot::circuit::model::{
    trigger_kind_to_node_kind, validate_circuit_request, CircuitGraph,
};
pub use crate::autopilot::circuit::model::CircuitBlueprintKind;
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

/// User-requested adjacent movement in the pending Circuit Run queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitQueueDirection.ts")]
#[serde(rename_all = "lowercase")]
pub enum CircuitQueueDirection {
    Up,
    Down,
}

/// One run plus its step ledger, for the Probe tab's run list.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitRunDetail.ts")]
pub struct CircuitRunDetail {
    pub run: AutopilotCircuitRun,
    pub steps: Vec<AutopilotCircuitRunStep>,
}

/// One pending Circuit Run in the mesh-wide admission queue. `queue_rank` is
/// presentation-friendly (1 = next to start); the mutable storage position
/// stays private to the database.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitQueueEntry.ts")]
pub struct CircuitQueueEntry {
    pub run: AutopilotCircuitRun,
    pub circuit_name: String,
    #[ts(as = "i32")]
    pub queue_rank: i64,
}

/// Identifies the circuit run currently or historically owning a visible
/// Agent Node. Generated for the node store; the run id is the number shown
/// in the header pill.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitAgentOwnership.ts")]
pub struct CircuitAgentOwnership {
    #[ts(as = "i32")]
    pub node_id: i64,
    #[ts(as = "i32")]
    pub run_id: i64,
    #[ts(as = "i32")]
    pub circuit_id: i64,
    pub circuit_name: String,
    pub state: String,
}

#[command]
pub fn list_circuit_agent_ownerships() -> Result<Vec<CircuitAgentOwnership>, String> {
    crate::db::list_circuit_agent_ownerships()
        .map(|rows| {
            rows.into_iter()
                .map(|(node_id, run_id, circuit_id, circuit_name, state)| CircuitAgentOwnership {
                    node_id,
                    run_id,
                    circuit_id,
                    circuit_name,
                    state,
                })
                .collect()
        })
        .map_err(|error| error.to_string())
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

/// One circuit plus its visible run ledger — the Probe tab's load unit.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitWithRuns.ts")]
pub struct CircuitWithRuns {
    pub circuit: AutopilotCircuit,
    pub runs: Vec<CircuitRunDetail>,
}

/// The complete Circuits Probe hydration payload. Keeping the ledger and the
/// mesh queue in one response preserves the Probe's single-IPC load contract
/// while retaining the standalone commands for older clients.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitProbeSnapshot.ts")]
pub struct CircuitProbeSnapshot {
    pub circuits: Vec<CircuitWithRuns>,
    pub queue: Vec<CircuitQueueEntry>,
}

fn map_circuit_rows(
    rows: Vec<(crate::models::AutopilotCircuit, Vec<crate::db::CircuitRunLedger>)>,
) -> Vec<CircuitWithRuns> {
    rows.into_iter()
        .map(|(circuit, ledgers)| CircuitWithRuns {
            circuit,
            runs: ledgers
                .into_iter()
                .map(|ledger| CircuitRunDetail { run: ledger.run, steps: ledger.steps })
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

/// Batched single-IPC load for the Circuits Probe tab: every user-authored
/// circuit (and any active built-in preset) on the mesh with every
/// running/paused run and up to `limit` newest terminal runs (steps included),
/// one command instead of N+1 round-trips. Pending runs are returned by
/// `list_circuit_queue` so none are hidden behind this limit.
#[command]
pub fn list_circuits_with_runs(
    mesh_id: i64,
    limit: Option<i64>,
) -> Result<Vec<CircuitWithRuns>, String> {
    let limit = limit.unwrap_or(10).clamp(1, 100);
    crate::db::list_circuits_with_recent_runs(mesh_id, limit)
        .map(map_circuit_rows)
        .map_err(|e| e.to_string())
}

#[command]
pub fn list_circuit_queue(mesh_id: i64) -> Result<Vec<CircuitQueueEntry>, String> {
    crate::db::list_queued_circuit_runs(mesh_id)
        .map(map_queue_rows)
        .map_err(|error| error.to_string())
}

#[command]
pub fn list_circuit_probe(
    mesh_id: i64,
    limit: Option<i64>,
) -> Result<CircuitProbeSnapshot, String> {
    let limit = limit.unwrap_or(10).clamp(1, 100);
    let (circuits, queue) = crate::db::list_circuit_probe(mesh_id, limit)
        .map_err(|e| e.to_string())?;
    let circuits = map_circuit_rows(circuits);
    let queue = map_queue_rows(queue);
    Ok(CircuitProbeSnapshot { circuits, queue })
}

/// Create a circuit with the canonical blueprint.
///
/// Milestone 3 (issue #1208) added the trigger vocabulary: `trigger_kind`
/// selects the root node (see [`CircuitTriggerKind`]). All domain
/// restrictions — the review blueprint's GitHub-issue-label trigger
/// requirement, concurrency floors, interval clamp, label trim — live in
/// [`crate::autopilot::circuit::model::validate_circuit_request`].
/// This Tauri command is a dumb router: parse → call the model →
/// persist → wake the GitHub poll for labelled circuits.
// Tauri IPC commands carry every primitive as a separate wire parameter;
// collapsing into a struct would change the wire shape.
#[allow(clippy::too_many_arguments)]
#[command]
pub fn create_circuit(
    mesh_id: i64,
    name: String,
    description: String,
    concurrency_limit: i64,
    initial_prompt: String,
    trigger_kind: Option<CircuitTriggerKind>,
    trigger_label: Option<String>,
    interval_seconds: Option<i64>,
    blueprint: Option<CircuitBlueprintKind>,
) -> Result<AutopilotCircuit, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("circuit name must not be empty".to_string());
    }
    let blueprint = blueprint.unwrap_or(CircuitBlueprintKind::WalkingSkeleton);

    // Mesh policy gate: the review blueprint's wrap-up path opens a PR;
    // a mesh configured with `action_on_success = "none"` has no PR
    // pipeline to feed. Lives outside the domain model because it's
    // mesh-level configuration, not a blueprint property.
    if matches!(blueprint, CircuitBlueprintKind::IssueDrivenAutopilotReview)
        && crate::services::autopilot::configured_action_on_success(mesh_id) == "none"
    {
        return Err(
            "the issue-driven Autopilot review blueprint requires a pull-request wrap-up policy"
                .to_string(),
        );
    }

    let validated = validate_circuit_request(
        blueprint,
        trigger_kind,
        trigger_label.as_deref(),
        interval_seconds,
        concurrency_limit,
    )?;
    let kind = trigger_kind_to_node_kind(&validated);

    let graph = match blueprint {
        CircuitBlueprintKind::WalkingSkeleton => {
            CircuitGraph::triggered_skeleton(&initial_prompt, kind.clone())
        }
        CircuitBlueprintKind::IssueDrivenAutopilotReview => {
            // Guarded above: the model rejected every other trigger;
            // we still defensively unwrap the label here.
            let crate::autopilot::circuit::model::CircuitNodeKind::GithubIssueLabel {
                label,
            } = &kind
            else {
                return Err(
                    "the issue-driven Autopilot review blueprint requires an issue-label trigger"
                        .to_string(),
                );
            };
            CircuitGraph::issue_driven_autopilot_review(label)
        }
    };
    graph.validate()?;
    let graph_json = graph.to_json()?;
    let circuit = crate::db::create_autopilot_circuit(
        mesh_id,
        name,
        &description,
        validated.concurrency_limit,
        &graph_json,
    )
    .map_err(|e| e.to_string())?;

    // On-demand poll capability: a freshly created labelled circuit
    // ingests on the very next worker tick.
    if matches!(
        validated.trigger_kind,
        CircuitTriggerKind::GithubIssueLabel | CircuitTriggerKind::GithubPrLabel
    ) {
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
/// bounded cycles — see [`CircuitGraph::validate`]), so an editor bug
/// can never persist a graph the stepper would walk forever. The worker
/// wakes so trigger passes see the new topology immediately.
#[command]
pub fn update_circuit_graph(circuit_id: i64, graph_json: String) -> Result<(), String> {
    let mut graph = CircuitGraph::from_json(&graph_json)?;
    graph.upgrade_legacy_issue_review_first_turns();
    graph.validate()?;
    // Persist the parsed/canonical form so an explicit legacy graph upgrade
    // is durable after an editor save.
    let canonical_graph_json = graph.to_json()?;
    crate::db::update_autopilot_circuit_graph(circuit_id, &canonical_graph_json).map_err(|e| {
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

fn retire_cancelled_agents_with(
    agent_ids: Vec<i64>,
    mut retire: impl FnMut(i64) -> Result<(), String>,
) -> Result<(), String> {
    let mut failures = Vec::new();
    for agent_id in agent_ids {
        if let Err(error) = retire(agent_id) {
            failures.push(format!("agent node {}: {}", agent_id, error));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("run was cancelled, but cleanup failed for {}", failures.join("; ")))
    }
}

fn retire_cancelled_agents(agent_ids: Vec<i64>) -> Result<(), String> {
    retire_cancelled_agents_with(agent_ids, |agent_id| {
        match crate::db::get_agent_node_by_id(agent_id) {
            Ok(_) => crate::services::agent_node::delete(agent_id, true)
                .map_err(|error| error.to_string()),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(()),
            Err(error) => Err(format!("lookup failed: {}", error)),
        }
    })
}

fn release_circuit_source(run_id: i64) {
    if let Some(source) = crate::db::get_circuit_run(run_id).ok().flatten()
        .and_then(|run| run.source_agent_node_id) {
        crate::autopilot::evaluator::unregister(source);
    }
}

fn cancel_run_and_cleanup_inner(app: &AppHandle, run_id: i64) -> Result<(), String> {
    let agent_ids = crate::db::cancel_circuit_run(run_id).map_err(|error| error.to_string())?;
    release_circuit_source(run_id);
    crate::services::circuit_worker::wake_circuit_worker();
    let cleanup = retire_cancelled_agents(agent_ids);
    let state = crate::db::get_circuit_run(run_id)
        .map_err(|error| error.to_string())?
        .map(|run| run.state)
        .unwrap_or_else(|| "cancelled".to_string());
    let _ = app.emit(
        "circuit-run-updated",
        crate::services::circuit_worker::CircuitRunUpdatedPayload { run_id, state },
    );
    cleanup
}

fn cancel_run_and_cleanup(app: &AppHandle, run_id: i64) -> Result<(), String> {
    match crate::services::circuit_worker::with_circuit_run_spawns_quiesced(run_id, || {
        cancel_run_and_cleanup_inner(app, run_id)
    }) {
        Ok(result) => result,
        Err(wait_error) => {
            // Stop future effects even if a stage-2 spawn exceeded the
            // quiescence window. The spawn's post-launch compensation will
            // retire itself; retaining the terminal ledger makes a retry
            // possible if that OS cleanup is transiently locked.
            let _ = crate::db::cancel_circuit_run(run_id);
            release_circuit_source(run_id);
            crate::services::circuit_worker::wake_circuit_worker();
            Err(wait_error)
        }
    }
}

#[command]
pub fn cancel_circuit_run(app: AppHandle, run_id: i64) -> Result<(), String> {
    cancel_run_and_cleanup(&app, run_id)
}

#[command]
pub fn move_circuit_run(run_id: i64, direction: CircuitQueueDirection) -> Result<(), String> {
    let toward_front = match direction {
        CircuitQueueDirection::Up => true,
        CircuitQueueDirection::Down => false,
    };
    crate::db::move_queued_circuit_run(run_id, toward_front)
        .map_err(|error| error.to_string())?;
    crate::services::circuit_worker::wake_circuit_worker();
    Ok(())
}

#[command]
pub fn delete_circuit(app: AppHandle, circuit_id: i64) -> Result<(), String> {
    crate::db::get_autopilot_circuit(circuit_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("circuit {} does not exist", circuit_id))?;

    // Stop fresh trigger ingestion before terminalising existing runs. The
    // worker re-checks terminal state before spawning, closing the race with a
    // pass that already loaded this circuit.
    crate::db::set_autopilot_circuit_enabled(circuit_id, false)
        .map_err(|error| error.to_string())?;
    let result = crate::services::circuit_worker::with_circuit_spawns_quiesced(circuit_id, || {
        let run_ids = crate::db::list_circuit_run_ids_for_cleanup(circuit_id)
            .map_err(|error| error.to_string())?;
        let mut cleanup_errors = Vec::new();
        for run_id in run_ids {
            if let Err(error) = cancel_run_and_cleanup_inner(&app, run_id) {
                cleanup_errors.push(format!("run {}: {}", run_id, error));
            }
        }
        // Keep the ledger as the retry anchor when any external retirement
        // fails. Deleting it here would erase the agent IDs needed by a
        // subsequent retry and could orphan a retained PTY/worktree
        // permanently. The circuit is already disabled and every run has
        // been terminalised, so retrying is safe and exhaustive.
        if !cleanup_errors.is_empty() {
            return Err(format!(
                "circuit cleanup incomplete; ledger retained for retry: {}",
                cleanup_errors.join("; ")
            ));
        }
        // Ledger deletion stays inside the barrier: a worker pass that had
        // already loaded this circuit cannot begin a late stage-1 spawn after
        // the final cleanup snapshot.
        crate::db::delete_autopilot_circuit(circuit_id).map_err(|e| e.to_string())
    });
    match result {
        Ok(result) => result,
        Err(wait_error) => {
            // Terminalise every known run even when a spawn refuses to quiesce
            // within the bounded wait. Keep the disabled ledger for the next
            // retry, which will re-snapshot all attached agents.
            if let Ok(run_ids) = crate::db::list_circuit_run_ids_for_cleanup(circuit_id) {
                for run_id in run_ids {
                    let _ = crate::db::cancel_circuit_run(run_id);
                    release_circuit_source(run_id);
                }
            }
            crate::services::circuit_worker::wake_circuit_worker();
            Err(format!(
                "circuit cleanup incomplete; ledger retained for retry: {}",
                wait_error
            ))
        }
    }
}

/// Trigger Now: mint a fresh `pending` run with a `manual:<unix-ms>`
/// dedupe identity, seed its `circuit.*` template context, and wake the
/// worker so it starts within milliseconds. Returns the new run id.
/// Works on disabled (draft) circuits so a graph can be dry-tested
/// before background pollers are turned on (issue #1356).
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
    let graph = CircuitGraph::from_json(&circuit.graph_json)?;
    if graph.requires_source_agent() {
        return Err("Start this Circuit from the source agent's title bar.".into());
    }
    if graph.is_issue_driven_autopilot_review() {
        return Err(
            "issue-driven Autopilot review circuits are triggered by labelled GitHub issues; Trigger Now requires issue context"
                .to_string(),
        );
    }
    // Draft-first (issue #1356): Trigger Now is the dry-run seam and
    // must work while the circuit is still disabled. Background
    // pollers (`list_enabled_circuits`) stay gated on `enabled`.
    let identity = format!(
        "manual:{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let mut context = crate::autopilot::circuit::context::CircuitContext::new();
    context.with_circuit(circuit.id, &circuit.name, circuit.mesh_id);
    let action = crate::services::autopilot::configured_action_on_success(circuit.mesh_id);
    context.with_autopilot_finish_prompt(None, Some(action.as_str()));
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

#[command]
pub fn trigger_circuit_from_node(app: AppHandle, node_id: i64, circuit_id: Option<i64>, max_rounds: i32) -> Result<i64, String> {
    if !(1..=10).contains(&max_rounds) {
        return Err("Review rounds must be between 1 and 10.".into());
    }
    if !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
        return Err("Resume the agent before starting a review.".into());
    }
    let node = crate::db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())?;
    if node.provider == "terminal" {
        return Err("Review loops require an AI agent.".into());
    }
    let run_id = crate::db::create_node_circuit_run(node_id, circuit_id, max_rounds)?;
    crate::autopilot::evaluator::register_circuit(node_id);
    let state = crate::db::get_circuit_run(run_id)
        .map_err(|e| e.to_string())?
        .map(|run| run.state)
        .unwrap_or_else(|| "pending".into());
    let _ = app.emit("circuit-run-updated", crate::services::circuit_worker::CircuitRunUpdatedPayload {
        run_id, state,
    });
    crate::services::circuit_worker::wake_circuit_worker();
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
    if !crate::db::transition_circuit_run_state(run_id, "running", "paused")
        .map_err(|e| e.to_string())?
    {
        return Err(format!("run {} changed state before it could be paused", run_id));
    }
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
    if !crate::db::transition_circuit_run_state(run_id, "paused", "running")
        .map_err(|e| e.to_string())?
    {
        return Err(format!("run {} changed state before it could be resumed", run_id));
    }
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

#[cfg(test)]
mod tests {
    use super::retire_cancelled_agents_with;

    #[test]
    fn cancellation_retirement_attempts_every_agent_and_reports_partial_failure() {
        let mut attempted = Vec::new();
        let error = retire_cancelled_agents_with(vec![11, 12, 13], |agent_id| {
            attempted.push(agent_id);
            if agent_id == 12 {
                Err("worktree busy".to_string())
            } else {
                Ok(())
            }
        })
        .unwrap_err();

        assert_eq!(attempted, vec![11, 12, 13]);
        assert!(error.contains("agent node 12: worktree busy"));
    }
}
