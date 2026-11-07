//! Circuit IPC surface (spec #1205 / walking skeleton #1206).
//!
//! Minimal milestone-1 contract: list / create / enable / delete
//! circuits, Trigger Now, and read back runs with their step ledger. The
//! walking-skeleton blueprint is built server-side
//! ([`CircuitGraph::walking_skeleton`]) so the AST stays canonical in
//! Rust — the throwaway Probe-tab authoring only sends a name and a
//! prompt.
//!
//! This module is an **adapter**, not the owner. The Circuit control
//! workflows live in [`crate::services::circuit_control`], so the mobile HTTP
//! routes and this file are two doors onto one implementation (issue #2673).
//! What stays here is what only a transport needs: the wire DTO declarations
//! (ADR 0047 keeps them here so the generated TypeScript and the ts-rs
//! export names do not move), the offload onto the blocking pool, and the
//! flattening of a typed failure into the Tauri wire's `String`.

use tauri::{command, AppHandle, Emitter};

use crate::circuit::model::{trigger_kind_to_node_kind, validate_circuit_request, CircuitGraph};
pub use crate::circuit::model::{CircuitBlueprintKind, CircuitTriggerKind};
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
    /// Absent on older servers; clients cannot infer permission from context.
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_continuation: Option<crate::circuit::review_continuation::ReviewContinuation>,
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

#[command]
pub async fn record_circuit_outcome(
    app: AppHandle,
    request: crate::db::circuit::evidence::CheckpointRequest,
) -> Result<(), String> {
    crate::blocking::run_blocking("record_circuit_outcome", move || {
        crate::services::circuit_control::record_outcome(Some(&app), &request).map_err(String::from)
    })
    .await
}

#[command]
pub async fn circuit_prompt_payload(
    request: crate::db::circuit::evidence::prompt_delivery::CircuitPromptRequest,
) -> Result<String, String> {
    crate::blocking::run_blocking("circuit_prompt_payload", move || {
        crate::services::circuit_prompt::payload(&request).map_err(String::from)
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
    crate::services::circuit_control::push_step(Some(&app), &request)
        .map_err(|error| error.to_string())
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
    crate::services::circuit_control::recover_run(Some(&app), &request)
        .map_err(|error| error.to_string())
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
    crate::services::circuit_control::list_agent_ownerships().map_err(|error| error.to_string())
}

#[command]
pub fn list_circuits(mesh_id: i64) -> Result<Vec<AutopilotCircuit>, String> {
    crate::services::circuit_control::list_circuits(mesh_id).map_err(|e| e.to_string())
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
    crate::services::circuit_control::list_circuits_with_runs(mesh_id, limit)
        .map_err(|e| e.to_string())
}

#[command]
pub fn list_circuit_queue(mesh_id: i64) -> Result<Vec<CircuitQueueEntry>, String> {
    crate::services::circuit_control::list_queue(mesh_id).map_err(|error| error.to_string())
}

#[command]
pub fn list_circuit_probe(
    mesh_id: i64,
    limit: Option<i64>,
) -> Result<CircuitProbeSnapshot, String> {
    crate::services::circuit_control::list_probe(mesh_id, limit).map_err(|error| error.to_string())
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

#[command]
pub fn cancel_circuit_run(app: AppHandle, run_id: i64) -> Result<(), String> {
    crate::services::circuit_control::cancel_run(Some(&app), run_id)
        .map_err(|error| error.to_string())
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
    crate::services::circuit_control::cancel_runs(Some(&app), run_ids)
}

#[command]
pub fn delete_circuit(app: AppHandle, circuit_id: i64) -> Result<(), String> {
    crate::services::circuit_control::delete_circuit(Some(&app), circuit_id)
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
    // checkout that races the wake against the next commit. The service
    // owns the connection and releases it before it returns.
    let preferences = crate::preferences::load()?;
    let run_id = crate::services::circuit_trigger::trigger_circuit_now(circuit_id, &preferences)?;
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
// allow-test-in-production: two suites call it — this file's own suite and the DB
// suite — so it stays a `pub` item here rather than a sibling test file's helper.
pub fn trigger_circuit_now_locked(
    conn: &mut rusqlite::Connection,
    circuit_id: i64,
) -> Result<i64, String> {
    crate::services::circuit_trigger::trigger_circuit_now_prepared_locked(
        conn,
        circuit_id,
        &crate::preferences::AppPreferences::default(),
    )
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
    reviewer_login_id: Option<String>,
) -> Result<i64, String> {
    let store = crate::preferences::default_store();
    crate::services::circuit_control::start_review(
        store.as_ref(),
        Some(&app),
        node_id,
        circuit_id,
        max_rounds,
        reviewer_provider,
        reviewer_login_id,
    )
}

/// Recent runs for one circuit, newest first, each with its step ledger.
#[command]
pub fn list_circuit_runs(
    circuit_id: i64,
    limit: Option<i64>,
) -> Result<Vec<CircuitRunDetail>, String> {
    crate::services::circuit_control::list_runs(circuit_id, limit).map_err(String::from)
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
    crate::services::circuit_control::continue_review(&app, run_id, additional_rounds).await
}

/// Update only this run's review allowance; current work keeps running.
#[command]
pub async fn set_circuit_review_round_limit(
    app: AppHandle,
    run_id: i64,
    max_rounds: i32,
) -> Result<(), String> {
    crate::services::circuit_control::set_round_limit(Some(&app), run_id, max_rounds).await
}

/// Gracefully pause one active run: the graph stops advancing while the
/// current steps finish. Idempotent-friendly (pausing a paused run is a
/// no-op error only if the run isn't active).
#[command]
pub fn pause_circuit_run(run_id: i64) -> Result<(), String> {
    crate::services::circuit_control::pause_run(run_id).map_err(|error| error.to_string())
}

/// Resume one paused run; automation picks up exactly where it stopped.
#[command]
pub fn resume_circuit_run(run_id: i64) -> Result<(), String> {
    crate::services::circuit_control::resume_run(run_id).map_err(|error| error.to_string())
}

/// Approve a CollaboratorCheck gate parked in `blocked` on this run.
#[command]
pub fn approve_circuit_step(run_id: i64, node_id: String) -> Result<(), String> {
    crate::services::circuit_control::approve_step(run_id, node_id)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "circuit_tests.rs"]
mod tests;
