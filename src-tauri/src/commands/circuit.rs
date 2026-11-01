//! Circuit IPC surface (spec #1205 / walking skeleton #1206).
//!
//! Minimal milestone-1 contract: list / create / enable / delete
//! circuits, Trigger Now, and read back runs with their step ledger. The
//! walking-skeleton blueprint is built server-side
//! ([`CircuitGraph::walking_skeleton`]) so the AST stays canonical in
//! Rust — the throwaway Probe-tab authoring only sends a name and a
//! prompt.

use tauri::{command, AppHandle, Emitter};

use crate::circuit::model::{trigger_kind_to_node_kind, validate_circuit_request, CircuitGraph};
pub use crate::circuit::model::{CircuitBlueprintKind, CircuitTriggerKind};
use crate::db::DbError;
use crate::models::{AutopilotCircuit, AutopilotCircuitRun, AutopilotCircuitRunStep};

/// User-requested movement in the pending Circuit Run queue: adjacent
/// steps plus jumps to either edge for long queues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitQueueDirection.ts")]
#[serde(rename_all = "lowercase")]
pub enum CircuitQueueDirection {
    Up,
    Down,
    Top,
    Bottom,
}

/// One run plus its step ledger, for the Probe tab's run list.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitRunDetail.ts")]
pub struct CircuitRunDetail {
    pub run: AutopilotCircuitRun,
    pub steps: Vec<AutopilotCircuitRunStep>,
    /// Active review allowance: zero is unlimited, absent means unavailable.
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_round_limit: Option<i32>,
}

#[command]
pub fn circuit_run_history(
    run_id: i64,
) -> Result<crate::db::circuit::evidence::CircuitEvidenceView, String> {
    crate::db::circuit::evidence::history(run_id).map_err(|e| e.to_string())
}

#[command]
pub fn copy_review_blueprint(circuit_id: i64, name: String) -> Result<AutopilotCircuit, String> {
    crate::db::circuit::ledger::copy_review_blueprint(circuit_id, &name).map_err(String::from)
}

/// Tell the desktop windows a run changed under them. The mobile HTTP routes
/// share this with the commands; `None` is a process whose app handle is not
/// installed yet, which has no window to tell.
pub(crate) fn emit_run_updated(app: Option<&AppHandle>, run_id: i64, state: &str) {
    if let Some(app) = app {
        let _ = app.emit(
            "circuit-run-updated",
            crate::services::circuit_worker::CircuitRunUpdatedPayload {
                run_id,
                state: state.into(),
            },
        );
    }
}

/// Record the outcome and wake the worker. Shared by the command and the mobile
/// route so both obey the same revision and eligibility checks.
///
/// The failure stays typed: the mobile route answers `503` for a transient
/// pool shortage and `409` for a refused revision, and text cannot tell those
/// apart (issue #1533). The command above flattens it at the Tauri boundary.
pub(crate) fn record_circuit_outcome_now(
    request: &crate::db::circuit::evidence::CheckpointRequest,
) -> Result<(), DbError> {
    crate::db::circuit::evidence::record_outcome(request)?;
    crate::services::circuit_worker::wake_circuit_worker();
    Ok(())
}

#[command]
pub async fn record_circuit_outcome(
    app: AppHandle,
    request: crate::db::circuit::evidence::CheckpointRequest,
) -> Result<(), String> {
    let run_id = request.run_id;
    crate::blocking::run_blocking("record_circuit_outcome", move || {
        record_circuit_outcome_now(&request).map_err(String::from)
    })
    .await?;
    emit_run_updated(Some(&app), run_id, "running");
    Ok(())
}

pub(crate) fn circuit_prompt_payload_blocking(
    request: crate::db::circuit::evidence::prompt_delivery::CircuitPromptRequest,
) -> Result<String, String> {
    crate::services::circuit_prompt::payload(&request).map_err(String::from)
}

#[command]
pub async fn circuit_prompt_payload(
    request: crate::db::circuit::evidence::prompt_delivery::CircuitPromptRequest,
) -> Result<String, String> {
    crate::blocking::run_blocking("circuit_prompt_payload", move || {
        circuit_prompt_payload_blocking(request)
    })
    .await
}

/// Everything a node card's Overview tab draws for one run, read at once.
#[command]
pub fn circuit_run_overview(
    run_id: i64,
) -> Result<crate::db::circuit::overview::CircuitRunOverview, String> {
    crate::db::circuit::overview::overview(run_id).map_err(String::from)
}

/// Move a stuck agent wait or classifier step on to the outcome a person
/// chose, overriding stale work or input evidence, and wake the worker so the
/// next stage starts promptly.
#[command]
pub fn push_circuit_step(
    app: AppHandle,
    request: crate::db::circuit::operator_push::CircuitStepPushRequest,
) -> Result<(), String> {
    crate::db::circuit::operator_push::push_step(&request)?;
    crate::services::circuit_worker::wake_circuit_worker();
    emit_run_updated(Some(&app), request.run_id, "running");
    Ok(())
}

/// What the run card shows a person who has to act: the steps awaiting a
/// decision, how a failed run can be recovered, and the revision to act against.
/// Cheap by design: unlike the history it carries no log.
#[command]
pub fn circuit_run_attention(
    run_id: i64,
) -> Result<crate::db::circuit::evidence::CircuitRunAttention, String> {
    crate::db::circuit::evidence::attention(run_id).map_err(String::from)
}

/// How many runs on this mesh are still asking for attention.
///
/// Counted against the same unbounded predicate `dismiss_all_circuit_run_attention`
/// updates, not against the Probe's per-circuit ledger window, so the toolbar's
/// confirmation names every run the command will touch.
#[command]
pub fn count_circuit_run_attention(mesh_id: i64) -> Result<i64, String> {
    crate::db::count_outstanding_circuit_run_attention(mesh_id).map_err(|e| e.to_string())
}

/// Acknowledge every outstanding attention run on a mesh — the History tab's
/// "Dismiss all (N)". Returns how many runs changed so the UI can report the
/// real number instead of an optimistic one.
///
/// Selection happens in SQL against the same predicate that decides which runs
/// are surfaced, keeping it in step with what History called attention.
#[command]
pub fn dismiss_all_circuit_run_attention(app: AppHandle, mesh_id: i64) -> Result<usize, String> {
    let changed =
        crate::db::dismiss_all_circuit_run_attention(mesh_id).map_err(|e| e.to_string())?;
    if changed > 0 {
        let _ = app.emit(
            "circuit-runs-attention-dismissed",
            crate::services::circuit_worker::CircuitRunAttentionDismissedPayload {
                mesh_id,
                changed,
            },
        );
    }
    Ok(changed)
}

/// Retry a failed run's failed step, or record that the person finished it, and
/// reopen the run. The worker is woken so it re-enters the queue promptly.
#[command]
pub fn recover_failed_circuit_run(
    app: AppHandle,
    request: crate::db::circuit::step_recovery::RecoveryRequest,
) -> Result<(), String> {
    recover_failed_circuit_run_now(&request)?;
    emit_run_updated(Some(&app), request.run_id, "pending");
    Ok(())
}

/// Run Recovery plus the worker wake, shared with the mobile route.
///
/// The failure stays typed: the mobile route answers `503` for a transient
/// pool shortage and `409` for a refused recovery, and text cannot tell those
/// apart (issue #1533).
pub(crate) fn recover_failed_circuit_run_now(
    request: &crate::db::circuit::step_recovery::RecoveryRequest,
) -> Result<(), DbError> {
    crate::db::circuit::step_recovery::recover_failed_run(request)?;
    crate::services::circuit_worker::wake_circuit_worker();
    Ok(())
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
    // Presentation relationship only; the circuit retains process ownership.
    #[ts(as = "Option<i32>")]
    pub parent_node_id: Option<i64>,
}

#[command]
pub fn list_circuit_agent_ownerships() -> Result<Vec<CircuitAgentOwnership>, String> {
    list_circuit_agent_ownerships_now().map_err(|error| error.to_string())
}

/// The ownership ledger read, keeping the database failure typed.
///
/// The command above flattens the failure to text because that is the Tauri
/// wire type; the mobile route needs to know whether the failure was a
/// *transient* pool shortage (a `503` it can retry) or a broken query, and
/// text cannot answer that (issue #1533).
pub(crate) fn list_circuit_agent_ownerships_now() -> Result<Vec<CircuitAgentOwnership>, DbError> {
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
    /// Latest worker admission observation for each pending run.
    pub queue_waits: std::collections::HashMap<String, String>,
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

/// Batched single-IPC load for the Circuits Probe tab: every user-authored
/// circuit (and any active built-in preset) on the mesh with every
/// running/paused run (Activity — the exact capacity set) plus up to
/// `limit` newest terminal runs and a bounded attention window (failed /
/// needs-review, up to 50/circuit) for History (steps included).
/// Pending runs are returned by `list_circuit_queue` so none are hidden
/// behind this limit. One command instead of N+1 round-trips.
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
    let (circuits, queue) =
        crate::db::list_circuit_probe(mesh_id, limit).map_err(|e| e.to_string())?;
    let circuits = map_circuit_rows(circuits);
    let queue = map_queue_rows(queue);
    let queue_waits =
        crate::db::circuit::evidence::queue_waits(mesh_id).map_err(|error| error.to_string())?;
    Ok(CircuitProbeSnapshot {
        circuits,
        queue,
        queue_waits,
    })
}

/// Create a circuit with the canonical blueprint.
///
/// Milestone 3 (issue #1208) added the trigger vocabulary: `trigger_kind`
/// selects the root node (see [`CircuitTriggerKind`]). All domain
/// restrictions — the review blueprint's GitHub-issue-label trigger
/// requirement, interval clamp, label trim — live in
/// [`crate::circuit::model::validate_circuit_request`].
/// The `create_circuit` invoke payload. Tauri IPC commands carry every
/// primitive as a separate wire parameter, so this stays flat on the wire
/// (issue #2449) and is read in one piece through `WirePayload` rather than
/// collapsing into a nested payload parameter.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCircuitRequest {
    mesh_id: i64,
    name: String,
    description: String,
    initial_prompt: String,
    /// Every optional field defaults, which is what Tauri's own per-key
    /// extraction does for an `Option` parameter whose key the caller
    /// omitted (`createCircuit` in `src/lib/tauri.ts` omits nothing today,
    /// but a partial payload must not turn into a hard error).
    #[serde(default)]
    trigger_kind: Option<CircuitTriggerKind>,
    #[serde(default)]
    trigger_label: Option<String>,
    #[serde(default)]
    interval_seconds: Option<i64>,
    #[serde(default)]
    blueprint: Option<CircuitBlueprintKind>,
}

/// This Tauri command is a dumb router: parse → call the model →
/// persist → wake the GitHub poll for labelled circuits.
#[command]
pub fn create_circuit(
    payload: crate::commands::wire_payload::WirePayload<CreateCircuitRequest>,
) -> Result<AutopilotCircuit, String> {
    let CreateCircuitRequest {
        mesh_id,
        name,
        description,
        initial_prompt,
        trigger_kind,
        trigger_label,
        interval_seconds,
        blueprint,
    } = payload.into_inner();
    let name = name.trim();
    if name.is_empty() {
        return Err("circuit name must not be empty".to_string());
    }
    let blueprint = blueprint.unwrap_or(CircuitBlueprintKind::WalkingSkeleton);

    let validated = validate_circuit_request(
        blueprint,
        trigger_kind,
        trigger_label.as_deref(),
        interval_seconds,
    )?;
    let kind = trigger_kind_to_node_kind(&validated);

    let graph = match blueprint {
        CircuitBlueprintKind::WalkingSkeleton => {
            CircuitGraph::triggered_skeleton(&initial_prompt, kind.clone())
        }
        CircuitBlueprintKind::IssueDrivenAutopilotReview => {
            // Guarded above: the model rejected every other trigger;
            // we still defensively unwrap the label here.
            let crate::circuit::model::CircuitNodeKind::GithubIssueLabel { label } = &kind else {
                return Err(
                    "the issue-driven Circuit review blueprint requires an issue-label trigger"
                        .to_string(),
                );
            };
            CircuitGraph::issue_driven_autopilot_review(label)
        }
    };
    graph.validate()?;
    let graph_json = graph.to_json()?;
    let circuit = crate::db::create_autopilot_circuit(mesh_id, name, &description, &graph_json)
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
    crate::circuit::stored_graph::upgrade(
        &mut graph,
        crate::circuit::stored_graph::UpgradeTarget::Blueprint,
    );
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
        Err(format!(
            "run was cancelled, but cleanup failed for {}",
            failures.join("; ")
        ))
    }
}

fn retire_cancelled_agents(agent_ids: Vec<i64>) -> Result<(), String> {
    retire_cancelled_agents_with(agent_ids, |agent_id| match crate::db::get_agent_node_by_id(
        agent_id,
    ) {
        Ok(_) => {
            crate::services::agent_node::delete(agent_id, true).map_err(|error| error.to_string())
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(()),
        Err(error) => Err(format!("lookup failed: {}", error)),
    })
}

fn release_circuit_source(run_id: i64) {
    if let Some(source) = crate::db::get_circuit_run(run_id)
        .ok()
        .flatten()
        .and_then(|run| run.source_agent_node_id)
    {
        crate::circuit::evaluator::unregister(source);
    }
}

fn cancel_run_and_cleanup_inner(app: Option<&AppHandle>, run_id: i64) -> Result<(), DbError> {
    crate::services::circuit_worker::mark_circuit_run_cancelled(run_id);
    let agent_ids = crate::db::cancel_circuit_run(run_id).map_err(DbError::from)?;
    crate::services::circuit_worker::finish_circuit_run_cancellation(run_id);
    release_circuit_source(run_id);
    crate::services::circuit_worker::wake_circuit_worker();
    let cleanup = retire_cancelled_agents(agent_ids);
    let state = crate::db::get_circuit_run(run_id)
        .map_err(DbError::from)?
        .map(|run| run.state)
        .unwrap_or_else(|| "cancelled".to_string());
    emit_run_updated(app, run_id, &state);
    cleanup.map_err(DbError::Rendered)
}

/// Cancel a run and retire its helper agents. The desktop command and the
/// mobile route both call this; `app` only decides whether a window is told.
///
/// The failure stays typed: the mobile route answers `503` for a transient
/// pool shortage (issue #1533) rather than the `500` a flattened string
/// would produce.
pub(crate) fn cancel_circuit_run_now(app: Option<&AppHandle>, run_id: i64) -> Result<(), DbError> {
    // Invalidate the worker's current effect batch before waiting for spawn
    // teardown. This closes the race where a slow external effect would
    // otherwise continue after the user pressed Cancel.
    crate::services::circuit_worker::mark_circuit_run_cancelled(run_id);
    match crate::services::circuit_worker::with_circuit_run_spawns_quiesced(run_id, || {
        cancel_run_and_cleanup_inner(app, run_id)
    }) {
        Ok(result) => result,
        Err(wait_error) => {
            // Stop future effects even if a stage-2 spawn exceeded the
            // quiescence window. The spawn's post-launch compensation will
            // retire itself; retaining the terminal ledger makes a retry
            // possible if that OS cleanup is transiently locked.
            if crate::db::cancel_circuit_run(run_id).is_ok() {
                crate::services::circuit_worker::finish_circuit_run_cancellation(run_id);
            }
            release_circuit_source(run_id);
            crate::services::circuit_worker::wake_circuit_worker();
            Err(DbError::Rendered(wait_error))
        }
    }
}

#[command]
pub fn cancel_circuit_run(app: AppHandle, run_id: i64) -> Result<(), String> {
    cancel_circuit_run_now(Some(&app), run_id).map_err(|error| error.to_string())
}

#[command]
pub fn move_circuit_run(run_id: i64, direction: CircuitQueueDirection) -> Result<(), String> {
    match direction {
        CircuitQueueDirection::Up => {
            crate::db::move_queued_circuit_run(run_id, true).map_err(|error| error.to_string())?;
        }
        CircuitQueueDirection::Down => {
            crate::db::move_queued_circuit_run(run_id, false).map_err(|error| error.to_string())?;
        }
        CircuitQueueDirection::Top => {
            crate::db::move_queued_circuit_run_to_edge(run_id, true)
                .map_err(|error| error.to_string())?;
        }
        CircuitQueueDirection::Bottom => {
            crate::db::move_queued_circuit_run_to_edge(run_id, false)
                .map_err(|error| error.to_string())?;
        }
    }
    crate::services::circuit_worker::wake_circuit_worker();
    Ok(())
}

/// Drag-drop / keyboard reorder seam: persist an explicit front-to-back
/// run-id order for one mesh queue. The payload must match the mesh's
/// current pending set exactly; stale or partial payloads abort with a
/// domain error so the UI can refresh instead of half-applying.
#[command]
pub fn reorder_circuit_queue(mesh_id: i64, ordered_run_ids: Vec<i64>) -> Result<(), String> {
    crate::db::reorder_queued_circuit_runs(mesh_id, &ordered_run_ids)?;
    crate::services::circuit_worker::wake_circuit_worker();
    Ok(())
}

/// Bulk cancel for queue / activity hygiene in ONE write transaction:
/// every listed run is terminalised together, the worker is woken once,
/// and a single `circuit-run-updated` event refreshes the Probe once.
/// Missing rows are already gone and skipped without failing the batch.
#[command]
pub fn cancel_circuit_runs(app: AppHandle, run_ids: Vec<i64>) -> Result<(), String> {
    if run_ids.is_empty() {
        return Ok(());
    }
    let mut deduped = run_ids;
    deduped.sort_unstable();
    deduped.dedup();
    // Stop future effects first so slow spawns cannot continue after Cancel.
    for run_id in &deduped {
        crate::services::circuit_worker::mark_circuit_run_cancelled(*run_id);
    }
    // Settle in-flight spawns per run (bounded waits, no wake/emit inside).
    // A timeout does not fail the batch: the DB txn below terminalises the
    // runs anyway and late spawns self-compensate.
    for run_id in &deduped {
        if let Err(wait_error) =
            crate::services::circuit_worker::with_circuit_run_spawns_quiesced(*run_id, || {
                Ok::<(), String>(())
            })
        {
            tracing::warn!(
                "circuits: batch cancel quiescence for run {}: {}",
                run_id,
                wait_error
            );
        }
    }
    let batch = crate::db::cancel_circuit_runs(&deduped).map_err(|error| error.to_string())?;
    for run_id in &deduped {
        crate::services::circuit_worker::finish_circuit_run_cancellation(*run_id);
        // Source unregister is idempotent; the batch txn already collected
        // sources, but per-run release covers rows whose source was set
        // outside the txn window.
        release_circuit_source(*run_id);
    }
    for source in &batch.sources {
        crate::circuit::evaluator::unregister(*source);
    }
    crate::services::circuit_worker::wake_circuit_worker();
    let cleanup = retire_cancelled_agents(batch.agents);
    // One event refreshes the Probe once; the listener ignores the payload
    // and reloads the snapshot.
    let signal_run = batch
        .cancelled
        .first()
        .copied()
        .or(deduped.first().copied())
        .unwrap_or(0);
    let _ = app.emit(
        "circuit-run-updated",
        crate::services::circuit_worker::CircuitRunUpdatedPayload {
            run_id: signal_run,
            state: "cancelled".into(),
        },
    );
    cleanup
}

#[command]
pub fn delete_circuit(app: AppHandle, circuit_id: i64) -> Result<(), String> {
    let circuit = crate::db::get_autopilot_circuit(circuit_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("circuit {} does not exist", circuit_id))?;

    if circuit.is_preset {
        return Err("Built-in Review Blueprints are read-only".into());
    }

    // Stop fresh trigger ingestion before terminalising existing runs. The
    // worker re-checks terminal state before spawning, closing the race with a
    // pass that already loaded this circuit.
    crate::db::set_autopilot_circuit_enabled(circuit_id, false)
        .map_err(|error| error.to_string())?;
    // Invalidate effect batches before waiting for spawn quiescence. Fresh
    // triggers are disabled above, so this snapshot covers every run that
    // the deletion barrier will subsequently clean up.
    if let Ok(run_ids) = crate::db::list_circuit_run_ids_for_cleanup(circuit_id) {
        for run_id in run_ids {
            crate::services::circuit_worker::mark_circuit_run_cancelled(run_id);
        }
    }
    let result = crate::services::circuit_worker::with_circuit_spawns_quiesced(circuit_id, || {
        let run_ids = crate::db::list_circuit_run_ids_for_cleanup(circuit_id)
            .map_err(|error| error.to_string())?;
        let mut cleanup_errors = Vec::new();
        for run_id in run_ids {
            if let Err(error) = cancel_run_and_cleanup_inner(Some(&app), run_id) {
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
                    crate::services::circuit_worker::mark_circuit_run_cancelled(run_id);
                    if crate::db::cancel_circuit_run(run_id).is_ok() {
                        crate::services::circuit_worker::finish_circuit_run_cancellation(run_id);
                    }
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
    // Three-phase discipline (CLAUDE.md hard rule): all DB writes under
    // the writer mutex, then drop the guard before any signal/IO. The
    // worker wake signals a condvar other DB callers can park on; firing
    // it while holding the writer mutex would deadlock any reader pool
    // checkout that races the wake against the next commit.
    let preferences = crate::preferences::load()?;
    let run_id = {
        let mut db = crate::db::write_conn();
        trigger_circuit_now_prepared_locked(&mut db, circuit_id, &preferences)?
    };
    crate::services::circuit_worker::wake_circuit_worker();
    tracing::info!(
        "circuits: manual trigger for circuit {} → run {}",
        circuit_id,
        run_id
    );
    Ok(run_id)
}

/// Per-test isolated variant of [`trigger_circuit_now`] (issue #1691).
/// The public function locks the process-global writer; this helper
/// takes an explicit `&mut Connection` so parallel tests can each
/// operate against their own in-memory DB. Production callers go
/// through the public function; tests use this one.
///
/// Does NOT wake the circuit worker — the caller must drop the writer
/// mutex first so the wake's condvar signal doesn't deadlock concurrent
/// reader-pool checkouts (CLAUDE.md three-phase pattern).
#[cfg(test)]
pub fn trigger_circuit_now_locked(
    conn: &mut rusqlite::Connection,
    circuit_id: i64,
) -> Result<i64, String> {
    trigger_circuit_now_prepared_locked(
        conn,
        circuit_id,
        &crate::preferences::AppPreferences::default(),
    )
}

fn trigger_circuit_now_prepared_locked(
    conn: &mut rusqlite::Connection,
    circuit_id: i64,
    preferences: &crate::preferences::AppPreferences,
) -> Result<i64, String> {
    let circuit = crate::db::circuit::get_autopilot_circuit_inner(conn, circuit_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("circuit {} does not exist", circuit_id))?;
    let graph = CircuitGraph::from_json(&circuit.graph_json)?;
    if graph.requires_source_agent() {
        return Err("Start this Circuit from the source agent's title bar.".into());
    }
    if graph.is_issue_driven_autopilot_review() {
        return Err(
            "issue-driven Circuit review runs are triggered by labelled GitHub issues; Trigger Now requires issue context"
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
    let mut context = crate::circuit::context::CircuitContext::new();
    context.with_circuit(circuit.id, &circuit.name, circuit.mesh_id);
    context.set(
        "review.provider",
        preferences.reviewer_provider.as_deref().unwrap_or(""),
    );
    let action = "draft_pr".to_string();
    context.with_circuit_finish_prompt(None, Some(action.as_str()));
    let run_id = crate::db::circuit::create_circuit_run_prepared_locked(
        conn,
        circuit.id,
        circuit.mesh_id,
        &identity,
        &context.to_json()?,
        preferences,
    )
    .map_err(|e| e.to_string())?;
    Ok(run_id)
}

/// Start a review or an authored Circuit from an agent node's title bar.
///
/// `reviewer_provider` is the optional reviewer provider (a Spawn Option id)
/// picked at the Start Review control; it becomes the run's reviewer provider
/// for the built-in review preset (the reviewer agent spawns on it). Authored
/// Circuits ignore it — their reviewer provider is part of the circuit graph.
/// A blank value means "inherit"; Terminal is rejected (see
/// [`crate::db::circuit::ledger::create_node_circuit_run`]).
///
/// Retrying for an agent that already owns a live run returns that run's id and
/// does not apply the new `reviewer_provider` or `max_rounds` — the first
/// writer wins. The source agent must have a live process and a turn-completion
/// signal, matching the title-bar control's eligibility check.
#[command]
pub fn trigger_circuit_from_node(
    app: AppHandle,
    node_id: i64,
    circuit_id: Option<i64>,
    max_rounds: i32,
    reviewer_provider: Option<String>,
) -> Result<i64, String> {
    trigger_circuit_from_node_now(
        Some(&app),
        node_id,
        circuit_id,
        max_rounds,
        reviewer_provider,
    )
}

pub(crate) fn trigger_circuit_from_node_now(
    app: Option<&AppHandle>,
    node_id: i64,
    circuit_id: Option<i64>,
    max_rounds: i32,
    reviewer_provider: Option<String>,
) -> Result<i64, String> {
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
    let source = crate::agent::provider::SpawnOptionId::from(node.provider.as_str());
    if crate::circuit::compatibility::reviewer_harness_reason(source.harness_id()).is_some() {
        return Err("This agent's harness has no turn-completion signal. Start the review from an agent with a supported turn hook, transcript watcher or result-file handoff.".into());
    }
    let run_id =
        crate::db::create_node_circuit_run(node_id, circuit_id, max_rounds, reviewer_provider)?;
    crate::circuit::evaluator::register_circuit(node_id);
    let state = crate::db::get_circuit_run(run_id)
        .map_err(|e| e.to_string())?
        .map(|run| run.state)
        .unwrap_or_else(|| "pending".into());
    emit_run_updated(app, run_id, &state);
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
            let review_round_limit = {
                // The reader checkout is fallible (issue #1533): a saturated
                // pool is a transient shortage, not a broken command, so the
                // error carries the retryable copy instead of panicking.
                let db = crate::db::read_conn().map_err(|e| e.to_string())?;
                crate::db::circuit::review_rounds::current_limit(&db, &run)
                    .map_err(|e| e.to_string())?
            };
            Ok(CircuitRunDetail {
                run,
                steps,
                review_round_limit,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Human-in-the-loop (#1207): graceful pause/resume + collaborator approval.
// ---------------------------------------------------------------------------

/// Add bounded review rounds to the failed run on its retained work. Resuming the source uses
/// the same lifecycle lease as Archive, so cleanup and process launch cannot
/// own the agent concurrently.
#[command]
pub async fn continue_circuit_review(
    app: AppHandle,
    run_id: i64,
    additional_rounds: i32,
) -> Result<i64, String> {
    if !(1..=10).contains(&additional_rounds) {
        return Err("Additional review rounds must be between 1 and 10.".into());
    }
    if let Some(existing) = crate::commands::run_blocking("existing_review_target", move || {
        crate::db::circuit::recovery::existing_review_target(run_id).map_err(String::from)
    })
    .await?
    {
        return Ok(existing);
    }
    let source_id = crate::commands::run_blocking("review_recovery_source", move || {
        crate::db::circuit::recovery::review_recovery_source(run_id).map_err(String::from)
    })
    .await?;
    let reviewer_id = crate::commands::run_blocking("review_recovery_reviewer", move || {
        crate::db::circuit::recovery::review_recovery_reviewer(run_id).map_err(String::from)
    })
    .await?;
    if let Some(reviewer_id) = reviewer_id {
        if !crate::agent::process::PROCESS_REGISTRY.is_alive(&reviewer_id) {
            use crate::agent::spawn::{ResumeCause, SpawnIntent, SpawnRequest};
            crate::agent::spawn::spawn_with_intent(
                &app,
                SpawnRequest::new(reviewer_id, SpawnIntent::Resume { cause: ResumeCause::Explicit }, Default::default())
                    .with_lifecycle_lease(),
            ).await.map_err(|error| format!("Could not resume the reviewer: {error}. Open the reviewer to resolve this before continuing."))?;
            if !crate::agent::process::PROCESS_REGISTRY.is_alive(&reviewer_id) {
                return Err(
                    "The reviewer is still being stopped or resumed. Try Review again in a moment."
                        .into(),
                );
            }
        }
    }
    if !crate::agent::process::PROCESS_REGISTRY.is_alive(&source_id) {
        use crate::agent::spawn::{ResumeCause, SpawnIntent, SpawnRequest};
        crate::agent::spawn::spawn_with_intent(&app,
            SpawnRequest::new(source_id, SpawnIntent::Resume { cause: ResumeCause::Explicit }, Default::default())
                .with_lifecycle_lease()).await.map_err(|error|
                    format!("Could not resume the implementation agent: {error}. Open the agent to resolve this, or recover from the PR branch and start a new review."))?;
        if !crate::agent::process::PROCESS_REGISTRY.is_alive(&source_id) {
            return Err("The implementation agent is still being stopped or resumed. Try Review again in a moment.".into());
        }
    }
    let (next_id, state) = crate::commands::run_blocking("extend_failed_review", move || {
        let next_id =
            crate::db::circuit::recovery::extend_failed_review(run_id, additional_rounds)?;
        let state = crate::db::get_circuit_run(next_id)
            .map_err(|e| e.to_string())?
            .map(|run| run.state)
            .unwrap_or_else(|| "pending".into());
        Ok((next_id, state))
    })
    .await?;
    crate::circuit::evaluator::register_circuit(source_id);
    if let Some(reviewer_id) = reviewer_id {
        crate::circuit::evaluator::register_circuit(reviewer_id);
    }
    crate::services::circuit_worker::wake_circuit_worker();
    let _ = app.emit(
        "circuit-run-updated",
        crate::services::circuit_worker::CircuitRunUpdatedPayload {
            run_id: next_id,
            state,
        },
    );
    Ok(next_id)
}

/// Update only this run's review allowance; current work keeps running.
#[command]
pub async fn set_circuit_review_round_limit(
    app: AppHandle,
    run_id: i64,
    max_rounds: i32,
) -> Result<(), String> {
    let state = crate::commands::run_blocking("set_circuit_review_round_limit", move || {
        crate::db::circuit::review_rounds::set_review_round_limit(run_id, max_rounds)
            .map_err(String::from)
    })
    .await?;
    crate::services::circuit_worker::wake_circuit_worker();
    emit_run_updated(Some(&app), run_id, &state);
    Ok(())
}

/// Gracefully pause one active run: the graph stops advancing while the
/// current steps finish. Idempotent-friendly (pausing a paused run is a
/// no-op error only if the run isn't active).
#[command]
pub fn pause_circuit_run(run_id: i64) -> Result<(), String> {
    let run = crate::db::get_circuit_run(run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("run {} does not exist", run_id))?;
    if run.state != "running" {
        return Err(format!(
            "only running runs can be paused (run {} is {})",
            run_id, run.state
        ));
    }
    if !crate::db::transition_circuit_run_state(run_id, "running", "paused")
        .map_err(|e| e.to_string())?
    {
        return Err(format!(
            "run {} changed state before it could be paused",
            run_id
        ));
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
        return Err(format!(
            "only paused runs can be resumed (run {} is {})",
            run_id, run.state
        ));
    }
    if !crate::db::transition_circuit_run_state(run_id, "paused", "running")
        .map_err(|e| e.to_string())?
    {
        return Err(format!(
            "run {} changed state before it could be resumed",
            run_id
        ));
    }
    crate::services::circuit_worker::wake_circuit_worker();
    tracing::info!("circuits: run {} resumed", run_id);
    Ok(())
}

/// Approve a CollaboratorCheck gate parked in `blocked` on this run.
#[command]
pub fn approve_circuit_step(run_id: i64, node_id: String) -> Result<(), String> {
    approve_circuit_step_now(run_id, node_id).map_err(|error| error.to_string())
}

/// The approval, keeping the database failure typed.
///
/// The command above flattens the failure to text because that is the Tauri
/// wire type; the mobile route needs to tell a *refused* approval (`409`)
/// from a transient pool shortage (`503`), and text cannot (issue #1533).
pub(crate) fn approve_circuit_step_now(run_id: i64, node_id: String) -> Result<(), DbError> {
    let steps = crate::db::list_circuit_run_steps(run_id).map_err(DbError::from)?;
    let step = steps
        .iter()
        .find(|s| s.node_id == node_id)
        .ok_or_else(|| DbError::Rendered(format!("run {} has no step {}", run_id, node_id)))?;
    if step.status != "blocked" {
        return Err(DbError::Rendered(format!(
            "step {} is not waiting for approval (status {})",
            node_id, step.status
        )));
    }
    crate::services::circuit_worker::request_circuit_approval(run_id, node_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::retire_cancelled_agents_with;

    /// Issue #2449: `create_circuit` reads its eight fields through
    /// `WirePayload` so the Rust signature stops being a positional bag,
    /// and the wire is asserted here so that refactor can never quietly
    /// become an IPC change. This payload is exactly what
    /// `createCircuit` in `src/lib/tauri.ts` sends, `?? null` optionals
    /// included.
    #[test]
    fn create_circuit_still_reads_the_flat_frontend_payload() {
        use super::CreateCircuitRequest;
        use crate::circuit::model::{CircuitBlueprintKind, CircuitTriggerKind};
        let wire = serde_json::json!({
            "meshId": 4,
            "name": "  nightly review  ",
            "description": "review open PRs",
            "initialPrompt": "start",
            "triggerKind": "github_issue_label",
            "triggerLabel": "ready",
            "intervalSeconds": 3600,
            "blueprint": "walking_skeleton",
        });
        let request: CreateCircuitRequest =
            serde_json::from_value(wire).expect("flat invoke payload still parses");
        assert_eq!(request.mesh_id, 4);
        assert_eq!(request.name.trim(), "nightly review");
        assert_eq!(request.description, "review open PRs");
        assert_eq!(request.initial_prompt, "start");
        assert_eq!(
            request.trigger_kind,
            Some(CircuitTriggerKind::GithubIssueLabel)
        );
        assert_eq!(request.trigger_label.as_deref(), Some("ready"));
        assert_eq!(request.interval_seconds, Some(3600));
        assert_eq!(
            request.blueprint,
            Some(CircuitBlueprintKind::WalkingSkeleton)
        );
    }

    /// The frontend wrapper substitutes `null` for absent optionals and
    /// `undefined` keys never reach the wire, so both shapes must land on
    /// `None` rather than failing the whole command.
    #[test]
    fn create_circuit_accepts_null_and_omitted_optional_fields() {
        use super::CreateCircuitRequest;
        let nulled: CreateCircuitRequest = serde_json::from_value(serde_json::json!({
            "meshId": 1,
            "name": "n",
            "description": "",
            "initialPrompt": "",
            "triggerKind": "manual",
            "triggerLabel": null,
            "intervalSeconds": null,
            "blueprint": "walking_skeleton",
        }))
        .expect("null optionals parse");
        assert_eq!(nulled.trigger_label, None);
        assert_eq!(nulled.interval_seconds, None);

        let omitted: CreateCircuitRequest = serde_json::from_value(serde_json::json!({
            "meshId": 1,
            "name": "n",
            "description": "",
            "initialPrompt": "",
        }))
        .expect("omitted optionals default to None");
        assert_eq!(omitted.trigger_kind, None);
        assert_eq!(omitted.blueprint, None);
    }

    #[test]
    fn saving_a_legacy_graph_applies_the_full_upgrade_sequence_and_preserves_authored_text() {
        use crate::circuit::model::{CircuitGraph, CircuitNodeKind};
        let _db = crate::db::test_support::isolated();
        let mesh = crate::db::create_mesh("legacy save", "C:/legacy-save").unwrap();
        let mut graph = crate::circuit::test_support::stuck_issue_review();
        if let CircuitNodeKind::InjectPty { prompt, .. } = &mut graph
            .nodes
            .iter_mut()
            .find(|node| node.id == "follow_feedback")
            .unwrap()
            .kind
        {
            *prompt = "Apply our team's review checklist.".into();
        }
        let circuit_id = {
            let conn = crate::db::write_conn();
            conn.execute(
                "INSERT INTO autopilot_circuits (mesh_id, name, graph_json) VALUES (?1, 'legacy', ?2)",
                rusqlite::params![mesh.id, graph.to_json().unwrap()],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        super::update_circuit_graph(circuit_id, graph.to_json().unwrap()).unwrap();
        let saved = crate::db::get_autopilot_circuit(circuit_id)
            .unwrap()
            .unwrap()
            .graph_json;
        let upgraded = CircuitGraph::from_json(&saved).unwrap();
        upgraded.validate().unwrap();
        assert!(
            upgraded.has_review_topology_of(&CircuitGraph::issue_driven_autopilot_review(
                "buildmesh:run"
            ))
        );
        assert!(upgraded.node("merge_verify").is_some());
        assert!(matches!(
            &upgraded.node("follow_feedback").unwrap().kind,
            CircuitNodeKind::InjectPty { prompt, .. }
                if prompt == "Apply our team's review checklist."
        ));
        super::update_circuit_graph(circuit_id, saved.clone()).unwrap();
        assert_eq!(
            crate::db::get_autopilot_circuit(circuit_id)
                .unwrap()
                .unwrap()
                .graph_json,
            saved
        );
    }

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

    /// #1910: the built-in Review Blueprint is present and inspectable but is
    /// never a trigger source of its own — a run only exists once a user
    /// starts it from an implementation agent's title bar. Trigger Now is the
    /// one IPC path that would otherwise mint a run on an unattended row.
    #[test]
    fn the_built_in_review_blueprint_cannot_be_triggered_on_its_own() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        let mesh =
            crate::db::create_mesh_inner(&conn, "trigger guard", "/tmp/trigger-guard").unwrap();
        crate::db::circuit::ledger::ensure_review_blueprint_inner(&conn, mesh.id).unwrap();
        let mut conn = conn;
        let blueprint =
            crate::db::circuit::list_circuits_with_recent_runs_inner(&conn, mesh.id, 10).unwrap()
                [0]
            .0
            .id;

        assert_eq!(
            super::trigger_circuit_now_locked(&mut conn, blueprint).unwrap_err(),
            "Start this Circuit from the source agent's title bar."
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM autopilot_circuit_runs", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "the refused trigger left no run behind"
        );
    }
}
