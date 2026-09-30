//! Durable external-effect claims and append-only Circuit Run History.

use crate::db::SqlResult;
use rusqlite::{params, Connection, OptionalExtension};

const OBSERVATION_FRESHNESS_REJECTION_PREFIX: &str = "Observation freshness fence rejected:";

pub(crate) fn restore_projection_conflicts(
    run_id: i64, node_id: &str, attempt: i32,
    evidence: &mut crate::autopilot::circuit::observation::WorkEvidence,
) -> SqlResult<bool> {
    restore_projection_conflicts_inner(&crate::db::read_conn(), run_id, node_id, attempt, evidence)
}

fn restore_projection_conflicts_inner(
    db: &Connection, run_id: i64, node_id: &str, attempt: i32,
    evidence: &mut crate::autopilot::circuit::observation::WorkEvidence,
) -> SqlResult<bool> {
    let mut query = db.prepare("SELECT detail FROM circuit_run_history
        WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='observation'
          AND source='agent_status_projection' AND disposition='conflicting'")?;
    let rows = query.query_map(params![run_id, node_id, attempt], |row| row.get::<_, String>(0))?;
    let mut changed = false;
    for row in rows {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&row?) else { continue; };
        if let Some(observation) = value.get("observation").cloned()
            .and_then(|value| serde_json::from_value(value).ok()) {
            changed |= evidence.restore_projection_conflict(&observation);
        }
    }
    Ok(changed)
}

// ---------------------------------------------------------------------------
// Circuit Run History provenance vocabulary (issue #1909 / #1847).
//
// Every append carries a `source` (who/what produced the event) and a
// `disposition` (what Buildmesh recorded doing with it), alongside the
// existing identity (`node_id`/`attempt`) and time (`observed_at`). This is
// the single vocabulary: the Probe renders these values and tests pin them,
// so add here rather than inlining literals at call sites.
// ---------------------------------------------------------------------------

/// The circuit worker's stepper committed a run/step transition or effect.
pub(super) const SOURCE_CIRCUIT_WORKER: &str = "circuit_worker";
/// The worker's admission gate parked a pending run on a capacity reason.
pub(super) const SOURCE_ADMISSION: &str = "circuit_worker.admission";
/// The worker's step-capacity gate parked a step.
pub(super) const SOURCE_CAPACITY: &str = "circuit_worker.capacity";
/// The worker reconciled an evidence window against its projection.
pub(super) const SOURCE_RECONCILIATION: &str = "circuit_worker.reconciliation";
/// The run pinned its configuration snapshot.
pub(super) const SOURCE_RUN_CONFIGURATION: &str = "run.configuration";
/// A native harness hook receipt.
pub(super) const SOURCE_NATIVE_HOOK: &str = "native_hook";
/// A Buildmesh PTY prompt submission awaiting a harness acknowledgement
/// (issue #1898).
pub(super) const SOURCE_PROMPT_SUBMISSION: &str = "prompt_submission";
/// The report classifier interpreted a report.
pub(super) const SOURCE_CLASSIFIER: &str = "classifier";
/// A GitHub read-only lookup or recorded action target.
pub(super) const SOURCE_GITHUB: &str = "github";
/// An operator action: attestation, evidence recheck, or continuation.
pub(super) const SOURCE_OPERATOR: &str = "operator";

/// A committed projection or operator action took effect.
pub(super) const DISPOSITION_APPLIED: &str = "applied";
/// An unresolved wait (admission, step capacity, or evidence window).
pub(super) const DISPOSITION_WAITING: &str = "waiting";
/// A wait whose binding window cleared — no longer active (issue #1909).
pub(super) const DISPOSITION_RESOLVED: &str = "resolved";
pub(super) const DISPOSITION_INTENT: &str = "intent";
pub(super) const DISPOSITION_POSSIBLE_DISPATCH: &str = "possible_dispatch";
pub(super) const DISPOSITION_ACKNOWLEDGED: &str = "acknowledged";
pub(super) const DISPOSITION_RECONCILED: &str = "reconciled";
pub(super) const DISPOSITION_RECORDED: &str = "recorded";
pub(super) const DISPOSITION_RECEIVED: &str = "received";
pub(super) const DISPOSITION_INTERPRETED: &str = "interpreted";
pub(super) const DISPOSITION_REQUESTED: &str = "requested";

/// Buildmesh's recorded disposition for one observation, matching the
/// snake_case serde form of [`ObservationDisposition`].
fn observation_disposition_str(
    value: crate::autopilot::circuit::observation::ObservationDisposition,
) -> &'static str {
    use crate::autopilot::circuit::observation::ObservationDisposition as D;
    match value {
        D::Accepted => "accepted",
        D::ReducedConfidence => "reduced_confidence",
        D::Duplicate => "duplicate",
        D::Rejected => "rejected",
        D::Unavailable => "unavailable",
        D::Conflicting => "conflicting",
    }
}

/// A wait event's disposition: `waiting` while its window still binds,
/// `resolved` once it clears. Without this, a freed capacity/evidence window
/// would be recorded as if the wait were still active (issue #1909).
fn wait_disposition(binding: bool) -> &'static str {
    if binding { DISPOSITION_WAITING } else { DISPOSITION_RESOLVED }
}

pub(crate) fn is_observation_freshness_rejection(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::InvalidParameterName(message)
        if message.starts_with(OBSERVATION_FRESHNESS_REJECTION_PREFIX))
}

fn observation_freshness_rejection(reason: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(format!(
        "{OBSERVATION_FRESHNESS_REJECTION_PREFIX} {reason}"
    ))
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitHistoryEntry.ts")]
pub struct CircuitHistoryEntry {
    #[ts(as = "i32")]
    pub id: i64,
    pub node_id: Option<String>,
    pub attempt: Option<i32>,
    pub kind: String,
    pub detail: String,
    /// Who/what produced the event (issue #1909 / #1847). Null on pre-v45 rows.
    pub source: Option<String>,
    /// What Buildmesh recorded doing with the event. Null on pre-v45 rows.
    pub disposition: Option<String>,
    pub observed_at: String,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitCheckpoint.ts")]
pub struct CircuitCheckpoint {
    pub node_id: String,
    pub attempt: i32,
    pub actions: Vec<CheckpointAction>,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitEvidenceView.ts")]
pub struct CircuitEvidenceView {
    pub entries: Vec<CircuitHistoryEntry>,
    pub checkpoints: Vec<CircuitCheckpoint>,
    pub coverage: Vec<CircuitStepObservationCoverage>,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitStepObservationCoverage.ts")]
pub struct CircuitStepObservationCoverage {
    pub node_id: String,
    pub attempt: i32,
    pub platform: String,
    pub capabilities: crate::services::circuit_worker::observer_policy::CircuitObserverCapabilities,
    #[ts(as = "Option<i32>")]
    pub deadline_ms: Option<i64>,
    pub waits_active: bool,
    pub human_waits: Vec<crate::autopilot::circuit::observation::HumanWait>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub observation_blocker: Option<crate::autopilot::circuit::observation::CircuitObservationBlocker>,
}

pub fn history(run_id: i64) -> Result<CircuitEvidenceView, String> {
    let db = crate::db::read_conn();
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let view = evidence_view_inner(&tx, run_id)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(view)
}

fn recovery_view(db: &Connection, run: &crate::models::AutopilotCircuitRun) -> Result<crate::autopilot::circuit::stepper::RunView, String> {
    use crate::autopilot::circuit::{context::CircuitContext, model::StepOutcome, stepper::{RunState, RunView, StepStatus, StepView}};
    let steps = super::ledger::list_circuit_run_steps_inner(db, run.id).map_err(|e| e.to_string())?;
    Ok(RunView { run_id:run.id, graph:run_graph(db, run.id)?, state:RunState::from_db_str(&run.state),
        context:CircuitContext::from_json(&run.context_json)?,
        steps:steps.into_iter().map(|step| StepView { node_id:step.node_id, attempt:step.attempt,
            status:StepStatus::from_db_str(&step.status), outcome:step.outcome.as_deref().and_then(StepOutcome::from_db_str),
            error:step.error_message, agent_node_id:step.agent_node_id }).collect() })
}

fn evidence_view_inner(db: &Connection, run_id: i64) -> Result<CircuitEvidenceView, String> {
    let entries = history_inner(db, run_id).map_err(|e| e.to_string())?;
    let run = super::ledger::get_circuit_run_inner(db, run_id)
        .map_err(|e| e.to_string())?
        .ok_or("Run no longer exists.")?;
    let mut checkpoints = Vec::new();
    let mut coverage = Vec::new();
    {
        let graph = run_graph(db, run_id)?;
        let context = crate::autopilot::circuit::context::CircuitContext::from_json(&run.context_json)?;
        let steps = super::ledger::list_circuit_run_steps_inner(db, run_id).map_err(|e| e.to_string())?;
        let view = recovery_view(db, &run)?;
        for step in steps {
            if let Some(agent_id) = step.agent_node_id.or_else(|| view.resolve_target_agent(&step.node_id)) {
                if let Some(agent) = crate::db::agent_node::get_agent_node_by_id_inner(db, agent_id).optional().map_err(|e| e.to_string())? {
                    let deadline_ms = view.evidence_deadline_ms(&step.node_id);
                    coverage.push(CircuitStepObservationCoverage {
                        node_id: step.node_id.clone(), attempt: step.attempt,
                        platform: format!("{} host / {} launch", std::env::consts::OS, agent.env),
                        capabilities: crate::services::circuit_worker::observer_policy::for_agent(&agent), deadline_ms,
                        observation_blocker: (step.status == "unverified").then(|| context.get(&format!("node.{}.observation_blocker", step.node_id))
                            .and_then(|json| serde_json::from_str(json).ok())).flatten(),
                        waits_active: run.state == "running" && !matches!(step.status.as_str(), "completed" | "failed" | "cancelled"),
                        human_waits: context.get(&format!("node.{}.evidence.{}", step.node_id, step.attempt))
                            .and_then(|json| serde_json::from_str::<crate::autopilot::circuit::observation::WorkEvidence>(json).ok())
                            .map(|evidence| evidence.human_waits.into_iter().filter(|wait| wait.source != "agent_status_projection").collect()).unwrap_or_default(),
                    });
                }
            }
            if run.state != "running" || step.status != "unverified" {
                continue;
            }
            use crate::autopilot::circuit::model::CircuitNodeKind;
            let actions = match graph.node(&step.node_id).map(|n| &n.kind) {
                Some(
                    kind @ (CircuitNodeKind::GithubAction { .. }
                    | CircuitNodeKind::InjectPty { .. }),
                ) => {
                    let not_performed: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM circuit_effects WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND state='not_performed')",
                        params![run_id,step.node_id,step.attempt], |r| r.get(0)).map_err(|e| e.to_string())?;
                    let mut actions = vec![CheckpointAction::NotPerformed];
                    if matches!(
                        kind,
                        CircuitNodeKind::GithubAction {
                            action: crate::autopilot::circuit::model::GithubActionKind::OpenPr,
                            ..
                        }
                    ) {
                        let has_target = has_effect_target(db, run_id, &step.node_id, step.attempt)
                            .map_err(|error| error.to_string())?;
                        if has_target {
                            actions.insert(0, CheckpointAction::Recheck);
                        }
                    } else {
                        actions.insert(0, CheckpointAction::Completed);
                    }
                    if not_performed {
                        actions.push(CheckpointAction::Retry);
                    }
                    actions
                }
                Some(CircuitNodeKind::SpawnAgentNode { .. }) if step.agent_node_id.is_none() => {
                    let not_performed: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM circuit_effects WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='spawn' AND state='not_performed')",
                        params![run_id,step.node_id,step.attempt], |row| row.get(0)).map_err(|e| e.to_string())?;
                    let mut actions = vec![CheckpointAction::Recheck, CheckpointAction::NotPerformed];
                    if not_performed { actions.push(CheckpointAction::Retry); }
                    actions
                }
                Some(
                    CircuitNodeKind::LlmTurnClassifier { .. }
                    | CircuitNodeKind::ReviewVerdict { .. }
                    | CircuitNodeKind::AwaitAgentTurn { .. }
                    | CircuitNodeKind::SpawnAgentNode { .. },
                ) => {
                    let mut actions = vec![CheckpointAction::Recheck];
                    if view.can_attest_completion(&step.node_id) {
                        actions.push(CheckpointAction::Completed);
                    }
                    actions
                },
                _ => vec![],
            };
            checkpoints.push(CircuitCheckpoint {
                node_id: step.node_id,
                attempt: step.attempt,
                actions,
            });
        }
    }
    Ok(CircuitEvidenceView {
        entries,
        checkpoints,
        coverage,
    })
}

fn history_inner(db: &Connection, run_id: i64) -> SqlResult<Vec<CircuitHistoryEntry>> {
    let mut stmt = db.prepare("SELECT id,node_id,attempt,kind,detail,source,disposition,observed_at FROM circuit_run_history WHERE run_id=?1 ORDER BY id")?;
    let rows = stmt
        .query_map([run_id], |r| {
            Ok(CircuitHistoryEntry {
                id: r.get(0)?,
                node_id: r.get(1)?,
                attempt: r.get(2)?,
                kind: r.get(3)?,
                detail: r.get(4)?,
                source: r.get(5)?,
                disposition: r.get(6)?,
                observed_at: r.get(7)?,
            })
        })?
        .collect();
    rows
}

pub(crate) fn receive_native_hook(
    receipt: &crate::services::circuit_worker::native_hooks::NativeReceipt,
) -> Result<(), String> {
    let mut db = crate::db::write_conn();
    receive_native_hook_locked(&mut db, receipt)
}

/// A Buildmesh prompt submission awaiting a harness acknowledgement
/// (issue #1898).
///
/// `submission_seq` is a per-agent-node ordinal assigned inside the writing
/// transaction. It is the *ordering* half of the submission proof: a receipt
/// may only claim the newest submission, so a turn whose start hook arrived
/// after Buildmesh had already submitted again can never acknowledge the
/// later one. It is stored in the record rather than in process memory
/// because the record is written *before* the PTY write — the harness can
/// report a prompt within milliseconds of Enter, and there must be no window
/// in which Buildmesh has written to the terminal but has no record to match
/// the report against.
///
/// The prompt text is deliberately absent: only `prompt_digest` is kept, and
/// the text is never written to the ledger.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct PromptSubmission {
    pub agent_node_id: i64,
    pub submission_seq: i64,
    pub prompt_digest: String,
}

/// Record a Buildmesh prompt submission for a Circuit step. Callers must
/// invoke this **before** writing the prompt into the PTY so the record
/// exists by the time the harness can echo the prompt back.
pub(crate) fn record_prompt_submission(
    run_id: i64,
    node_id: &str,
    attempt: i32,
    agent_node_id: i64,
    prompt: &str,
) -> Result<i64, String> {
    let db = crate::db::write_conn();
    record_prompt_submission_locked(&db, run_id, node_id, attempt, agent_node_id, prompt)
}

pub(crate) fn record_prompt_submission_locked(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    agent_node_id: i64,
    prompt: &str,
) -> Result<i64, String> {
    // The ordinal is per agent node, not per run: the fence that matters is
    // "has Buildmesh submitted to this terminal again", which is a property
    // of the PTY rather than of any one run. Allocating it under the same
    // writer transaction as the insert keeps concurrent runs from minting
    // the same ordinal.
    let previous = newest_submission_seq(db, agent_node_id)?;
    let submission = PromptSubmission {
        agent_node_id,
        submission_seq: previous + 1,
        prompt_digest: crate::services::circuit_worker::native_hooks::submission_digest(prompt),
    };
    append_history(
        db,
        run_id,
        Some(node_id),
        Some(attempt),
        "prompt_submitted",
        &serde_json::to_string(&submission).map_err(|e| e.to_string())?,
        Some(SOURCE_PROMPT_SUBMISSION),
        Some(DISPOSITION_RECORDED),
    )
    .map_err(|e| e.to_string())?;
    // This history entry advances the same revision used by transition fences.
    // Return it under the writer lock so the dispatcher's own write cannot
    // invalidate its subsequent PromptDelivered transition.
    revision_inner(db, run_id).map_err(|e| e.to_string())
}

fn newest_submission_seq(db: &Connection, agent_node_id: i64) -> Result<i64, String> {
    db.query_row(
        "SELECT COALESCE(MAX(json_extract(detail,'$.submission_seq')),0)
         FROM circuit_run_history
         WHERE kind='prompt_submitted' AND json_extract(detail,'$.agent_node_id')=?1",
        params![agent_node_id],
        |row| row.get(0),
    )
    .map_err(|e| e.to_string())
}

/// What a receipt established about the Buildmesh submission its turn belongs
/// to: the input stamp the binding was made against, and the ordinal of the
/// recorded submission that earned it. Both are `None` when nothing was
/// provably bound.
///
/// The two halves travel together through every path. A terminal receipt that
/// inherited the stamp but dropped the ordinal would break the ledger's own
/// contract — it could no longer answer "which input completed here?" without
/// re-deriving it from the turn start (issue #1898 review).
type SubmissionBinding = (Option<String>, Option<i64>);

/// The binding a previously recorded, correlated turn start established for
/// this receipt's turn token. `Stop` carries the token but not the prompt
/// text, so it can only inherit a binding made when the turn began.
fn recorded_turn_binding(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    receipt: &crate::services::circuit_worker::native_hooks::NativeReceipt,
) -> Result<Option<SubmissionBinding>, String> {
    db.query_row(
        "SELECT json_extract(detail,'$.input_stamp'),
                json_extract(detail,'$.submission_seq')
         FROM circuit_run_history
         WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='native_hook_received'
         AND json_extract(detail,'$.agent_node_id')=?4
         AND json_extract(detail,'$.session_incarnation') IS ?5
         AND json_extract(detail,'$.hook.session_id') IS ?6
         AND json_extract(detail,'$.hook.turn_id') IS ?7
         AND json_extract(detail,'$.hook.event')='UserPromptSubmit'
         AND json_extract(detail,'$.submission_correlated')=1
         ORDER BY id LIMIT 1",
        params![
            run_id,
            node_id,
            attempt,
            receipt.agent_node_id,
            receipt.session_incarnation,
            receipt.hook.session_id,
            receipt.hook.turn_id
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(|e| e.to_string())
}

/// Decide whether a `UserPromptSubmit` receipt provably acknowledges a
/// recorded Buildmesh submission (issue #1898).
///
/// `UserPromptSubmit` is the only event carrying both halves of Claude Code's
/// native submission contract: the verbatim `prompt` text the harness says it
/// received, and the `prompt_id` turn token naming the turn it is starting.
/// A turn token alone proves nothing about *which* Buildmesh input it belongs
/// to, so the token is bound only when all of the following hold inside one
/// (run, step, attempt, agent) scope:
///
/// 1. The receipt is stamped with a current input stamp, so Buildmesh can
///    still tell which submission is live.
/// 2. The harness's prompt digest equals a recorded submission's digest — the
///    harness received byte-for-byte the text Buildmesh wrote.
/// 3. That submission is the newest one for this agent, so no later
///    Buildmesh submission has been made since. This is what stops a delayed
///    start hook from claiming the submission that replaced it.
/// 4. No *other* turn has already claimed that same submission, so two turns
///    can never both acknowledge one submission while a later submission
///    still gets its own turn.
///
/// Returns `None` for every refusal. The receipt is still recorded, replayed
/// and presented — as reduced-confidence evidence that cannot complete a
/// step. That is the explicit unavailable path: a missing `prompt_id`
/// (Claude Code before v2.1.196), a missing or transformed `prompt`, a
/// superseded submission, or an ambiguous claim all land here.
fn earn_turn_binding(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    receipt: &crate::services::circuit_worker::native_hooks::NativeReceipt,
) -> Result<Option<SubmissionBinding>, String> {
    let hook = &receipt.hook;
    let (Some(turn_id), Some(digest)) = (hook.turn_id.as_deref(), hook.prompt_digest.as_deref())
    else {
        return Ok(None);
    };
    let Some(stamp) = receipt.input_stamp.clone() else {
        return Ok(None);
    };
    // Rule 2.
    let submission: Option<PromptSubmission> = db
        .query_row(
            "SELECT detail FROM circuit_run_history
             WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='prompt_submitted'
             AND json_extract(detail,'$.agent_node_id')=?4
             AND json_extract(detail,'$.prompt_digest')=?5
             ORDER BY json_extract(detail,'$.submission_seq') DESC LIMIT 1",
            params![run_id, node_id, attempt, receipt.agent_node_id, digest],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .and_then(|detail| serde_json::from_str(&detail).ok());
    let Some(submission) = submission else { return Ok(None) };
    // Rule 3.
    if submission.submission_seq != newest_submission_seq(db, receipt.agent_node_id)? {
        return Ok(None);
    }
    // Rule 4, plus idempotency: read the turns that have already claimed
    // *this* submission, under the same session generation. A prior
    // generation's claim cannot block a re-submission into a restarted
    // session, and a later submission starts with an empty claim set.
    let claimed: Vec<String> = {
        let mut stmt = db.prepare(
            "SELECT json_extract(detail,'$.hook.turn_id') FROM circuit_run_history
             WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='native_hook_received'
             AND json_extract(detail,'$.agent_node_id')=?4
             AND json_extract(detail,'$.session_incarnation') IS ?5
             AND json_extract(detail,'$.hook.event')='UserPromptSubmit'
             AND json_extract(detail,'$.submission_correlated')=1
             AND json_extract(detail,'$.submission_seq')=?6",
        ).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                params![
                    run_id,
                    node_id,
                    attempt,
                    receipt.agent_node_id,
                    receipt.session_incarnation,
                    submission.submission_seq
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(|e| e.to_string())?;
        rows.collect::<SqlResult<Vec<_>>>().map_err(|e| e.to_string())?
    };
    if !claimed.iter().any(|turn| turn == turn_id) && !claimed.is_empty() {
        return Ok(None);
    }
    Ok(Some((Some(stamp), Some(submission.submission_seq))))
}

pub(crate) fn receive_native_hook_locked(
    db: &mut Connection,
    receipt: &crate::services::circuit_worker::native_hooks::NativeReceipt,
) -> Result<(), String> {
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let run_ids = {
        let mut stmt = tx.prepare("SELECT id FROM autopilot_circuit_runs r WHERE r.state IN ('running','paused')
            AND (r.source_agent_node_id=?1 OR EXISTS (SELECT 1 FROM autopilot_circuit_run_steps s WHERE s.run_id=r.id AND s.agent_node_id=?1))").map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([receipt.agent_node_id], |r| r.get::<_, i64>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<SqlResult<Vec<_>>>()
            .map_err(|e| e.to_string())?
    };
    let mut targets = Vec::new();
    for run_id in run_ids {
        use crate::autopilot::circuit::{
            context::CircuitContext,
            model::StepOutcome,
            stepper::{RunState, RunView, StepStatus, StepView},
        };
        let run = super::ledger::get_circuit_run_inner(&tx, run_id)
            .map_err(|e| e.to_string())?
            .ok_or("Run no longer exists")?;
        let rows =
            super::ledger::list_circuit_run_steps_inner(&tx, run_id).map_err(|e| e.to_string())?;
        let view = RunView {
            run_id,
            state: RunState::from_db_str(&run.state),
            graph: run_graph(&tx, run_id)?,
            context: CircuitContext::from_json(&run.context_json)?,
            steps: rows
                .into_iter()
                .map(|s| StepView {
                    node_id: s.node_id,
                    attempt: s.attempt,
                    status: StepStatus::from_db_str(&s.status),
                    outcome: s.outcome.as_deref().and_then(StepOutcome::from_db_str),
                    error: s.error_message,
                    agent_node_id: s.agent_node_id,
                })
                .collect(),
        };
        for step in &view.steps {
            if matches!(step.status, StepStatus::Running | StepStatus::Unverified)
                && step
                    .agent_node_id
                    .or_else(|| view.resolve_target_agent(&step.node_id))
                    == Some(receipt.agent_node_id)
            {
                targets.push((run_id, step.node_id.clone(), step.attempt));
            }
        }
    }
    for (run_id, node_id, attempt) in targets {
        let mut receipt = receipt.clone();
        // Receipt arrival can follow a newer PTY submission, so a terminal
        // hook may only claim input through the persisted turn-start binding
        // (issue #1898). A `UserPromptSubmit` receipt is the only place a
        // turn token is ever earned, and only from the harness prompt echo
        // plus submission ordering — never from arrival order.
        let binding: Option<SubmissionBinding> =
            if receipt.hook.event == "UserPromptSubmit" {
                earn_turn_binding(&tx, run_id, &node_id, attempt, &receipt)?
            } else {
                recorded_turn_binding(&tx, run_id, &node_id, attempt, &receipt)?
            };
        match binding {
            Some((stamp, seq)) => {
                receipt.submission_correlated = stamp.is_some();
                receipt.input_stamp = stamp;
                // A terminal receipt records the same ordinal its turn start
                // bound, so the ledger answers "which input completed here?"
                // without re-deriving it (issue #1898 review).
                receipt.submission_seq = seq;
            }
            None => {
                // Uncorrelated: drop the receipt-time input stamp entirely so
                // no downstream freshness check can read authority from it.
                receipt.submission_correlated = false;
                receipt.input_stamp = None;
                receipt.submission_seq = None;
            }
        }
        let detail = serde_json::to_string(&receipt).map_err(|e| e.to_string())?;
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM circuit_run_history WHERE run_id=?1 AND node_id=?2 AND attempt=?3
            AND kind='native_hook_received' AND json_extract(detail,'$.source_id')=?4)",
            params![run_id,node_id,attempt,receipt.source_id], |r| r.get(0)).map_err(|e| e.to_string())?;
        if !exists {
            append_history(
                &tx,
                run_id,
                Some(&node_id),
                Some(attempt),
                "native_hook_received",
                &detail,
                Some(SOURCE_NATIVE_HOOK),
                Some(DISPOSITION_RECEIVED),
            )
            .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

pub(crate) fn native_hook_receipts(run_id: i64, after: i64) -> SqlResult<Vec<CircuitHistoryEntry>> {
    let db = crate::db::read_conn();
    let mut stmt = db.prepare("SELECT id,node_id,attempt,kind,detail,source,disposition,observed_at FROM circuit_run_history WHERE run_id=?1 AND id>?2 AND kind='native_hook_received' ORDER BY id LIMIT 512")?;
    let rows = stmt.query_map(params![run_id, after], |r| {
        Ok(CircuitHistoryEntry {
            id: r.get(0)?,
            node_id: r.get(1)?,
            attempt: r.get(2)?,
            kind: r.get(3)?,
            detail: r.get(4)?,
            source: r.get(5)?,
            disposition: r.get(6)?,
            observed_at: r.get(7)?,
        })
    })?;
    rows.collect()
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "CheckpointAction.ts")]
pub enum CheckpointAction {
    Completed,
    NotPerformed,
    Retry,
    Recheck,
}

#[derive(Debug, Clone, serde::Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CheckpointRequest.ts")]
pub struct CheckpointRequest {
    #[ts(as = "i32")]
    pub run_id: i64,
    pub node_id: String,
    pub attempt: i32,
    #[ts(as = "i32")]
    pub expected_revision: i64,
    pub action: CheckpointAction,
    pub reason: String,
}

pub fn record_outcome(request: &CheckpointRequest) -> Result<(), String> {
    let mut db = crate::db::write_conn();
    record_outcome_locked(&mut db, request)
}

fn record_outcome_locked(db: &mut Connection, request: &CheckpointRequest) -> Result<(), String> {
    if request.reason.trim().is_empty() {
        return Err("Give a reason and any supporting evidence.".into());
    }
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let revision: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(id),0) FROM circuit_run_history WHERE run_id=?1",
            [request.run_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if revision != request.expected_revision {
        return Err("The run changed. Refresh its evidence before acting.".into());
    }
    let run = super::ledger::get_circuit_run_inner(&tx, request.run_id)
        .map_err(|e| e.to_string())?
        .ok_or("Run no longer exists.")?;
    if run.state != "running" {
        return Err("Only an active run can resolve a checkpoint.".into());
    }
    let step = super::ledger::list_circuit_run_steps_inner(&tx, request.run_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|s| {
            s.node_id == request.node_id && s.attempt == request.attempt && s.status == "unverified"
        })
        .ok_or("This checkpoint changed. Refresh before acting.")?;
    let graph = run_graph(&tx, run.id)?;
    // An attestation cannot authorize tool use or supply a review verdict.
    use crate::autopilot::circuit::model::CircuitNodeKind;
    if matches!(request.action, CheckpointAction::Completed)
        && matches!(graph.node(&request.node_id).map(|n| &n.kind), Some(
            CircuitNodeKind::SpawnAgentNode { .. } | CircuitNodeKind::AwaitAgentTurn { .. }
            | CircuitNodeKind::LlmTurnClassifier { .. }))
        && (step.agent_node_id.is_some() || !matches!(graph.node(&request.node_id).map(|n| &n.kind), Some(CircuitNodeKind::SpawnAgentNode { .. })))
    {
        let mut view = recovery_view(&tx, &run)?;
        if !view.can_attest_completion(&request.node_id) {
            return Err("Resolve outstanding work, input requests and conflicting evidence before recording completion.".into());
        }
        let reason = format!("Operator-recorded completion: {}", request.reason.trim());
        view.context.set(&format!("node.{}.status", step.node_id), "completed");
        view.context.set(&format!("node.{}.wait.attempt", step.node_id), "");
        view.context.set(&format!("node.{}.observation_blocker", step.node_id), "");
        // Leave native evidence untouched. The next worker tick schedules
        // successors, retaining their independent approval and report gates.
        super::ledger::commit_circuit_advance_inner(&tx, run.id, None, Some(&view.context.to_json()?), &[
            super::CircuitStepOp { node_id:step.node_id.clone(), status:"completed".into(), attempt:step.attempt,
                outcome:Some(Some("completed".into())), error:Some(None),
                agent_node_id:None, fresh_attempt:false }
        ]).map_err(|e| e.to_string())?;
        append_history(&tx, run.id, Some(&step.node_id), Some(step.attempt), "operator_attestation",
            &reason, Some(SOURCE_OPERATOR), Some("completed")).map_err(|e| e.to_string())?;
        return tx.commit().map_err(|e| e.to_string());
    }
    if matches!(request.action, CheckpointAction::Recheck) {
        let kind = graph.node(&request.node_id).map(|n| &n.kind);
        let open_pr = matches!(
            kind,
            Some(CircuitNodeKind::GithubAction {
                action: crate::autopilot::circuit::model::GithubActionKind::OpenPr,
                ..
            })
        );
        let has_open_pr_target = if open_pr {
            has_effect_target(&tx, request.run_id, &request.node_id, request.attempt)
                .map_err(|error| error.to_string())?
        } else {
            false
        };
        let observed_recheck = matches!(
            kind,
            Some(
                CircuitNodeKind::LlmTurnClassifier { .. }
                    | CircuitNodeKind::ReviewVerdict { .. }
                    | CircuitNodeKind::AwaitAgentTurn { .. }
                    | CircuitNodeKind::SpawnAgentNode { .. }
            )
        );
        if !(observed_recheck || (open_pr && has_open_pr_target)) {
            return Err("This action has no authoritative automatic evidence recheck. Inspect the external result.".into());
        }
        let mut context =
            crate::autopilot::circuit::context::CircuitContext::from_json(&run.context_json)?;
        context.set(&format!("node.{}.wait.attempt", step.node_id), "");
        context.set(&format!("node.{}.evaluated_attempt", step.node_id), "");
        context.set(
            &format!("node.{}.classifier_failures.{}", step.node_id, step.attempt),
            "0",
        );
        context.set(&format!("node.{}.recheck_only", step.node_id), "1");
        let op = super::CircuitStepOp {
            node_id: step.node_id,
            status: if open_pr {
                "pending_slot".into()
            } else {
                "running".into()
            },
            attempt: step.attempt,
            outcome: None,
            error: Some(None),
            agent_node_id: None,
            fresh_attempt: false,
        };
        let commit = super::ledger::commit_circuit_advance_inner(
            &tx,
            request.run_id,
            None,
            Some(&context.to_json()?),
            &[op],
        )
        .map_err(|e| e.to_string())?;
        if !commit.applied {
            return Err("Circuit run became terminal before evidence recheck commit".into());
        }
        append_history(
            &tx,
            request.run_id,
            Some(&request.node_id),
            Some(request.attempt),
            "evidence_recheck",
            request.reason.trim(),
            Some(SOURCE_OPERATOR),
            Some(DISPOSITION_REQUESTED),
        )
        .map_err(|e| e.to_string())?;
        return tx.commit().map_err(|e| e.to_string());
    }
    let mut context =
        crate::autopilot::circuit::context::CircuitContext::from_json(&run.context_json)?;
    context.set(&format!("node.{}.recheck_only", request.node_id), "0");
    let effect_kind = match graph.node(&request.node_id).map(|n| &n.kind) {
        Some(CircuitNodeKind::GithubAction { .. }) => "github",
        Some(CircuitNodeKind::InjectPty { .. }) => "prompt",
        Some(CircuitNodeKind::SpawnAgentNode { .. }) if step.agent_node_id.is_none() => "spawn",
        _ => return Err("This checkpoint requires authoritative agent evidence; an external-action attestation cannot resolve it.".into()),
    };
    if effect_kind == "spawn" && matches!(request.action, CheckpointAction::Completed) {
        return Err("Agent identity must be reconciled; attestation cannot establish a spawn attachment or completion.".into());
    }
    if matches!(request.action, CheckpointAction::Completed)
        && matches!(
            graph.node(&request.node_id).map(|n| &n.kind),
            Some(CircuitNodeKind::GithubAction {
                action: crate::autopilot::circuit::model::GithubActionKind::OpenPr,
                ..
            })
        )
    {
        return Err(
            "The pull request identity must be reconciled before this step can advance.".into(),
        );
    }
    // This step was already admitted by the graph. Requiring every ancestor
    // verdict to be approved deadlocks the changes-requested feedback route
    // and alternative approval branches. Current human requests still block.
    let recovery = recovery_view(&tx, &run)?;
    if recovery.report_has_known_blockers(&request.node_id) {
        return Err("Resolve outstanding work, input requests and conflicting evidence before recording an outcome.".into());
    }
    let prior: Option<String> = tx.query_row("SELECT state FROM circuit_effects WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind=?4",
        params![request.run_id,request.node_id,request.attempt,effect_kind], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
    let (status, attempt, outcome) =
        match request.action {
            CheckpointAction::Completed => {
                tx.execute(
                    "INSERT INTO circuit_effects VALUES (?1,?2,?3,?4,'attested_completed')
                ON CONFLICT(run_id,node_id,attempt,kind) DO UPDATE SET state='attested_completed'",
                    params![
                        request.run_id,
                        request.node_id,
                        request.attempt,
                        effect_kind
                    ],
                )
                .map_err(|e| e.to_string())?;
                ("completed", step.attempt, Some(Some("completed".into())))
            }
            CheckpointAction::NotPerformed => {
                tx.execute(
                    "INSERT INTO circuit_effects VALUES (?1,?2,?3,?4,'not_performed')
                ON CONFLICT(run_id,node_id,attempt,kind) DO UPDATE SET state='not_performed'",
                    params![
                        request.run_id,
                        request.node_id,
                        request.attempt,
                        effect_kind
                    ],
                )
                .map_err(|e| e.to_string())?;
                ("unverified", step.attempt, None)
            }
            CheckpointAction::Retry if prior.as_deref() == Some("not_performed") => {
                ("pending_slot", step.attempt + 1, Some(None))
            }
            CheckpointAction::Retry => return Err(
                "The earlier action must be recorded as not performed before a deliberate retry."
                    .into(),
            ),
            CheckpointAction::Recheck => unreachable!("handled above"),
        };
    let reason = format!(
        "Operator-recorded outcome ({:?}): {}",
        request.action,
        request.reason.trim()
    );
    // The attestation's disposition is the action the operator recorded.
    let disposition = match request.action {
        CheckpointAction::Completed => "completed",
        CheckpointAction::NotPerformed => "not_performed",
        CheckpointAction::Retry => "retry",
        CheckpointAction::Recheck => "recheck",
    };
    let op = super::CircuitStepOp {
        node_id: request.node_id.clone(),
        status: status.into(),
        attempt,
        outcome,
        error: Some((status != "completed").then(|| reason.clone())),
        agent_node_id: None,
        fresh_attempt: attempt != step.attempt,
    };
    let commit = super::ledger::commit_circuit_advance_inner(
        &tx,
        request.run_id,
        None,
        Some(&context.to_json()?),
        &[op],
    )
        .map_err(|e| e.to_string())?;
    if !commit.applied {
        return Err("Circuit run became terminal before operator outcome commit".into());
    }
    append_history(
        &tx,
        request.run_id,
        Some(&request.node_id),
        Some(request.attempt),
        "operator_attestation",
        &reason,
        Some(SOURCE_OPERATOR),
        Some(disposition),
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    Spawn,
    Prompt,
    Github,
}

impl EffectKind {
    fn as_db_str(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Prompt => "prompt",
            Self::Github => "github",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectIntent {
    pub node_id: String,
    pub attempt: i32,
    pub kind: EffectKind,
}

/// A local status mutation that is committed with the transition which
/// completes its Circuit step. It has no external dispatch window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatusEffect {
    pub node_id: String,
    pub attempt: i32,
    pub agent_node_id: i64,
    pub status: crate::models::SessionStatus,
}

#[derive(Debug, Clone)]
pub struct ReconciledEffect {
    pub intent: EffectIntent,
    pub detail: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub(crate) enum QueueWaitReason {
    CircuitDisabled,
    MeshCapacity { capacity: i64 },
    AgentCapacity { required: i64, available: Option<i64> },
    ReservationUnavailable,
}

pub(crate) fn record_queue_wait(run_id: i64, reason: QueueWaitReason) -> SqlResult<()> {
    record_queue_wait_locked(&mut crate::db::write_conn(), run_id, reason)
}

fn record_queue_wait_locked(db: &mut Connection, run_id: i64, reason: QueueWaitReason) -> SqlResult<()> {
    let tx = db.transaction()?;
    let pending: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM autopilot_circuit_runs WHERE id=?1 AND state='pending')", [run_id], |row|row.get(0))?;
    if !pending { return Ok(()); }
    let detail = serde_json::to_string(&reason).expect("queue wait fields");
    let previous: Option<String> = tx.query_row("SELECT detail FROM circuit_run_history WHERE run_id=?1 AND kind='queue_wait' ORDER BY id DESC LIMIT 1", [run_id], |row|row.get(0)).optional()?;
    if previous.as_deref() != Some(&detail) {
        append_history(&tx,run_id,None,None,"queue_wait",&detail,Some(SOURCE_ADMISSION),Some(DISPOSITION_WAITING))?;
    }
    tx.commit()
}

pub(super) fn pin_graph(db: &Connection, run_id: i64) -> SqlResult<()> {
    use sha2::{Digest, Sha256};
    // `behavior_revision` is a recorded placeholder constant (issue #1909); it
    // is not a live revision counter. Graph identity is pinned by the
    // `graph_sha256` in the history entry, which is what the operator surface
    // relies on. Redefining the revision scheme is a separate decision.
    let inserted = db.execute("INSERT OR IGNORE INTO circuit_run_snapshots (run_id,graph_json,behavior_revision)
        SELECT r.id,c.graph_json,1 FROM autopilot_circuit_runs r JOIN autopilot_circuits c ON c.id=r.circuit_id WHERE r.id=?1", [run_id])?;
    if inserted == 0 { return Ok(()); }
    let (graph, context): (String, String) = db.query_row("SELECT s.graph_json,r.context_json FROM circuit_run_snapshots s JOIN autopilot_circuit_runs r ON r.id=s.run_id WHERE r.id=?1", [run_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let context: std::collections::BTreeMap<String,String> = serde_json::from_str(&context).unwrap_or_default();
    let reviewers: Vec<serde_json::Value> = context.iter().filter_map(|(key, value)| {
        let node_id = key.strip_prefix("review.launch.")?;
        let plan: crate::preferences::spawn_configurations::SpawnConfiguration = serde_json::from_str(value).ok()?;
        // Explicit allowlist: launch arguments, environment, endpoints and prompts
        // can contain private values and must not leak into operator history.
        Some(serde_json::json!({"node_id":node_id,"configuration_id":plan.id,
            "spawn_option_id":plan.spawn_option_id,"harness_id":plan.harness_id,
            "provider_route_id":plan.provider_route_id,"model":plan.model,"effort":plan.effort,
            "extra_arguments_configured":plan.extra_args.as_ref().is_some_and(|args|!args.is_empty())}))
    }).collect();
    append_history(db,run_id,None,None,"configuration_pinned", &serde_json::json!({
        "behavior_revision":1,"graph_sha256":hex::encode(Sha256::digest(graph.as_bytes())),
        "reviewers":reviewers
    }).to_string(), Some(SOURCE_RUN_CONFIGURATION), Some(DISPOSITION_APPLIED))?;
    Ok(())
}

/// Record changes to the persisted evidence window alongside its projection.
pub(super) fn record_wait_changes(db: &Connection, run_id: i64, next_context: &str) -> SqlResult<()> {
    let previous: String = db.query_row("SELECT context_json FROM autopilot_circuit_runs WHERE id=?1", [run_id], |row| row.get(0))?;
    let previous: std::collections::BTreeMap<String,String> = serde_json::from_str(&previous).unwrap_or_default();
    let next: std::collections::BTreeMap<String,String> = serde_json::from_str(next_context).unwrap_or_default();
    let continuation_keys: std::collections::BTreeSet<&String> = previous.keys().chain(next.keys()).filter(|key|key.starts_with("node.") && key.ends_with(".continuation.delivery")).collect();
    for key in continuation_keys {
        let node = key.strip_prefix("node.").and_then(|key|key.strip_suffix(".continuation.delivery")).expect("filtered continuation key");
        let attempt = next.get(&format!("node.{node}.continuation.attempt")).and_then(|value|value.parse::<i32>().ok());
        let count_key = format!("node.{node}.continuations.{}", attempt.unwrap_or_default());
        if previous.get(key) != next.get(key) || previous.get(&count_key) != next.get(&count_key) {
            let state = match next.get(key).map(String::as_str) {
                Some("pending") => "intent", Some("claimed") => "possible_dispatch",
                Some("delivered") => "acknowledged", Some("obsolete") => "not_performed",
                Some("uncertain") => "uncertain", _ => continue,
            };
            if state == "possible_dispatch" && previous.get(key).map(String::as_str) != Some("pending") {
                append_history(db,run_id,Some(node),attempt,"continuation_effect",&serde_json::json!({
                    "effect":"continuation_prompt","state":"intent","ordinal":next.get(&count_key)
                }).to_string(),Some(SOURCE_CIRCUIT_WORKER),Some(DISPOSITION_INTENT))?;
            }
            append_history(db,run_id,Some(node),attempt,"continuation_effect",&serde_json::json!({
                "effect":"continuation_prompt","state":state,"ordinal":next.get(&count_key)
            }).to_string(),Some(SOURCE_CIRCUIT_WORKER),Some(state))?;
        }
    }
    let blocker_keys: std::collections::BTreeSet<&String> = previous.keys().chain(next.keys())
        .filter(|key| key.starts_with("node.") && key.ends_with(".observation_blocker")).collect();
    for key in blocker_keys {
        if previous.get(key) == next.get(key) { continue; }
        let node = key.strip_prefix("node.").and_then(|key| key.strip_suffix(".observation_blocker"));
        let Some(node) = node else { continue; };
        let attempt = db.query_row("SELECT attempt FROM autopilot_circuit_run_steps WHERE run_id=?1 AND node_id=?2",
            params![run_id, node], |row| row.get::<_, i32>(0)).optional()?;
        let blocker = next.get(key).and_then(|value| serde_json::from_str::<crate::autopilot::circuit::observation::CircuitObservationBlocker>(value).ok());
        let detail = serde_json::json!({"blocker": blocker, "message": blocker.as_ref().map(|blocker| blocker.message())});
        append_history(db, run_id, Some(node), attempt, "observation_readiness", &detail.to_string(),
            Some(SOURCE_RECONCILIATION), Some(wait_disposition(blocker.is_some())))?;
    }
    let capacity_keys: std::collections::BTreeSet<&String> = previous.keys().chain(next.keys()).filter(|key|key.starts_with("node.") && key.ends_with(".capacity_wait")).collect();
    for key in capacity_keys {
        if previous.get(key) != next.get(key) {
            let node = key.strip_prefix("node.").and_then(|key|key.strip_suffix(".capacity_wait"));
            // Identity: the parked step's current attempt, so the operator
            // surface can name which execution is waiting (issue #1909).
            let attempt = match node {
                Some(node) => db.query_row(
                    "SELECT attempt FROM autopilot_circuit_run_steps WHERE run_id=?1 AND node_id=?2",
                    params![run_id, node], |r| r.get::<_, i32>(0),
                ).optional()?,
                None => None,
            };
            // A freed window (`circuit_limit`/`agent_limit` both false, or the
            // key gone) is a resolution, not an active wait (issue #1909).
            let window = next.get(key).and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok());
            let binding = window.as_ref().is_some_and(|value|
                value["circuit_limit"].as_bool().unwrap_or(false)
                    || value["agent_limit"].as_bool().unwrap_or(false));
            append_history(db,run_id,node,attempt,"step_capacity_wait",&serde_json::json!({"before":previous.get(key),"after":next.get(key)}).to_string(),Some(SOURCE_CAPACITY),Some(wait_disposition(binding)))?;
        }
    }
    let nodes: std::collections::BTreeSet<&str> = previous.keys().chain(next.keys()).filter_map(|key| key.strip_prefix("node.")?.strip_suffix(".wait.attempt")).collect();
    for node in nodes {
        let prefix = format!("node.{node}.wait.");
        let fields = ["attempt","timeout_ms","since_ms","observed","explicit_budget"];
        let project = |context: &std::collections::BTreeMap<String,String>| -> std::collections::BTreeMap<&str,Option<String>> {
            fields.iter().map(|field|(*field,context.get(&format!("{prefix}{field}")).cloned())).collect()
        };
        let before = project(&previous);
        let after = project(&next);
        if before != after {
            // Identity survives a resolution: a cleared window writes an empty
            // attempt, so fall back to the prior window's attempt rather than
            // dropping the identity the wait was parked on (issue #1909 review).
            let attempt = next.get(&format!("{prefix}attempt")).and_then(|value| value.parse::<i32>().ok())
                .or_else(|| previous.get(&format!("{prefix}attempt")).and_then(|value| value.parse::<i32>().ok()));
            // A window whose (possibly cleared) attempt is empty is a
            // resolution, not an active wait.
            let binding = after.get("attempt").and_then(|value| value.as_deref()).is_some_and(|value| !value.is_empty());
            append_history(db,run_id,Some(node),attempt,"evidence_window_changed",&serde_json::json!({"before":before,"after":after}).to_string(),Some(SOURCE_RECONCILIATION),Some(wait_disposition(binding)))?;
        }
    }
    Ok(())
}

pub(super) fn run_graph(
    db: &Connection,
    run_id: i64,
) -> Result<crate::autopilot::circuit::model::CircuitGraph, String> {
    let json = run_graph_json(db, run_id)?;
    crate::autopilot::circuit::model::CircuitGraph::from_json(&json)
}

pub(super) fn run_graph_json(db: &Connection, run_id: i64) -> Result<String, String> {
    db
        .query_row(
            "SELECT COALESCE(s.graph_json,c.graph_json)
        FROM autopilot_circuit_runs r JOIN autopilot_circuits c ON c.id=r.circuit_id
        LEFT JOIN circuit_run_snapshots s ON s.run_id=r.id WHERE r.id=?1",
            [run_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())
}

/// Append one Circuit Run History event. `source` and `disposition` are the
/// provenance vocabulary above; both are nullable for pre-v45 rows (issue
/// #1909 / #1847). `observed_at` defaults to the append time, which for these
/// synchronous events is the observed time.
pub(super) fn append_history(
    db: &Connection,
    run_id: i64,
    node_id: Option<&str>,
    attempt: Option<i32>,
    kind: &str,
    detail: &str,
    source: Option<&str>,
    disposition: Option<&str>,
) -> SqlResult<()> {
    db.execute("INSERT INTO circuit_run_history (run_id,node_id,attempt,kind,detail,source,disposition) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![run_id, node_id, attempt, kind, crate::secret_scrubber::SecretScrubber::scrub(detail), source, disposition])?;
    Ok(())
}

fn revision_inner(db: &Connection, run_id: i64) -> SqlResult<i64> {
    db.query_row(
        "SELECT COALESCE(MAX(id),0) FROM circuit_run_history WHERE run_id=?1",
        [run_id],
        |r| r.get(0),
    )
}

pub fn observation_revision(run: &crate::models::AutopilotCircuitRun) -> SqlResult<i64> {
    let db = crate::db::read_conn();
    db.query_row(
        "SELECT (SELECT COALESCE(MAX(id),0) FROM circuit_run_history WHERE run_id=r.id)
        FROM autopilot_circuit_runs r WHERE r.id=?1 AND r.context_json=?2 AND r.state=?3",
        params![run.id, run.context_json, run.state],
        |r| r.get(0),
    )
}

/// State, step projection and effect intent land in one transaction.
pub fn commit_transition(
    run_id: i64,
    state: Option<&str>,
    context: &str,
    steps: &[super::CircuitStepOp],
    evidence: EvidenceWrite<'_>,
) -> SqlResult<i64> {
    // Filesystem validation belongs before acquiring the DB writer. Retain the
    // original pull fingerprint so activity since observation invalidates it.
    if evidence.input_guard.and_then(|guard|guard.transcript_guard.as_ref()).is_some_and(|snapshot|!snapshot.is_current()) {
        return Err(observation_freshness_rejection(
            "Native transcript changed before evidence commit; recheck required",
        ));
    }
    if evidence.input_guard.and_then(|guard| guard.report_guard.as_ref()).is_some_and(|snapshot| !snapshot.is_current()) {
        return Err(observation_freshness_rejection("Agent report changed before evidence commit; recheck required"));
    }
    let mut db = crate::db::write_conn();
    let result = if let Some(guard) = evidence.input_guard {
        let mut revision = None;
        let mut commit_error = None;
        let accepted = crate::agent::process::PROCESS_REGISTRY.commit_recovered_turn(
                guard.agent_node_id,
                &guard.input_stamp,
                guard.observed_at_ms,
                || {
                    match commit_transition_locked(&mut db, run_id, state, context, steps, evidence) {
                        Ok(committed_revision) => {
                            revision = Some(committed_revision);
                            Ok(true)
                        }
                        Err(error) => {
                            let message = error.to_string();
                            commit_error = Some(error);
                            Err(message)
                        }
                    }
                },
            );
        if let Some(error) = commit_error {
            Err(error)
        } else if !accepted.map_err(rusqlite::Error::InvalidParameterName)? {
            Err(observation_freshness_rejection("agent input or session changed before evidence commit"))
        } else {
            revision.ok_or(rusqlite::Error::InvalidQuery)
        }
    } else {
        commit_transition_locked(&mut db, run_id, state, context, steps, evidence)
    };
    drop(db);
    if result.is_ok()
        && state.is_some_and(crate::autopilot::circuit::vocabulary::RunState::is_terminal_db_str)
    {
        crate::services::circuit_worker::wake_circuit_worker();
    }
    result
}

#[derive(Default)]
pub struct EvidenceWrite<'a> {
    pub input_guard: Option<&'a crate::autopilot::circuit::stepper::ObservationInputFence>,
    pub intents: &'a [EffectIntent],
    pub agent_status_effects: &'a [AgentStatusEffect],
    pub reconciled_effects: &'a [ReconciledEffect],
    pub observations: &'a [crate::autopilot::circuit::observation::RecordedObservation],
    pub classifications: &'a [crate::autopilot::circuit::observation::RecordedClassification],
    pub expected: Option<&'a crate::autopilot::circuit::stepper::TransitionFence>,
}

pub(crate) fn commit_transition_locked(
    db: &mut Connection,
    run_id: i64,
    state: Option<&str>,
    context: &str,
    steps: &[super::CircuitStepOp],
    evidence: EvidenceWrite<'_>,
) -> SqlResult<i64> {
    let tx = db.transaction()?;
    if let Some(guard) = evidence.input_guard {
        let current: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_nodes WHERE id=?1 AND cli_session_id=?2
            AND CAST(session_started_at AS TEXT)=?3 AND status NOT IN ('archived','error','lost'))",
            params![
                guard.agent_node_id,
                guard.session_id,
                guard.session_incarnation
            ],
            |r| r.get(0),
        )?;
        if !current {
            return Err(observation_freshness_rejection(
                "agent session incarnation changed before evidence commit",
            ));
        }
    }
    if let Some(expected) = evidence.expected {
        if expected
            .revision
            .is_some_and(|r| revision_inner(&tx, run_id).ok() != Some(r))
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let current = super::ledger::get_circuit_run_inner(&tx, run_id)?;
        let durable = super::ledger::list_circuit_run_steps_inner(&tx, run_id)?;
        let matches = current.is_some_and(|r| r.state == expected.state.as_db_str())
            && durable.len() == expected.steps.len()
            && durable.iter().all(|step| {
                expected.steps.iter().any(|old| {
                    old.node_id == step.node_id
                        && old.attempt == step.attempt
                        && old.status.as_db_str() == step.status
                        && old.agent_node_id == step.agent_node_id
                        && old.error == step.error_message
                })
            });
        if !matches {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    let commit = super::ledger::commit_circuit_advance_inner(&tx, run_id, state, Some(context), steps)?;
    if !commit.applied {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let graph = if evidence.agent_status_effects.is_empty() {
        None
    } else {
        Some(run_graph(&tx, run_id).map_err(|error| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                error,
            )))
        })?)
    };
    for effect in evidence.agent_status_effects {
        let completed = steps.iter().any(|step| {
            step.node_id == effect.node_id
                && step.attempt == effect.attempt
                && step.status == "completed"
        });
        let configured_status = match graph.as_ref().and_then(|graph| graph.node(&effect.node_id)).map(|node| &node.kind) {
            Some(crate::autopilot::circuit::model::CircuitNodeKind::SetNodeStatus {
                status,
                ..
            }) => match status {
                crate::autopilot::circuit::model::SessionStatusKind::Running => {
                    crate::models::SessionStatus::Running
                }
                crate::autopilot::circuit::model::SessionStatusKind::Idle => {
                    crate::models::SessionStatus::Idle
                }
                crate::autopilot::circuit::model::SessionStatusKind::Completed => {
                    crate::models::SessionStatus::Completed
                }
            },
            _ => return Err(rusqlite::Error::InvalidQuery),
        };
        if !completed || configured_status != effect.status {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let current_status: String = tx
            .query_row(
                "SELECT status FROM agent_nodes WHERE id=?1",
                [effect.agent_node_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        if current_status == crate::models::SessionStatus::Archived.to_db_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        crate::db::update_agent_node_status_inner(&tx, effect.agent_node_id, effect.status)?;
        append_history(
            &tx,
            run_id,
            Some(&effect.node_id),
            Some(effect.attempt),
            "effect_result",
            &serde_json::json!({
                "effect": "set_node_status",
                "state": "acknowledged",
                "agent_node_id": effect.agent_node_id,
                "status": effect.status.to_db_str(),
                "meaning": "Agent status and Circuit step committed atomically"
            })
            .to_string(),
            Some(SOURCE_CIRCUIT_WORKER),
            Some(DISPOSITION_ACKNOWLEDGED),
        )?;
    }
    for observation in evidence.observations {
        let identity = &observation.observation.identity;
        let detail = serde_json::to_string(observation)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        append_history(
            &tx,
            run_id,
            Some(&identity.step_id),
            Some(identity.attempt),
            "observation",
            &detail,
            Some(observation.observation.source.as_str()),
            Some(observation_disposition_str(observation.disposition)),
        )?;
    }
    for classification in evidence.classifications {
        let detail = serde_json::to_string(classification).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        append_history(&tx, run_id, Some(&classification.step_id), Some(classification.attempt), "classification", &detail, Some(SOURCE_CLASSIFIER), Some(DISPOSITION_INTERPRETED))?;
    }
    for effect in evidence.reconciled_effects {
        let completed = steps.iter().any(|step| {
            step.node_id == effect.intent.node_id
                && step.attempt == effect.intent.attempt
                && step.status == "completed"
        });
        let graph = run_graph(&tx, run_id).map_err(|error| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                error,
            )))
        })?;
        let is_open_pr = matches!(
            graph.node(&effect.intent.node_id).map(|node| &node.kind),
            Some(crate::autopilot::circuit::model::CircuitNodeKind::GithubAction {
                action: crate::autopilot::circuit::model::GithubActionKind::OpenPr,
                ..
            })
        );
        if !completed || effect.intent.kind != EffectKind::Github || !is_open_pr {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let changed = tx.execute(
            "UPDATE circuit_effects SET state='acknowledged'
             WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='github'
               AND state IN ('uncertain','not_performed')",
            params![run_id, effect.intent.node_id, effect.intent.attempt],
        )?;
        if changed == 1 {
            append_history(
                &tx,
                run_id,
                Some(&effect.intent.node_id),
                Some(effect.intent.attempt),
                "effect_reconciled",
                &effect.detail,
                Some(SOURCE_GITHUB),
                Some(DISPOSITION_RECONCILED),
            )?;
        } else {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    for intent in evidence.intents {
        let inserted = tx.execute("INSERT OR IGNORE INTO circuit_effects (run_id,node_id,attempt,kind,state)
            SELECT ?1,?2,?3,?4,'intent' WHERE EXISTS
            (SELECT 1 FROM autopilot_circuit_runs r JOIN autopilot_circuit_run_steps s ON s.run_id=r.id
             WHERE r.id=?1 AND r.state='running' AND s.node_id=?2 AND s.attempt=?3)",
            params![run_id,intent.node_id,intent.attempt,intent.kind.as_db_str()])?;
        if inserted == 1 {
            append_history(
                &tx,
                run_id,
                Some(&intent.node_id),
                Some(intent.attempt),
                "effect_intent",
                intent.kind.as_db_str(),
                Some(SOURCE_CIRCUIT_WORKER),
                Some(DISPOSITION_INTENT),
            )?;
        }
    }
    let revision = revision_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(revision)
}

/// Must commit before any bytes are sent. A repeated claim never dispatches.
pub(crate) fn acknowledge_spawn_attachment(
    run_id: i64, node_id: &str, attempt: i32, agent_node_id: i64, parent_agent_node_id: Option<i64>, expected_agent_node_id: Option<i64>,
) -> SqlResult<Option<i64>> {
    let mut db = crate::db::write_conn();
    acknowledge_spawn_attachment_locked(&mut db, run_id, node_id, attempt, agent_node_id, parent_agent_node_id, expected_agent_node_id)
}

fn acknowledge_spawn_attachment_locked(
    db: &mut Connection, run_id: i64, node_id: &str, attempt: i32, agent_node_id: i64, parent_agent_node_id: Option<i64>, expected_agent_node_id: Option<i64>,
) -> SqlResult<Option<i64>> {
    let tx = db.transaction()?;
    let updated = tx.execute("UPDATE autopilot_circuit_run_steps SET agent_node_id=?4,parent_agent_node_id=?5
        WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND status='running'
        AND EXISTS(SELECT 1 FROM circuit_effects e WHERE e.run_id=?1 AND e.node_id=?2 AND e.attempt=?3 AND e.kind='spawn'
            AND ((e.state='possible_dispatch' AND agent_node_id IS ?6 AND (agent_node_id IS NULL OR agent_node_id=?4 OR ?3>1))
                OR (e.state='acknowledged' AND agent_node_id=?4)))
        AND EXISTS(SELECT 1 FROM autopilot_circuit_runs WHERE id=?1 AND state='running')",
        params![run_id,node_id,attempt,agent_node_id,parent_agent_node_id,expected_agent_node_id])?;
    if updated == 0 { return Ok(None); }
    let acknowledged = tx.execute("INSERT INTO circuit_effects(run_id,node_id,attempt,kind,state) VALUES (?1,?2,?3,'spawn','acknowledged')
        ON CONFLICT(run_id,node_id,attempt,kind) DO UPDATE SET state='acknowledged' WHERE state='possible_dispatch'",
        params![run_id,node_id,attempt])?;
    if acknowledged > 0 {
        append_history(&tx, run_id, Some(node_id), Some(attempt), "effect_result",
            &serde_json::json!({"effect":"spawn","state":"acknowledged","agent_node_id":agent_node_id,
                "meaning":if expected_agent_node_id == Some(agent_node_id) { "Prompt delivered to retained agent; completion still requires evidence" }
                    else { "Agent allocation attached; foreground and owned work completion still require evidence" }}).to_string(),
            Some(SOURCE_CIRCUIT_WORKER), Some(DISPOSITION_ACKNOWLEDGED))?;
    }
    let revision = revision_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(Some(revision))
}

pub fn claim_effect(run_id: i64, intent: &EffectIntent) -> SqlResult<Option<i64>> {
    let mut db = crate::db::write_conn();
    claim_effect_locked(&mut db, run_id, intent)
}

pub(crate) fn acknowledge_prompt_delivery(run_id: i64, node_id: &str, attempt: i32) -> SqlResult<Option<i64>> {
    let mut db = crate::db::write_conn();
    acknowledge_prompt_delivery_locked(&mut db, run_id, node_id, attempt)
}

fn acknowledge_prompt_delivery_locked(db: &mut Connection, run_id: i64, node_id: &str, attempt: i32) -> SqlResult<Option<i64>> {
    let tx = db.transaction()?;
    let updated = tx.execute("UPDATE circuit_effects SET state='acknowledged'
        WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='prompt' AND state='possible_dispatch'
        AND EXISTS(SELECT 1 FROM autopilot_circuit_runs r JOIN autopilot_circuit_run_steps s ON s.run_id=r.id
            WHERE r.id=?1 AND r.state IN ('running','paused') AND s.node_id=?2 AND s.attempt=?3 AND s.status='running')",
        params![run_id, node_id, attempt])?;
    if updated == 0 { return Ok(None); }
    append_history(&tx, run_id, Some(node_id), Some(attempt), "effect_result", "acknowledged",
        Some(SOURCE_CIRCUIT_WORKER), Some(DISPOSITION_ACKNOWLEDGED))?;
    let revision = revision_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(Some(revision))
}

pub(crate) fn prompt_delivery_acknowledged(run_id: i64, node_id: &str, attempt: i32) -> SqlResult<bool> {
    let db = crate::db::read_conn();
    db.query_row("SELECT EXISTS(SELECT 1 FROM circuit_effects
        WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='prompt' AND state='acknowledged')",
        params![run_id, node_id, attempt], |row| row.get(0))
}

pub(crate) fn record_effect_target(
    run_id: i64,
    node_id: &str,
    attempt: i32,
    detail: &str,
) -> Result<i64, String> {
    let mut db = crate::db::write_conn();
    record_effect_target_locked(&mut db, run_id, node_id, attempt, detail)
}

fn record_effect_target_locked(
    db: &mut Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    detail: &str,
) -> Result<i64, String> {
    let tx = db.transaction().map_err(|error| error.to_string())?;
    let graph = run_graph(&tx, run_id)?;
    if !matches!(
        graph.node(node_id).map(|node| &node.kind),
        Some(crate::autopilot::circuit::model::CircuitNodeKind::GithubAction {
            action: crate::autopilot::circuit::model::GithubActionKind::OpenPr,
            ..
        })
    ) {
        return Err("Effect target is only supported for an OpenPr step.".into());
    }
    let dispatch_claimed: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM circuit_effects e
             JOIN autopilot_circuit_runs r ON r.id=e.run_id
             JOIN autopilot_circuit_run_steps s ON s.run_id=e.run_id AND s.node_id=e.node_id
             WHERE e.run_id=?1 AND e.node_id=?2 AND e.attempt=?3 AND e.kind='github'
               AND e.state='possible_dispatch' AND r.state='running'
               AND s.attempt=?3 AND s.status='running')",
            params![run_id, node_id, attempt],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if !dispatch_claimed {
        return Err("OpenPr dispatch claim changed before its target was recorded.".into());
    }
    append_history(
        &tx,
        run_id,
        Some(node_id),
        Some(attempt),
        "effect_target",
        detail,
        Some(SOURCE_GITHUB),
        Some(DISPOSITION_RECORDED),
    )
    .map_err(|error| error.to_string())?;
    let revision = revision_inner(&tx, run_id).map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok(revision)
}

pub(crate) fn latest_effect_target(
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> Result<Option<String>, String> {
    let db = crate::db::read_conn();
    latest_effect_target_inner(&db, run_id, node_id, attempt).map_err(|error| error.to_string())
}

fn latest_effect_target_inner(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> SqlResult<Option<String>> {
    db.query_row(
        "SELECT detail FROM circuit_run_history
         WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='effect_target'
         ORDER BY id DESC LIMIT 1",
        params![run_id, node_id, attempt],
        |row| row.get(0),
    )
    .optional()
}

fn has_effect_target(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> SqlResult<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM circuit_run_history WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='effect_target')",
        params![run_id, node_id, attempt],
        |row| row.get(0),
    )
}

fn claim_effect_locked(
    db: &mut Connection,
    run_id: i64,
    intent: &EffectIntent,
) -> SqlResult<Option<i64>> {
    let tx = db.transaction()?;
    let claimed = tx.execute("UPDATE circuit_effects SET state='possible_dispatch'
        WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind=?4 AND state='intent'
        AND EXISTS (SELECT 1 FROM autopilot_circuit_runs r JOIN autopilot_circuit_run_steps s ON s.run_id=r.id
          WHERE r.id=?1 AND r.state='running' AND s.node_id=?2 AND s.attempt=?3 AND s.status='running')",
        params![run_id,intent.node_id,intent.attempt,intent.kind.as_db_str()])? == 1;
    if claimed {
        append_history(
            &tx,
            run_id,
            Some(&intent.node_id),
            Some(intent.attempt),
            "effect_possible_dispatch",
            intent.kind.as_db_str(),
            Some(SOURCE_CIRCUIT_WORKER),
            Some(DISPOSITION_POSSIBLE_DISPATCH),
        )?;
    }
    let revision = revision_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(claimed.then_some(revision))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_projection_conflicts_restore_only_from_matching_history_and_attempt() {
        use crate::autopilot::circuit::observation::*;
        let db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');").unwrap();
        let identity = ObservationIdentity { run_id: 1, step_id: "gate".into(), attempt: 1,
            agent_node_id: 9, session_incarnation: Some("old".into()), session_id: None,
            turn_id: None, report_revision: None };
        let current = ObservationIdentity { session_incarnation: Some("new".into()), ..identity.clone() };
        let observation = CircuitObservation { identity: current.clone(), source: "agent_status_projection".into(),
            source_id: Some("current".into()), observed_at_ms: 100, authoritative: false, fact: ObservedWorkFact::Working };
        let mut evidence = WorkEvidence { identity: Some(identity), conflicted: true, ..Default::default() };
        evidence.observe(&current, &observation);
        evidence.conflicts.retain(|conflict| conflict.kind == EvidenceConflictKind::Identity);
        evidence.conflicts[0].status_projection = None;
        let detail = serde_json::json!({"observation":observation,"disposition":"conflicting"}).to_string();
        append_history(&db, 1, Some("gate"), Some(2), "observation", &detail,
            Some("agent_status_projection"), Some("conflicting")).unwrap();
        assert!(!restore_projection_conflicts_inner(&db, 1, "gate", 1, &mut evidence).unwrap());
        append_history(&db, 1, Some("gate"), Some(1), "observation", &detail,
            Some("agent_status_projection"), Some("conflicting")).unwrap();
        assert!(restore_projection_conflicts_inner(&db, 1, "gate", 1, &mut evidence).unwrap());
        assert_eq!(evidence.conflicts[0].status_projection, Some(true));
        assert!(!restore_projection_conflicts_inner(&db, 1, "gate", 1, &mut evidence).unwrap(), "idempotent");
    }

    /// The persisted native receipt carrying this exact `source_id`.
    ///
    /// Addressing receipts by identity rather than by row position matters
    /// here: the ledger interleaves recorded submissions with receipts, and a
    /// positional index silently re-reads an earlier row when one is inserted.
    /// A prefix or "either event" match is worse still — it returns the turn
    /// start when the assertion meant the `Stop`. Requiring exactly one match
    /// turns both mistakes into a loud failure.
    fn receipt_for(
        db: &Connection,
        run_id: i64,
        source_id: &str,
    ) -> crate::services::circuit_worker::native_hooks::NativeReceipt {
        let found: Vec<_> = history_inner(db, run_id)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.kind == "native_hook_received")
            .filter(|entry| entry.detail.contains(&format!("\"source_id\":\"{source_id}\"")))
            .collect();
        assert_eq!(
            found.len(),
            1,
            "expected exactly one receipt with source_id {source_id}, found {}",
            found.len()
        );
        serde_json::from_str(&found[0].detail).unwrap()
    }

    /// How many native receipts the run has recorded.
    fn receipt_count(db: &Connection, run_id: i64) -> usize {
        history_inner(db, run_id)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.kind == "native_hook_received")
            .count()
    }

    #[test]
    fn readiness_history_retains_reason_without_duplicate_poll_entries() {
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'source',1,'unverified');").unwrap();
        let waiting = serde_json::json!({"node.source.observation_blocker":"{\"kind\":\"input_uncertain\"}"}).to_string();
        for _ in 0..3 {
            super::super::ledger::commit_circuit_advance_locked(&mut db,1,None,Some(&waiting),&[]).unwrap();
        }
        let history = history_inner(&db,1).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].kind, "observation_readiness");
        assert_eq!(history[0].disposition.as_deref(), Some("waiting"));
        assert!(history[0].detail.contains("Terminal input tracking is uncertain"));
        let resolved = serde_json::json!({"node.source.observation_blocker":""}).to_string();
        super::super::ledger::commit_circuit_advance_locked(&mut db,1,None,Some(&resolved),&[]).unwrap();
        let history = history_inner(&db,1).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].disposition.as_deref(), Some("resolved"));
    }

    #[test]
    fn input_freshness_rejections_are_distinct_from_transition_conflicts() {
        assert!(is_observation_freshness_rejection(&observation_freshness_rejection(
            "a newer input was submitted",
        )));
        assert!(!is_observation_freshness_rejection(
            &rusqlite::Error::InvalidQuery,
        ));
        assert!(!is_observation_freshness_rejection(
            &rusqlite::Error::InvalidParameterName("unrelated failure".into()),
        ));
    }

    #[test]
    fn circuit_continuation_history_retains_dispatch_uncertainty_without_replay() {
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');").unwrap();
        let mut context = serde_json::json!({"node.verdict.continuation.attempt":"1","node.verdict.continuations.1":"1","node.verdict.continuation.delivery":"claimed"});
        for _ in 0..2 {
            super::super::ledger::commit_circuit_advance_locked(&mut db,1,None,Some(&context.to_string()),&[]).unwrap();
        }
        context["node.verdict.continuation.delivery"] = "uncertain".into();
        super::super::ledger::commit_circuit_advance_locked(&mut db,1,None,Some(&context.to_string()),&[]).unwrap();
        let history = history_inner(&db,1).unwrap();
        let states: Vec<String> = history.iter().map(|entry|serde_json::from_str::<serde_json::Value>(&entry.detail).unwrap()["state"].as_str().unwrap().to_owned()).collect();
        assert_eq!(states,vec!["intent","possible_dispatch","uncertain"]);
        assert!(history.iter().all(|entry|entry.node_id.as_deref()==Some("verdict") && entry.attempt==Some(1)));
    }

    #[test]
    fn circuit_queue_wait_history_records_reason_changes_without_poll_duplicates() {
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'pending');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'spawn',1,'pending_slot');").unwrap();
        record_queue_wait_locked(&mut db,1,QueueWaitReason::CircuitDisabled).unwrap();
        record_queue_wait_locked(&mut db,1,QueueWaitReason::CircuitDisabled).unwrap();
        record_queue_wait_locked(&mut db,1,QueueWaitReason::MeshCapacity { capacity: 3 }).unwrap();
        assert_eq!(history_inner(&db,1).unwrap().len(),2);
        assert!(history_inner(&db,1).unwrap()[1].detail.contains("mesh_capacity"));
        db.execute("UPDATE autopilot_circuit_runs SET state='cancelled' WHERE id=1",[]).unwrap();
        record_queue_wait_locked(&mut db,1,QueueWaitReason::ReservationUnavailable).unwrap();
        assert_eq!(history_inner(&db,1).unwrap().len(),2,"late capacity reports cannot reopen cancelled queue waits");
        db.execute("UPDATE autopilot_circuit_runs SET state='running' WHERE id=1",[]).unwrap();
        let context = serde_json::json!({"node.spawn.capacity_wait":"{\"circuit_limit\":true,\"agent_limit\":false}"}).to_string();
        super::super::ledger::commit_circuit_advance_locked(&mut db,1,None,Some(&context),&[]).unwrap();
        super::super::ledger::commit_circuit_advance_locked(&mut db,1,None,Some(&context),&[]).unwrap();
        let history = history_inner(&db,1).unwrap();
        assert_eq!(history.len(),3);
        assert_eq!(history[2].kind,"step_capacity_wait");
        assert_eq!(history[2].node_id.as_deref(),Some("spawn"));
        // Provenance and identity (issue #1909): the run-level admission wait
        // names its source and disposition but has no step identity, while the
        // step capacity wait names the parked step's attempt.
        for entry in &history[..2] {
            assert_eq!(entry.kind, "queue_wait");
            assert_eq!(entry.source.as_deref(), Some(SOURCE_ADMISSION));
            assert_eq!(entry.disposition.as_deref(), Some(DISPOSITION_WAITING));
            assert_eq!((entry.node_id.as_deref(), entry.attempt), (None, None));
        }
        assert_eq!(history[2].source.as_deref(), Some(SOURCE_CAPACITY));
        assert_eq!(history[2].disposition.as_deref(), Some(DISPOSITION_WAITING));
        assert_eq!(history[2].attempt, Some(1));
    }

    /// Issue #1909: a freed capacity or evidence window is a resolution, not an
    /// active wait — the history must say so, or the operator surface renders a
    /// cleared wait as still parked.
    #[test]
    fn circuit_wait_history_records_resolution_when_capacity_and_evidence_clear() {
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'spawn',1,'pending_slot');").unwrap();
        let parked = serde_json::json!({"node.spawn.capacity_wait":"{\"circuit_limit\":true,\"agent_limit\":true}"}).to_string();
        super::super::ledger::commit_circuit_advance_locked(&mut db,1,None,Some(&parked),&[]).unwrap();
        let freed = serde_json::json!({"node.spawn.capacity_wait":"{\"circuit_limit\":false,\"agent_limit\":false}"}).to_string();
        super::super::ledger::commit_circuit_advance_locked(&mut db,1,None,Some(&freed),&[]).unwrap();
        let history = history_inner(&db,1).unwrap();
        assert_eq!(history.len(),2);
        assert_eq!(history[0].kind,"step_capacity_wait");
        assert_eq!(history[0].disposition.as_deref(),Some(DISPOSITION_WAITING));
        assert_eq!(history[1].kind,"step_capacity_wait");
        assert_eq!(history[1].disposition.as_deref(),Some(DISPOSITION_RESOLVED),"a freed capacity window is a resolution, not an active wait");

        // The same rule applies to an evidence wait window: opening binds,
        // clearing resolves. The real clear writes an empty attempt string
        // (`CircuitContext::set(key, "")`), so identity must survive it.
        let waiting = serde_json::json!({
            "node.spawn.capacity_wait":"{\"circuit_limit\":false,\"agent_limit\":false}",
            "node.spawn.wait.attempt":"1","node.spawn.wait.timeout_ms":"60000","node.spawn.wait.since_ms":"1000"
        }).to_string();
        super::super::ledger::commit_circuit_advance_locked(&mut db,1,None,Some(&waiting),&[]).unwrap();
        let resolved = serde_json::json!({
            "node.spawn.capacity_wait":"{\"circuit_limit\":false,\"agent_limit\":false}",
            "node.spawn.wait.attempt":""
        }).to_string();
        super::super::ledger::commit_circuit_advance_locked(&mut db,1,None,Some(&resolved),&[]).unwrap();
        let windows: Vec<CircuitHistoryEntry> = history_inner(&db,1).unwrap()
            .into_iter().filter(|entry| entry.kind == "evidence_window_changed").collect();
        assert_eq!(windows.len(),2);
        assert_eq!(windows[0].disposition.as_deref(),Some(DISPOSITION_WAITING));
        assert_eq!(windows[0].attempt,Some(1));
        assert_eq!(windows[1].disposition.as_deref(),Some(DISPOSITION_RESOLVED));
        assert_eq!(windows[1].attempt,Some(1),"a resolved evidence wait keeps the attempt it was parked on");
    }

    #[test]
    fn circuit_configuration_and_wait_history_is_atomic_deduplicated_and_retained() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut db = Connection::open(file.path()).unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name,graph_json) VALUES(1,1,'test','{}');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');").unwrap();
        let configuration = crate::preferences::spawn_configurations::SpawnConfiguration {
            id: "launch/reviewer".into(), spawn_option_id: "codex".into(), model: Some("gpt-6-luna".into()),
            effort: Some("low".into()), extra_args: Some("--token private-secret".into()), ..Default::default()
        };
        let context = serde_json::json!({"review.launch.reviewer":serde_json::to_string(&configuration).unwrap()}).to_string();
        db.execute("UPDATE autopilot_circuit_runs SET context_json=?1",[&context]).unwrap();
        { let tx = db.transaction().unwrap(); pin_graph(&tx,1).unwrap(); pin_graph(&tx,1).unwrap(); tx.commit().unwrap(); }
        let history = history_inner(&db,1).unwrap();
        assert_eq!(history.len(),1);
        assert_eq!(history[0].kind,"configuration_pinned");
        assert!(history[0].detail.contains("gpt-6-luna"));
        assert!(!history[0].detail.contains("private-secret"));
        // Configuration revision provenance (issue #1909).
        assert_eq!(history[0].source.as_deref(),Some(SOURCE_RUN_CONFIGURATION));
        assert_eq!(history[0].disposition.as_deref(),Some(DISPOSITION_APPLIED));
        let mut next: serde_json::Value = serde_json::from_str(&context).unwrap();
        next["node.spawn.wait.attempt"] = "1".into();
        next["node.spawn.wait.since_ms"] = "1000".into();
        next["node.spawn.wait.timeout_ms"] = "60000".into();
        let next = next.to_string();
        super::super::ledger::commit_circuit_advance_locked(&mut db,1,None,Some(&next),&[]).unwrap();
        super::super::ledger::commit_circuit_advance_locked(&mut db,1,None,Some(&next),&[]).unwrap();
        assert_eq!(history_inner(&db,1).unwrap().len(),2);
        db.execute_batch("CREATE TRIGGER fail_wait_history BEFORE INSERT ON circuit_run_history BEGIN SELECT RAISE(ABORT,'injected history failure'); END;").unwrap();
        let changed = next.replace("1000","2000");
        assert!(super::super::ledger::commit_circuit_advance_locked(&mut db,1,None,Some(&changed),&[]).is_err());
        assert_eq!(db.query_row("SELECT context_json FROM autopilot_circuit_runs WHERE id=1",[],|row|row.get::<_,String>(0)).unwrap(),next);
        drop(db);
        let db = Connection::open(file.path()).unwrap();
        let history = history_inner(&db,1).unwrap();
        assert_eq!(history.len(),2);
        assert_eq!(history[1].kind,"evidence_window_changed");
        assert_eq!(history[1].attempt,Some(1));
        assert!(history[1].detail.contains("60000"));
        assert_eq!(history[1].source.as_deref(),Some(SOURCE_RECONCILIATION));
        assert_eq!(history[1].disposition.as_deref(),Some(DISPOSITION_WAITING));
    }

    /// Issue #1909 acceptance: the wait / capacity / configuration history
    /// survives an app restart and agrees with the materialized projection.
    #[test]
    fn circuit_wait_capacity_and_configuration_history_survives_reopen_and_agrees_with_projection() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut db = Connection::open(file.path()).unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name,graph_json) VALUES(1,1,'test','{}');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'pending');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'spawn',2,'pending_slot');").unwrap();
        // A pending run parked on mesh admission, then admitted.
        record_queue_wait_locked(&mut db, 1, QueueWaitReason::MeshCapacity { capacity: 2 }).unwrap();
        // The pinned configuration is written on run creation; pin it here and
        // assert the entry agrees with `circuit_run_snapshots` after reopen.
        { let tx = db.transaction().unwrap(); pin_graph(&tx, 1).unwrap(); tx.commit().unwrap(); }
        let context = serde_json::json!({
            "node.spawn.capacity_wait": "{\"circuit_limit\":true,\"agent_limit\":false}",
            "node.spawn.wait.attempt": "2",
            "node.spawn.wait.timeout_ms": "60000",
            "node.spawn.wait.since_ms": "1000"
        }).to_string();
        // Queue exit: pending -> running commits the `run_transition` entry.
        super::super::ledger::commit_circuit_advance_locked(&mut db, 1, Some("running"), Some(&context), &[]).unwrap();
        drop(db);

        // Restart: reopen the same file and read the history back.
        let db = Connection::open(file.path()).unwrap();
        let history = history_inner(&db, 1).unwrap();
        let entry = |kind: &str| history.iter().find(|entry| entry.kind == kind)
            .unwrap_or_else(|| panic!("{kind} history retained across reopen"));
        // Identity, time, source and disposition on every retained event.
        assert!(history.iter().all(|entry| entry.source.is_some() && entry.disposition.is_some() && !entry.observed_at.is_empty()));
        assert_eq!(
            (entry("queue_wait").source.as_deref(), entry("queue_wait").disposition.as_deref(), entry("queue_wait").attempt),
            (Some(SOURCE_ADMISSION), Some(DISPOSITION_WAITING), None),
        );
        assert_eq!(
            (entry("step_capacity_wait").node_id.as_deref(), entry("step_capacity_wait").attempt),
            (Some("spawn"), Some(2)),
        );
        assert_eq!(
            (entry("evidence_window_changed").node_id.as_deref(), entry("evidence_window_changed").attempt),
            (Some("spawn"), Some(2)),
        );
        let configuration = entry("configuration_pinned");
        assert_eq!((configuration.source.as_deref(), configuration.disposition.as_deref()), (Some(SOURCE_RUN_CONFIGURATION), Some(DISPOSITION_APPLIED)));
        let run_transition = entry("run_transition");
        assert_eq!(run_transition.detail, "running");
        assert_eq!((run_transition.source.as_deref(), run_transition.disposition.as_deref()), (Some(SOURCE_CIRCUIT_WORKER), Some(DISPOSITION_APPLIED)));

        // Agreement with the materialized projection: the parked step, the
        // admitted run state and the pinned snapshot all match their history.
        let (state, step_attempt, step_status): (String, i32, String) = db.query_row(
            "SELECT r.state, s.attempt, s.status FROM autopilot_circuit_runs r
             JOIN autopilot_circuit_run_steps s ON s.run_id=r.id WHERE r.id=1",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!((state.as_str(), step_attempt, step_status.as_str()), ("running", 2, "pending_slot"));
        let snapshot_revision: i64 = db.query_row("SELECT behavior_revision FROM circuit_run_snapshots WHERE run_id=1", [], |row| row.get(0)).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&configuration.detail).unwrap()["behavior_revision"].as_i64(),
            Some(snapshot_revision),
        );
    }

    #[test]
    fn native_receipt_is_durable_deduplicated_and_scoped_to_active_attempts() {
        use crate::services::circuit_worker::native_hooks::{NativeHook, NativeReceipt};
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut db = Connection::open(file.path()).unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO agent_nodes (id,mesh_id,name,path) VALUES (9,1,'owned','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,attempt,status,agent_node_id) VALUES (1,'spawn',2,'running',9);").unwrap();
        let mut graph = crate::autopilot::circuit::model::CircuitGraph::walking_skeleton("");
        graph
            .nodes
            .iter_mut()
            .find(|n| n.id == "inject")
            .unwrap()
            .kind = crate::autopilot::circuit::model::CircuitNodeKind::LlmTurnClassifier {
            target_node_id: None,
        };
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [graph.to_json().unwrap()],
        )
        .unwrap();
        let mut receipt = NativeReceipt { agent_node_id: 9, input_stamp: Some("1:2".into()), session_incarnation: Some("1000".into()), source_id: "event-1".into(), received_at_ms: 1, turn_fenced: true, explicit_turn_mismatch: false, submission_correlated: false, submission_seq: None,
            hook: NativeHook::parse("claude", br#"{"session_id":"session","prompt_id":"prompt","hook_event_name":"Stop","background_tasks":[],"session_crons":[],"last_assistant_message":"Complete final report"}"#).unwrap() };
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        drop(db);
        let mut db = Connection::open(file.path()).unwrap();
        receipt.received_at_ms = 2;
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        let history = history_inner(&db, 1).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].node_id.as_deref(), Some("spawn"));
        assert_eq!(history[0].attempt, Some(2));
        let persisted: NativeReceipt = serde_json::from_str(&history[0].detail).unwrap();
        assert_eq!(persisted.received_at_ms, 1);
        assert_eq!(
            persisted.hook.final_report.as_deref(),
            Some("Complete final report")
        );
        db.execute_batch("UPDATE autopilot_circuit_run_steps SET status='completed';
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,attempt,status) VALUES (1,'inject',1,'running');").unwrap();
        receipt.source_id = "event-2".into();
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        let history = history_inner(&db, 1).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(
            history[1].node_id.as_deref(),
            Some("inject"),
            "completed spawn retains the target identity for its running classifier"
        );
        db.execute("UPDATE autopilot_circuit_runs SET state='cancelled'", [])
            .unwrap();
        receipt.source_id = "event-3".into();
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        assert_eq!(history_inner(&db, 1).unwrap().len(), 2);
        for state in ["running", "paused", "completed", "cancelled"] {
            db.execute("UPDATE autopilot_circuit_runs SET state=?1", [state]).unwrap();
            let view = evidence_view_inner(&db, 1).unwrap();
            assert_eq!(view.coverage.len(), 2, "capabilities retained while {state}");
            assert!(view.coverage.iter().all(|item| item.deadline_ms.is_none()));
        }
    }

    #[test]
    fn native_delayed_stop_keeps_its_original_turn_input_after_new_submission() {
        use crate::services::circuit_worker::native_hooks::{NativeHook, NativeReceipt};
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut db = Connection::open(file.path()).unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO agent_nodes (id,mesh_id,name,path) VALUES (9,1,'owned','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,attempt,status,agent_node_id) VALUES (1,'spawn',1,'running',9);").unwrap();
        db.execute("UPDATE autopilot_circuits SET graph_json=?1", [crate::autopilot::circuit::model::CircuitGraph::walking_skeleton("work").to_json().unwrap()]).unwrap();
        // Two real submissions: input A then input B (issue #1898). B is
        // submitted before A's turn start hook lands, so A's start describes
        // a superseded turn.
        record_prompt_submission_locked(&db, 1, "spawn", 1, 9, "first prompt").unwrap();
        record_prompt_submission_locked(&db, 1, "spawn", 1, 9, "second prompt").unwrap();
        let mut receipt = NativeReceipt { agent_node_id: 9, input_stamp: Some("input-a".into()), session_incarnation: Some("1000".into()),
            source_id: "start-a".into(), received_at_ms: 1000, turn_fenced: true, explicit_turn_mismatch: false, submission_correlated: true, submission_seq: None,
            hook: NativeHook::parse("claude", br#"{"session_id":"session","prompt_id":"a","hook_event_name":"UserPromptSubmit","prompt":"first prompt"}"#).unwrap() };
        // The delayed first start is refused: B is the newest submission, so
        // A's turn cannot acknowledge it, and a receipt can never vouch for
        // itself by asserting `submission_correlated`.
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        let refused = receipt_for(&db, 1, "start-a");
        assert!(!refused.submission_correlated);
        assert_eq!(refused.input_stamp, None);
        assert_eq!(refused.submission_seq, None);
        drop(db);
        let mut db = Connection::open(file.path()).unwrap();
        // Input B's turn has not been observed, so A's stop cannot inherit a
        // binding either. This asserts on the Stop receipt itself, not on the
        // start receipt recorded above it.
        receipt.input_stamp = Some("input-b".into());
        receipt.source_id = "stop-a".into();
        receipt.received_at_ms = 2000;
        receipt.hook = NativeHook::parse("claude", br#"{"session_id":"session","prompt_id":"a","hook_event_name":"Stop","last_assistant_message":"Old approval"}"#).unwrap();
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        let unbound_stop = receipt_for(&db, 1, "stop-a");
        assert_eq!(unbound_stop.input_stamp, None, "a Stop with no bound turn start keeps no input stamp");
        assert!(!unbound_stop.submission_correlated);
        assert_eq!(unbound_stop.submission_seq, None);
        // B is observed in order, so it does bind, and its Stop inherits both
        // the stamp and the submission ordinal.
        receipt.hook = NativeHook::parse("claude", br#"{"session_id":"session","prompt_id":"b","hook_event_name":"UserPromptSubmit","prompt":"second prompt"}"#).unwrap();
        receipt.source_id = "start-b".into();
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        let current_start = receipt_for(&db, 1, "start-b");
        assert!(current_start.submission_correlated);
        assert_eq!(current_start.input_stamp.as_deref(), Some("input-b"));
        assert_eq!(current_start.submission_seq, Some(2), "B is the second recorded submission");
        receipt.hook = NativeHook::parse("claude", br#"{"session_id":"session","prompt_id":"b","hook_event_name":"Stop"}"#).unwrap();
        receipt.source_id = "stop-b".into();
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        let bound_stop = receipt_for(&db, 1, "stop-b");
        assert_eq!(bound_stop.input_stamp.as_deref(), Some("input-b"));
        assert!(bound_stop.submission_correlated);
        assert_eq!(
            bound_stop.submission_seq.as_ref(),
            current_start.submission_seq.as_ref(),
            "the Stop inherits the exact submission its turn start bound"
        );
        // An unobserved start cannot acquire authority from receipt-time input.
        receipt.hook.turn_id = Some("missing-start".into());
        receipt.source_id = "stop-c".into();
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        let missing = receipt_for(&db, 1, "stop-c");
        assert_eq!(missing.input_stamp, None);
        assert!(!missing.submission_correlated);
        assert_eq!(missing.submission_seq, None);
    }

    #[test]
    fn claude_submission_correlation_binds_a_turn_only_to_a_provable_submission() {
        use crate::services::circuit_worker::native_hooks::{NativeHook, NativeReceipt};
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO agent_nodes (id,mesh_id,name,path) VALUES (9,1,'owned','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,attempt,status,agent_node_id) VALUES (1,'spawn',1,'running',9);").unwrap();
        db.execute("UPDATE autopilot_circuits SET graph_json=?1", [crate::autopilot::circuit::model::CircuitGraph::walking_skeleton("work").to_json().unwrap()]).unwrap();

        // Buildmesh submits exactly this text.
        record_prompt_submission_locked(&db, 1, "spawn", 1, 9, "run the tests").unwrap();
        let submit = |turn: &str, prompt: &str, stamp: &str| NativeReceipt {
            agent_node_id: 9,
            input_stamp: Some(stamp.into()),
            session_incarnation: Some("1000".into()),
            source_id: format!("start-{turn}"),
            received_at_ms: 1000,
            turn_fenced: true,
            explicit_turn_mismatch: false,
            // Never trusted: correlation is decided from durable evidence.
            submission_correlated: true, submission_seq: None,
            hook: NativeHook::parse(
                "claude",
                serde_json::to_vec(&serde_json::json!({
                    "session_id": "session", "prompt_id": turn,
                    "hook_event_name": "UserPromptSubmit", "prompt": prompt,
                }))
                .unwrap()
                .as_slice(),
            )
            .unwrap(),
        };
        let stop = |turn: &str| NativeReceipt {
            agent_node_id: 9,
            input_stamp: Some("1:9".into()),
            session_incarnation: Some("1000".into()),
            source_id: format!("stop-{turn}"),
            received_at_ms: 2000,
            turn_fenced: true,
            explicit_turn_mismatch: false,
            submission_correlated: true, submission_seq: None,
            hook: NativeHook::parse(
                "claude",
                serde_json::to_vec(&serde_json::json!({
                    "session_id": "session", "prompt_id": turn, "hook_event_name": "Stop",
                }))
                .unwrap()
                .as_slice(),
            )
            .unwrap(),
        };

        // Happy path: the harness echoed back the exact text Buildmesh
        // wrote, so the turn token is bound to that submission.
        let start = submit("turn-1", "run the tests", "1:9");
        receive_native_hook_locked(&mut db, &start).unwrap();
        let start_receipt = receipt_for(&db, 1, "start-turn-1");
        assert!(start_receipt.submission_correlated, "content match earns the binding");
        assert_eq!(start_receipt.input_stamp.as_deref(), Some("1:9"));
        assert_eq!(start_receipt.submission_seq, Some(1), "the first recorded submission");
        receive_native_hook_locked(&mut db, &stop("turn-1")).unwrap();
        // Addressed by the Stop's own source_id. A lookup that also matched
        // the turn start would return that row instead and pass even if Stop
        // correlation had failed entirely.
        let stop_receipt = receipt_for(&db, 1, "stop-turn-1");
        assert!(stop_receipt.submission_correlated, "Stop inherits the turn's binding");
        assert_eq!(stop_receipt.input_stamp.as_deref(), Some("1:9"));
        assert_eq!(
            stop_receipt.submission_seq, start_receipt.submission_seq,
            "the terminal receipt records which submission it completed"
        );
        // Redelivery of the same start is idempotent, not a second claim.
        receive_native_hook_locked(&mut db, &start).unwrap();
        assert_eq!(receipt_count(&db, 1), 2, "redelivery is deduplicated, not appended");

        // A different turn reporting the same text must not take over a
        // submission another turn already acknowledged.
        receive_native_hook_locked(&mut db, &submit("turn-2", "run the tests", "1:9")).unwrap();
        let second = receipt_for(&db, 1, "start-turn-2");
        assert!(!second.submission_correlated, "one submission, one acknowledged turn");
        assert_eq!(second.input_stamp, None);
        assert_eq!(second.submission_seq, None);

        // Text Buildmesh never submitted cannot claim anything.
        receive_native_hook_locked(&mut db, &submit("turn-3", "an operator prompt", "1:9")).unwrap();
        let foreign = receipt_for(&db, 1, "start-turn-3");
        assert!(!foreign.submission_correlated);
        assert_eq!(foreign.input_stamp, None);
        // A hook with no turn token has nothing to bind in the first place.
        let untokened = NativeReceipt {
            source_id: "start-no-token".into(),
            hook: NativeHook::parse(
                "claude",
                br#"{"session_id":"session","hook_event_name":"UserPromptSubmit","prompt":"run the tests"}"#,
            )
            .unwrap(),
            ..submit("unused", "run the tests", "1:9")
        };
        receive_native_hook_locked(&mut db, &untokened).unwrap();
        let anonymous = receipt_for(&db, 1, "start-no-token");
        assert!(!anonymous.submission_correlated, "no prompt_id means no turn to bind");
        assert_eq!(anonymous.input_stamp, None);

        // A start hook that arrives after Buildmesh submitted again describes
        // an older turn, so it must not claim the newer submission.
        record_prompt_submission_locked(&db, 1, "spawn", 1, 9, "run the linter").unwrap();
        receive_native_hook_locked(&mut db, &submit("turn-late", "run the tests", "1:11")).unwrap();
        let late = receipt_for(&db, 1, "start-turn-late");
        assert!(!late.submission_correlated, "a delayed start cannot claim a superseded submission");
        assert_eq!(late.input_stamp, None);
        // ...and its Stop is equally uncorrelated.
        receive_native_hook_locked(&mut db, &stop("turn-late")).unwrap();
        let late_stop = receipt_for(&db, 1, "stop-turn-late");
        assert!(!late_stop.submission_correlated);
        assert_eq!(late_stop.input_stamp, None);
        assert_eq!(late_stop.submission_seq, None);
        // The current submission still correlates, proving the refusal above
        // was about ordering and not a broken digest comparison — and that a
        // later submission is free to claim its own turn.
        receive_native_hook_locked(&mut db, &submit("turn-4", "run the linter", "1:11")).unwrap();
        let current = receipt_for(&db, 1, "start-turn-4");
        assert!(current.submission_correlated);
        assert_eq!(current.submission_seq, Some(2));

        // A submission recorded against another step is not this step's
        // evidence, even for the same agent and the same text.
        record_prompt_submission_locked(&db, 1, "other-step", 1, 9, "cross-step text").unwrap();
        receive_native_hook_locked(&mut db, &submit("turn-5", "cross-step text", "1:12")).unwrap();
        let crossed = receipt_for(&db, 1, "start-turn-5");
        assert!(!crossed.submission_correlated, "another step's submission cannot bind this step");
        assert_eq!(crossed.input_stamp, None);
    }

    #[test]
    fn claude_submission_correlation_never_accepts_a_receipt_time_claim() {
        use crate::services::circuit_worker::native_hooks::{NativeHook, NativeReceipt};
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO agent_nodes (id,mesh_id,name,path) VALUES (9,1,'owned','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,attempt,status,agent_node_id) VALUES (1,'spawn',1,'running',9);").unwrap();
        db.execute("UPDATE autopilot_circuits SET graph_json=?1", [crate::autopilot::circuit::model::CircuitGraph::walking_skeleton("work").to_json().unwrap()]).unwrap();
        // No submission was ever recorded, so nothing can be correlated even
        // though the receipt asserts it and carries a plausible turn token.
        let receipt = NativeReceipt {
            agent_node_id: 9,
            input_stamp: Some("1:2".into()),
            session_incarnation: Some("1000".into()),
            source_id: "claimed".into(),
            received_at_ms: 1,
            turn_fenced: true,
            explicit_turn_mismatch: false,
            submission_correlated: true, submission_seq: None,
            hook: NativeHook::parse(
                "claude",
                br#"{"session_id":"session","prompt_id":"turn-1","hook_event_name":"UserPromptSubmit","prompt":"anything"}"#,
            )
            .unwrap(),
        };
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        let history = history_inner(&db, 1).unwrap();
        let stored: NativeReceipt = serde_json::from_str(&history[0].detail).unwrap();
        assert!(!stored.submission_correlated, "a receipt cannot vouch for itself");
        assert_eq!(stored.input_stamp, None, "an uncorrelated receipt keeps no input authority");
    }

    #[test]
    fn recorded_prompt_submissions_never_persist_the_prompt_text() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO agent_nodes (id,mesh_id,name,path) VALUES (9,1,'owned','/repo'),(10,1,'other','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,attempt,status,agent_node_id) VALUES (1,'spawn',1,'running',9);").unwrap();
        record_prompt_submission_locked(&db, 1, "spawn", 1, 9, "deploy with token hunter2").unwrap();
        let detail: String = db
            .query_row("SELECT detail FROM circuit_run_history WHERE kind='prompt_submitted'", [], |row| row.get(0))
            .unwrap();
        assert!(!detail.contains("hunter2"), "the ledger must keep only the digest");
        let submission: PromptSubmission = serde_json::from_str(&detail).unwrap();
        assert_eq!(submission.agent_node_id, 9);
        assert_eq!(submission.submission_seq, 1);
        assert_eq!(
            submission.prompt_digest,
            crate::services::circuit_worker::native_hooks::submission_digest("deploy with token hunter2")
        );
        // A second submission for the same agent advances the ordinal; a
        // different agent has its own sequence.
        record_prompt_submission_locked(&db, 1, "spawn", 1, 9, "next").unwrap();
        record_prompt_submission_locked(&db, 1, "spawn", 1, 10, "other node").unwrap();
        let seqs: Vec<i64> = db
            .prepare("SELECT json_extract(detail,'$.submission_seq') FROM circuit_run_history WHERE kind='prompt_submitted' ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<SqlResult<Vec<_>>>()
            .unwrap();
        assert_eq!(seqs, vec![1, 2, 1], "ordinals are per agent node, not global");
    }

    #[test]
    fn recheck_retains_ownership_uncertainty_and_cannot_authorize_classifier_completion() {
        use crate::autopilot::circuit::{
            context::CircuitContext,
            model::{CircuitGraph, CircuitNodeKind},
            observation::WorkEvidence,
            stepper::{advance, CircuitEvent, RunState, RunView, StepStatus, StepView},
        };
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'agent','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,source_agent_node_id) VALUES(1,1,1,'running',9);
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id) VALUES(1,'spawn',1,'unverified',9);").unwrap();
        let mut graph = CircuitGraph::walking_skeleton("");
        graph
            .nodes
            .iter_mut()
            .find(|node| node.id == "spawn")
            .unwrap()
            .kind = CircuitNodeKind::LlmTurnClassifier {
            target_node_id: Some("$source".into()),
        };
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [graph.to_json().unwrap()],
        )
        .unwrap();
        let mut evidence = WorkEvidence {
            foreground_terminated: true,
            ..Default::default()
        };
        evidence.sources.insert("native-event".into(), 1000);
        let mut context = CircuitContext::default();
        context.set("source.agent_id", "9");
        context.set(
            "node.spawn.evidence.1",
            serde_json::to_string(&evidence).unwrap(),
        );
        db.execute(
            "UPDATE autopilot_circuit_runs SET context_json=?1",
            [context.to_json().unwrap()],
        )
        .unwrap();
        let request = CheckpointRequest {
            run_id: 1,
            node_id: "spawn".into(),
            attempt: 1,
            expected_revision: 0,
            action: CheckpointAction::Recheck,
            reason: "Inspect the same session again".into(),
        };
        record_outcome_locked(&mut db, &request).unwrap();
        let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap();
        let context = CircuitContext::from_json(&stored.context_json).unwrap();
        assert_eq!(context.get("node.spawn.recheck_only"), Some("1"));
        let retained: WorkEvidence =
            serde_json::from_str(context.get("node.spawn.evidence.1").unwrap()).unwrap();
        assert_eq!(retained.sources.get("native-event"), Some(&1000));
        assert!(!retained.ownership_covered);
        let mut view = RunView {
            run_id: 1,
            state: RunState::Running,
            graph,
            context,
            steps: vec![StepView {
                node_id: "spawn".into(),
                attempt: 1,
                status: StepStatus::Running,
                agent_node_id: Some(9),
                outcome: None,
                error: None,
            }],
        };
        let transition = advance(
            &mut view,
            &CircuitEvent::TurnClassified { binding: None,
                node_id: "spawn".into(),
                classification: Some(crate::autopilot::evaluator::Classification::Completed),
                output: Some("Done".into()),
            },
        );
        assert!(transition.effects.is_empty());
        assert_eq!(view.steps[0].status, StepStatus::Unverified);
        let write = &transition.step_writes[0];
        commit_transition_locked(
            &mut db,
            1,
            None,
            &view.context.to_json().unwrap(),
            &[super::super::CircuitStepOp {
                node_id: write.node_id.clone(),
                status: write.status.as_db_str().into(),
                attempt: write.attempt,
                outcome: write
                    .outcome
                    .map(|value| value.map(|outcome| outcome.as_db_str().into())),
                error: write.error.clone(),
                agent_node_id: None,
                fresh_attempt: false,
            }],
            EvidenceWrite {
                expected: transition.expected.as_ref(),
                classifications: &transition.classifications,
                ..Default::default()
            },
        )
        .unwrap();
        let steps = super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap();
        assert_eq!(
            (steps[0].status.as_str(), steps[0].attempt),
            ("unverified", 1)
        );
        let history = history_inner(&db, 1).unwrap();
        let recorded: crate::autopilot::circuit::observation::RecordedClassification = serde_json::from_str(&history.iter().find(|entry| entry.kind == "classification").expect("interpretation recorded atomically").detail).unwrap();
        assert!(!recorded.lifecycle_verified);
        assert_eq!(recorded.report_revision, None);
        assert_eq!(recorded.interpretation, crate::autopilot::circuit::observation::ReportInterpretation::Completed);
        assert!(record_outcome_locked(&mut db, &request).is_err());
        assert!(history_inner(&db, 1)
            .unwrap()
            .iter()
            .any(|entry| entry.kind == "evidence_recheck"));
    }

    #[test]
    fn receipt_commit_rejects_replaced_sessions_without_advancing_cursor_or_history() {
        use crate::autopilot::circuit::stepper::ObservationInputFence;
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO agent_nodes (id,mesh_id,name,path,status,cli_session_id,session_started_at) VALUES (9,1,'agent','/repo','running','session',1000);
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');").unwrap();
        let guard = ObservationInputFence {
                transcript_guard: None, report_guard: None,
            agent_node_id: 9,
            input_stamp: "1:2".into(),
            observed_at_ms: 2000,
            session_id: "session".into(),
            session_incarnation: "1000".into(),
        };
        for change in [
            "UPDATE agent_nodes SET cli_session_id='replacement' WHERE id=9",
            "UPDATE agent_nodes SET cli_session_id='session',session_started_at=1001 WHERE id=9",
            "UPDATE agent_nodes SET session_started_at=1000,status='archived' WHERE id=9",
        ] {
            db.execute_batch(change).unwrap();
            let before = super::super::ledger::get_circuit_run_inner(&db, 1)
                .unwrap()
                .unwrap()
                .context_json;
            assert!(commit_transition_locked(
                &mut db,
                1,
                None,
                "{\"observer.receipt_cursor\":\"5\"}",
                &[],
                EvidenceWrite {
                    input_guard: Some(&guard),
                    intents: &[],
                    agent_status_effects: &[],
                    reconciled_effects: &[],
                    observations: &[],
                    classifications: &[],
                    expected: None,
                }
            )
            .is_err());
            assert_eq!(
                super::super::ledger::get_circuit_run_inner(&db, 1)
                    .unwrap()
                    .unwrap()
                    .context_json,
                before
            );
            assert!(history_inner(&db, 1).unwrap().is_empty());
        }
        db.execute_batch("UPDATE agent_nodes SET status='running' WHERE id=9")
            .unwrap();
        commit_transition_locked(
            &mut db,
            1,
            None,
            "{\"observer.receipt_cursor\":\"5\"}",
            &[],
            EvidenceWrite {
                input_guard: Some(&guard),
                intents: &[],
                agent_status_effects: &[],
                reconciled_effects: &[],
                observations: &[],
                classifications: &[],
                expected: None,
            },
        )
        .unwrap();
        assert_eq!(
            super::super::ledger::get_circuit_run_inner(&db, 1)
                .unwrap()
                .unwrap()
                .context_json,
            "{\"observer.receipt_cursor\":\"5\"}"
        );
    }

    #[test]
    fn observation_projection_and_effect_intent_commit_atomically() {
        use crate::autopilot::circuit::observation::*;
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            CREATE TRIGGER reject_observation BEFORE INSERT ON circuit_run_history WHEN NEW.kind='observation'
            BEGIN SELECT RAISE(ABORT,'injected append failure'); END;").unwrap();
        let observation = RecordedObservation {
            disposition: ObservationDisposition::ReducedConfidence,
            observation: CircuitObservation {
                identity: ObservationIdentity {
                    run_id: 1,
                    step_id: "work".into(),
                    attempt: 1,
                    agent_node_id: 9,
                    session_incarnation: None,
                    session_id: None,
                    turn_id: None,
                    report_revision: None,
                },
                source: "projection".into(),
                source_id: Some("source-1".into()),
                observed_at_ms: 123,
                authoritative: false,
                fact: ObservedWorkFact::Yielded,
            },
        };
        let op = super::super::CircuitStepOp {
            node_id: "work".into(),
            status: "running".into(),
            attempt: 1,
            outcome: None,
            error: None,
            agent_node_id: None,
            fresh_attempt: false,
        };
        let intent = EffectIntent {
            node_id: "work".into(),
            attempt: 1,
            kind: EffectKind::Github,
        };
        let write = || EvidenceWrite {
            input_guard: None,
            intents: std::slice::from_ref(&intent),
            agent_status_effects: &[],
            reconciled_effects: &[],
            observations: std::slice::from_ref(&observation),
            classifications: &[],
            expected: None,
        };
        assert!(commit_transition_locked(
            &mut db,
            1,
            None,
            "{\"watermark\":\"source-1\"}",
            std::slice::from_ref(&op),
            write()
        )
        .is_err());
        assert!(super::super::ledger::list_circuit_run_steps_inner(&db, 1)
            .unwrap()
            .is_empty());
        assert!(history_inner(&db, 1).unwrap().is_empty());
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM circuit_effects", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        db.execute_batch("DROP TRIGGER reject_observation").unwrap();
        commit_transition_locked(
            &mut db,
            1,
            None,
            "{\"watermark\":\"source-1\"}",
            &[op],
            write(),
        )
        .unwrap();
        assert_eq!(
            super::super::ledger::get_circuit_run_inner(&db, 1)
                .unwrap()
                .unwrap()
                .context_json,
            "{\"watermark\":\"source-1\"}"
        );
        let history = history_inner(&db, 1).unwrap();
        let recorded: RecordedObservation = serde_json::from_str(
            &history
                .iter()
                .find(|entry| entry.kind == "observation")
                .unwrap()
                .detail,
        )
        .unwrap();
        assert_eq!(recorded, observation);
        assert_eq!(
            db.query_row("SELECT state FROM circuit_effects", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "intent"
        );
    }

    #[test]
    fn set_node_status_and_step_commit_atomically_across_restart() {
        use crate::autopilot::circuit::model::{
            CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition,
            SessionStatusKind,
        };
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut db = Connection::open(file.path()).unwrap();
        crate::db::init_schema(&db).unwrap();
        let graph = CircuitGraph {
            version: 1,
            blueprint: None,
            nodes: vec![
                CircuitNode { id: "agent".into(), kind: CircuitNodeKind::SpawnAgentNode {
                    prompt: "work".into(), name: None, provider: None, model: None,
                    effort: None, extra_args: None, timeout_seconds: None,
                } },
                CircuitNode { id: "status".into(), kind: CircuitNodeKind::SetNodeStatus {
                    status: SessionStatusKind::Running,
                    target_node_id: Some("agent".into()),
                } },
            ],
            edges: vec![CircuitEdge { from: "agent".into(), to: "status".into(), condition: EdgeCondition::Always }],
        };
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(9,1,'agent','/repo','ready');").unwrap();
        crate::db::circuit::ledger::create_autopilot_circuit_inner(
            &db, 1, "status recovery", "", 1, &graph.to_json().unwrap(),
        ).unwrap();
        db.execute_batch("INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
                VALUES(1,'agent',1,'completed',9),(1,'status',1,'running',NULL);
            CREATE TRIGGER reject_status_ack BEFORE INSERT ON circuit_run_history
                WHEN NEW.kind='effect_result'
                BEGIN SELECT RAISE(ABORT,'injected status acknowledgement failure'); END;").unwrap();
        let steps = [
            super::super::CircuitStepOp {
                node_id: "status".into(), status: "completed".into(), outcome: None,
                error: None, agent_node_id: None, attempt: 1, fresh_attempt: false,
            },
        ];
        let status_effects = [AgentStatusEffect {
            node_id: "status".into(), attempt: 1, agent_node_id: 9,
            status: crate::models::SessionStatus::Running,
        }];
        assert!(commit_transition_locked(
            &mut db, 1, None, "{}", &steps,
            EvidenceWrite { agent_status_effects: &status_effects, ..Default::default() },
        ).is_err());
        let status: String = db.query_row("SELECT status FROM agent_nodes WHERE id=9", [], |row| row.get(0)).unwrap();
        let step: String = db.query_row("SELECT status FROM autopilot_circuit_run_steps WHERE run_id=1 AND node_id='status'", [], |row| row.get(0)).unwrap();
        assert_eq!((status.as_str(), step.as_str()), ("ready", "running"), "a failed append rolls back both the local effect and step completion");

        db.execute_batch("DROP TRIGGER reject_status_ack").unwrap();
        commit_transition_locked(
            &mut db, 1, None, "{}", &steps,
            EvidenceWrite { agent_status_effects: &status_effects, ..Default::default() },
        ).unwrap();
        drop(db);

        let reopened = Connection::open(file.path()).unwrap();
        let status: String = reopened.query_row("SELECT status FROM agent_nodes WHERE id=9", [], |row| row.get(0)).unwrap();
        let step: String = reopened.query_row("SELECT status FROM autopilot_circuit_run_steps WHERE run_id=1 AND node_id='status'", [], |row| row.get(0)).unwrap();
        let effect: String = reopened.query_row(
            "SELECT detail FROM circuit_run_history WHERE run_id=1 AND kind='effect_result'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!((status.as_str(), step.as_str()), ("running", "completed"));
        assert_eq!(serde_json::from_str::<serde_json::Value>(&effect).unwrap()["effect"], "set_node_status");
    }

    #[test]
    fn set_node_status_does_not_apply_after_circuit_run_is_cancelled_or_deleted() {
        use crate::autopilot::circuit::{
            model::{CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition, SessionStatusKind},
            stepper::{RunState, StepStatus, StepView, TransitionFence},
        };
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        let graph = CircuitGraph {
            version: 1,
            blueprint: None,
            nodes: vec![
                CircuitNode { id: "agent".into(), kind: CircuitNodeKind::SpawnAgentNode {
                    prompt: "work".into(), name: None, provider: None, model: None,
                    effort: None, extra_args: None, timeout_seconds: None,
                } },
                CircuitNode { id: "status".into(), kind: CircuitNodeKind::SetNodeStatus {
                    status: SessionStatusKind::Running,
                    target_node_id: Some("agent".into()),
                } },
            ],
            edges: vec![CircuitEdge { from: "agent".into(), to: "status".into(), condition: EdgeCondition::Always }],
        };
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(9,1,'agent','/repo','ready');").unwrap();
        crate::db::circuit::ledger::create_autopilot_circuit_inner(
            &db, 1, "cancelled status recovery", "", 1, &graph.to_json().unwrap(),
        ).unwrap();
        db.execute_batch("INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'cancelled');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status)
                VALUES(1,'status',1,'cancelled');").unwrap();
        let expected = TransitionFence {
            state: RunState::Cancelled,
            steps: vec![StepView {
                node_id: "status".into(),
                attempt: 1,
                status: StepStatus::Cancelled,
                outcome: None,
                error: None,
                agent_node_id: None,
            }],
            revision: None,
        };
        let steps = [super::super::CircuitStepOp {
            node_id: "status".into(), status: "completed".into(), outcome: None,
            error: None, agent_node_id: None, attempt: 1, fresh_attempt: false,
        }];
        let status_effects = [AgentStatusEffect {
            node_id: "status".into(), attempt: 1, agent_node_id: 9,
            status: crate::models::SessionStatus::Running,
        }];

        assert!(commit_transition_locked(
            &mut db,
            1,
            None,
            "{}",
            &steps,
            EvidenceWrite {
                agent_status_effects: &status_effects,
                expected: Some(&expected),
                ..Default::default()
            },
        ).is_err(), "a terminal run must reject its late local status effect");
        let status: String = db.query_row("SELECT status FROM agent_nodes WHERE id=9", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "ready");

        db.execute("DELETE FROM autopilot_circuit_runs WHERE id=1", []).unwrap();
        assert!(commit_transition_locked(
            &mut db,
            1,
            None,
            "{}",
            &steps,
            EvidenceWrite {
                agent_status_effects: &status_effects,
                ..Default::default()
            },
        ).is_err(), "a deleted run must also reject a late local status effect");
        let status: String = db.query_row("SELECT status FROM agent_nodes WHERE id=9", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "ready");
    }

    #[test]
    fn unknown_open_pr_checkpoint_offers_read_only_recheck_for_same_attempt() {
        use crate::autopilot::circuit::model::{
            CircuitGraph, CircuitNode, CircuitNodeKind, GithubActionKind,
        };
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'open_pr',1,'unverified');
            INSERT INTO circuit_effects VALUES(1,'open_pr',1,'github','uncertain');
            INSERT INTO circuit_run_history(run_id,node_id,attempt,kind,detail) VALUES(1,'open_pr',1,'effect_possible_dispatch','github');
            INSERT INTO circuit_run_history(run_id,node_id,attempt,kind,detail) VALUES(1,'open_pr',1,'effect_target','{}');").unwrap();
        let graph = CircuitGraph {
            version: 1,
            blueprint: None,
            nodes: vec![CircuitNode {
                id: "open_pr".into(),
                kind: CircuitNodeKind::GithubAction {
                    action: GithubActionKind::OpenPr,
                    open_pr_policy: None,
                    label: None,
                    comment: None,
                },
            }],
            edges: vec![],
        };
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [graph.to_json().unwrap()],
        )
        .unwrap();

        let evidence = evidence_view_inner(&db, 1).unwrap();
        assert!(evidence.checkpoints[0]
            .actions
            .iter()
            .any(|action| matches!(action, CheckpointAction::Recheck)));
        let request = CheckpointRequest {
            run_id: 1,
            node_id: "open_pr".into(),
            attempt: 1,
            expected_revision: evidence.entries.last().unwrap().id,
            action: CheckpointAction::Recheck,
            reason: "Look for the pull request created by the interrupted request".into(),
        };
        record_outcome_locked(&mut db, &request).unwrap();

        let step = super::super::ledger::list_circuit_run_steps_inner(&db, 1)
            .unwrap()
            .remove(0);
        assert_eq!((step.status.as_str(), step.attempt), ("pending_slot", 1));
        let run = super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap();
        let context = crate::autopilot::circuit::context::CircuitContext::from_json(
            &run.context_json,
        )
        .unwrap();
        assert_eq!(context.get("node.open_pr.recheck_only"), Some("1"));
        assert_eq!(
            db.query_row(
                "SELECT state FROM circuit_effects WHERE run_id=1 AND node_id='open_pr' AND attempt=1",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "uncertain",
            "a read-only recheck does not claim or replay the create effect"
        );
        assert_eq!(
            history_inner(&db, 1).unwrap().last().unwrap().kind,
            "evidence_recheck"
        );
        use crate::autopilot::circuit::stepper::{
            advance, Capacity, CircuitEvent, Effect, RunState, StepStatus, StepView,
        };
        let mut view = crate::autopilot::circuit::stepper::RunView {
            run_id: 1,
            state: RunState::Running,
            graph,
            context,
            steps: vec![StepView {
                node_id: "open_pr".into(),
                attempt: 1,
                status: StepStatus::Queued,
                outcome: None,
                error: None,
                agent_node_id: None,
            }],
        };
        let transition = advance(
            &mut view,
            &CircuitEvent::Tick(Capacity {
                circuit_free_slots: 1,
                agent_free_slots: 1,
            }),
        );
        assert!(matches!(
            transition.effects.as_slice(),
            [Effect::CallGithub {
                node_id,
                action: GithubActionKind::OpenPr,
                ..
            }] if node_id == "open_pr"
        ));
        assert_eq!(view.steps[0].status, StepStatus::Running);

        // Commit the scheduled recheck before the worker performs its remote
        // lookup. The existing uncertain create attempt must stay unchanged.
        let writes = transition.step_writes.iter().map(|write| super::super::CircuitStepOp {
            node_id: write.node_id.clone(),
            status: write.status.as_db_str().into(),
            attempt: write.attempt,
            outcome: write.outcome.map(|outcome| outcome.map(|value| value.as_db_str().into())),
            error: write.error.clone(),
            agent_node_id: None,
            fresh_attempt: write.fresh_attempt,
        }).collect::<Vec<_>>();
        commit_transition_locked(
            &mut db,
            1,
            None,
            &view.context.to_json().unwrap(),
            &writes,
            EvidenceWrite { expected: transition.expected.as_ref(), ..Default::default() },
        ).unwrap();

        // A matching read-only lookup finishes after the operator cancels.
        // Its stepper result must not overwrite the terminal run or reconcile
        // the uncertain effect when the stale worker tries to commit it.
        let event = CircuitEvent::GithubActionResult {
            node_id: "open_pr".into(),
            success: true,
            pr_number: Some(314),
            pr_url: Some("https://github.com/example/buildmesh/pull/314".into()),
            pr_head_ref: Some("feature/circuit".into()),
            pr_title: Some("Implementation".into()),
            error: None,
        };
        view.context.set("node.open_pr.recheck_only", "0");
        view.context.set("node.open_pr.effect_reconciled_attempt", "1");
        let result = advance(&mut view, &event);
        assert_eq!(view.steps[0].status, StepStatus::Completed);
        assert_eq!(view.context.get("pr.number"), Some("314"));
        super::super::ledger::cancel_circuit_run_locked(&mut db, 1).unwrap();
        let completion = result.step_writes.iter().map(|write| super::super::CircuitStepOp {
            node_id: write.node_id.clone(),
            status: write.status.as_db_str().into(),
            attempt: write.attempt,
            outcome: write.outcome.map(|outcome| outcome.map(|value| value.as_db_str().into())),
            error: write.error.clone(),
            agent_node_id: None,
            fresh_attempt: write.fresh_attempt,
        }).collect::<Vec<_>>();
        let reconciled = ReconciledEffect {
            intent: EffectIntent { node_id: "open_pr".into(), attempt: 1, kind: EffectKind::Github },
            detail: "Read-only GitHub lookup found open pull request #314.".into(),
        };
        assert!(commit_transition_locked(
            &mut db,
            1,
            result.run_state_changed.then_some(view.state.as_db_str()),
            &view.context.to_json().unwrap(),
            &completion,
            EvidenceWrite {
                expected: result.expected.as_ref(),
                reconciled_effects: std::slice::from_ref(&reconciled),
                ..Default::default()
            },
        ).is_err());
        assert_eq!(super::super::ledger::get_circuit_run_inner(&db, 1).unwrap().unwrap().state, "cancelled");
        let step = super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap().remove(0);
        assert_eq!((step.status.as_str(), step.attempt), ("cancelled", 1));
        assert_eq!(db.query_row("SELECT state FROM circuit_effects WHERE run_id=1 AND node_id='open_pr' AND attempt=1", [], |row| row.get::<_, String>(0)).unwrap(), "uncertain");
        assert!(!history_inner(&db, 1).unwrap().iter().any(|entry| entry.kind == "effect_reconciled"));
        let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap();
        let context = crate::autopilot::circuit::context::CircuitContext::from_json(
            &stored.context_json,
        )
        .unwrap();
        assert_eq!(context.get("pr.number"), None);
    }

    #[test]
    fn feedback_attestation_respects_current_requests_without_requiring_review_approval() {
        use crate::autopilot::circuit::{context::CircuitContext, model::CircuitGraph};
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'source','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,outcome)
                VALUES(1,'verdict',1,'completed','working'),(1,'feedback',1,'unverified',NULL);
            INSERT INTO circuit_effects VALUES(1,'feedback',1,'prompt','uncertain');").unwrap();
        let graph = CircuitGraph::agent_review(None,None,3);
        db.execute("UPDATE autopilot_circuits SET graph_json=?1", [graph.to_json().unwrap()]).unwrap();
        let mut context = CircuitContext::default();
        context.set("source.agent_id","9");
        context.set("node.feedback.human_wait","1");
        db.execute("UPDATE autopilot_circuit_runs SET context_json=?1", [context.to_json().unwrap()]).unwrap();
        let request = CheckpointRequest { run_id:1,node_id:"feedback".into(),attempt:1,expected_revision:0,
            action:CheckpointAction::Completed,reason:"Submitted the staged feedback in the source terminal and confirmed acceptance".into() };
        assert!(record_outcome_locked(&mut db,&request).is_err());
        context.set("node.feedback.human_wait","0");
        db.execute("UPDATE autopilot_circuit_runs SET context_json=?1", [context.to_json().unwrap()]).unwrap();
        record_outcome_locked(&mut db,&request).unwrap();
        let steps = super::super::ledger::list_circuit_run_steps_inner(&db,1).unwrap();
        assert_eq!(steps.iter().find(|s| s.node_id == "feedback").unwrap().status,"completed");
        assert_eq!(steps.iter().find(|s| s.node_id == "verdict").unwrap().outcome.as_deref(),Some("working"));
        assert_eq!(history_inner(&db,1).unwrap().iter().filter(|e| e.kind == "operator_attestation").count(),1);
    }

    #[test]
    fn operator_completion_advances_evidence_checkpoint_without_forging_observations() {
        use crate::autopilot::circuit::{context::CircuitContext, model::{CircuitGraph, CircuitNodeKind},
            observation::WorkEvidence, stepper::{advance, Capacity, CircuitEvent, RunState, RunView, StepStatus, StepView}};
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'agent','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
            VALUES(1,'spawn',1,'unverified',9);").unwrap();
        let mut graph = CircuitGraph::walking_skeleton("");
        graph.blueprint = None;
        graph.nodes.retain(|node| node.id != "inject");
        graph.edges.retain(|edge| edge.from != "inject");
        graph.edges.iter_mut().find(|edge| edge.to == "inject").unwrap().to = "notify".into();
        db.execute("UPDATE autopilot_circuits SET graph_json=?1", [graph.to_json().unwrap()]).unwrap();
        let request = CheckpointRequest { run_id:1, node_id:"spawn".into(), attempt:1,
            expected_revision:0, action:CheckpointAction::Completed,
            reason:"Inspected the finished work and terminal; advance this handoff".into() };
        for kind in [CircuitNodeKind::ReviewVerdict { target_node_id:None },
            CircuitNodeKind::CollaboratorCheck { require_approval:true }] {
            let mut protected = graph.clone();
            protected.nodes.iter_mut().find(|n| n.id == "spawn").unwrap().kind = kind;
            db.execute("UPDATE autopilot_circuits SET graph_json=?1", [protected.to_json().unwrap()]).unwrap();
            assert!(record_outcome_locked(&mut db, &request).is_err());
        }
        db.execute("UPDATE autopilot_circuits SET graph_json=?1", [graph.to_json().unwrap()]).unwrap();
        for evidence in [WorkEvidence { children:[("child".into(),false)].into_iter().collect(), ..Default::default() },
            WorkEvidence { conflicted:true, ..Default::default() }] {
            let mut context = CircuitContext::default();
            context.set("node.spawn.evidence.1", serde_json::to_string(&evidence).unwrap());
            db.execute("UPDATE autopilot_circuit_runs SET context_json=?1", [context.to_json().unwrap()]).unwrap();
            assert!(record_outcome_locked(&mut db, &request).is_err());
        }
        db.execute("UPDATE autopilot_circuit_runs SET context_json=?1", [r#"{"node.spawn.human_wait":"1"}"#]).unwrap();
        assert!(record_outcome_locked(&mut db, &request).is_err());
        db.execute("UPDATE autopilot_circuit_runs SET context_json='{}'", []).unwrap();
        assert!(evidence_view_inner(&db,1).unwrap().checkpoints[0].actions.iter().any(|a| matches!(a,CheckpointAction::Completed)));
        record_outcome_locked(&mut db, &request).unwrap();
        assert!(record_outcome_locked(&mut db, &request).is_err(), "stale operator action must not be replayed");
        let run = super::super::ledger::get_circuit_run_inner(&db,1).unwrap().unwrap();
        let steps = super::super::ledger::list_circuit_run_steps_inner(&db,1).unwrap();
        assert_eq!(steps[0].status,"completed");
        assert_eq!(steps[0].error_message,None, "the attestation belongs in history, not a completed step's error");
        assert_eq!(steps[0].attempt,1);
        let entries = history_inner(&db,1).unwrap();
        assert!(entries.iter().any(|e| e.kind == "operator_attestation" && e.source.as_deref() == Some("operator")));
        assert!(!entries.iter().any(|e| matches!(e.kind.as_str(),"observation"|"classification"|"effect_intent")));
        let mut view = RunView { run_id:1, graph, state:RunState::Running,
            context:CircuitContext::from_json(&run.context_json).unwrap(),
            steps: vec![StepView { node_id:"trigger".into(), status:StepStatus::Completed, attempt:1,
                outcome:Some(crate::autopilot::circuit::model::StepOutcome::Completed), error:None, agent_node_id:None },
                StepView { node_id:"spawn".into(), status:StepStatus::Completed, attempt:1,
                    outcome:Some(crate::autopilot::circuit::model::StepOutcome::Completed), error:steps[0].error_message.clone(), agent_node_id:Some(9) }] };
        advance(&mut view, &CircuitEvent::Tick(Capacity { circuit_free_slots:4,agent_free_slots:4 }));
        assert_eq!(view.state,RunState::Completed);
    }

    #[test]
    fn operator_outcome_requires_reason_and_fences_retry_attempt_and_history() {
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,status,attempt) VALUES (1,'comment','unverified',1);").unwrap();
        let graph = crate::autopilot::circuit::model::CircuitGraph {
            version: 2,
            blueprint: None,
            nodes: vec![crate::autopilot::circuit::model::CircuitNode {
                id: "comment".into(),
                kind: crate::autopilot::circuit::model::CircuitNodeKind::GithubAction {
                    action: crate::autopilot::circuit::model::GithubActionKind::PostComment,
                    label: None,
                    comment: Some("test".into()),
                    open_pr_policy: None,
                },
            }],
            edges: vec![],
        };
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [graph.to_json().unwrap()],
        )
        .unwrap();
        let mut request = CheckpointRequest {
            run_id: 1,
            node_id: "comment".into(),
            attempt: 1,
            expected_revision: 0,
            action: CheckpointAction::Retry,
            reason: "Checked the remote issue".into(),
        };
        let mut open_pr = graph.clone();
        if let crate::autopilot::circuit::model::CircuitNodeKind::GithubAction { action, .. } =
            &mut open_pr.nodes[0].kind
        {
            *action = crate::autopilot::circuit::model::GithubActionKind::OpenPr;
        }
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [open_pr.to_json().unwrap()],
        )
        .unwrap();
        request.action = CheckpointAction::Completed;
        assert!(record_outcome_locked(&mut db, &request)
            .unwrap_err()
            .contains("pull request identity"));
        assert!(!evidence_view_inner(&db, 1).unwrap().checkpoints[0]
            .actions
            .iter()
            .any(|a| matches!(a, CheckpointAction::Completed)));
        assert_eq!(
            super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap()[0].status,
            "unverified"
        );
        assert!(history_inner(&db, 1).unwrap().is_empty());
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [graph.to_json().unwrap()],
        )
        .unwrap();
        request.action = CheckpointAction::Retry;
        use crate::autopilot::circuit::stepper::{RunState, StepStatus, StepView, TransitionFence};
        let stale = TransitionFence {
            state: RunState::Running,
            revision: Some(0),
            steps: vec![StepView {
                node_id: "comment".into(),
                attempt: 1,
                status: StepStatus::Unverified,
                outcome: None,
                error: None,
                agent_node_id: None,
            }],
        };
        assert!(record_outcome_locked(&mut db, &request)
            .unwrap_err()
            .contains("not performed"));
        request.action = CheckpointAction::NotPerformed;
        request.reason.clear();
        assert!(record_outcome_locked(&mut db, &request)
            .unwrap_err()
            .contains("reason"));
        request.reason = "Confirmed request was rejected before dispatch".into();
        record_outcome_locked(&mut db, &request).unwrap();
        assert!(
            commit_transition_locked(
                &mut db,
                1,
                None,
                "{}",
                &[],
                EvidenceWrite {
                    expected: Some(&stale),
                    ..Default::default()
                }
            )
            .is_err(),
            "an observer loaded before the operator action must refresh, even at the same attempt"
        );
        assert_eq!(
            super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap()[0].attempt,
            1
        );
        request.action = CheckpointAction::Retry;
        assert!(record_outcome_locked(&mut db, &request)
            .unwrap_err()
            .contains("Refresh"));
        request.expected_revision = history_inner(&db, 1).unwrap().last().unwrap().id;
        record_outcome_locked(&mut db, &request).unwrap();
        let steps = super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap();
        assert_eq!(steps[0].attempt, 2);
        assert_eq!(steps[0].status, "pending_slot");
        assert!(
            commit_transition_locked(
                &mut db,
                1,
                None,
                "{}",
                &[],
                EvidenceWrite {
                    expected: Some(&stale),
                    ..Default::default()
                }
            )
            .is_err(),
            "stale worker must never roll the retry back to attempt one"
        );
        assert_eq!(
            db.query_row(
                "SELECT state FROM circuit_effects WHERE attempt=1",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "not_performed"
        );
        assert_eq!(
            history_inner(&db, 1)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "operator_attestation")
                .count(),
            2
        );
        request.expected_revision = history_inner(&db, 1).unwrap().last().unwrap().id;
        request.attempt = 2;
        db.execute("UPDATE autopilot_circuit_runs SET state='cancelled'", [])
            .unwrap();
        assert!(record_outcome_locked(&mut db, &request).is_err());
    }

    #[test]
    fn spawn_attachment_acknowledgement_is_atomic_fenced_and_deduplicated() {
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'first','/repo'),(10,1,'other','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'spawn',1,'running');
            INSERT INTO circuit_effects VALUES(1,'spawn',1,'spawn','intent');").unwrap();
        assert_eq!(acknowledge_spawn_attachment_locked(&mut db,1,"spawn",1,9,None,None).unwrap(),None, "unclaimed intent cannot acknowledge a spawn");
        let intent = EffectIntent { node_id:"spawn".into(),attempt:1,kind:EffectKind::Spawn };
        assert!(claim_effect_locked(&mut db,1,&intent).unwrap().is_some());
        db.execute_batch("CREATE TRIGGER reject_ack BEFORE INSERT ON circuit_run_history WHEN NEW.kind='effect_result'
            BEGIN SELECT RAISE(ABORT,'injected acknowledgement failure'); END;").unwrap();
        assert!(acknowledge_spawn_attachment_locked(&mut db,1,"spawn",1,9,None,None).is_err());
        let (agent, state): (Option<i64>, String) = db.query_row("SELECT s.agent_node_id,e.state FROM autopilot_circuit_run_steps s JOIN circuit_effects e ON e.run_id=s.run_id", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(agent,None);
        assert_eq!(state,"possible_dispatch");
        db.execute_batch("DROP TRIGGER reject_ack").unwrap();
        let revision = acknowledge_spawn_attachment_locked(&mut db,1,"spawn",1,9,None,None).unwrap().unwrap();
        assert_eq!(acknowledge_spawn_attachment_locked(&mut db,1,"spawn",1,9,None,None).unwrap(),Some(revision));
        assert_eq!(acknowledge_spawn_attachment_locked(&mut db,1,"spawn",1,10,None,None).unwrap(),None);
        assert_eq!(acknowledge_spawn_attachment_locked(&mut db,1,"spawn",2,9,None,None).unwrap(),None);
        db.execute("UPDATE autopilot_circuit_runs SET state='cancelled'",[]).unwrap();
        assert_eq!(acknowledge_spawn_attachment_locked(&mut db,1,"spawn",1,9,None,None).unwrap(),None);
        assert!(claim_effect_locked(&mut db,1,&intent).unwrap().is_none());
        assert_eq!(db.query_row("SELECT COUNT(*) FROM circuit_run_history WHERE kind='effect_result'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        // A new claimed retry may replace exactly the prior retained agent.
        db.execute_batch("UPDATE autopilot_circuit_runs SET state='running';
            UPDATE autopilot_circuit_run_steps SET attempt=2;
            INSERT INTO circuit_effects VALUES(1,'spawn',2,'spawn','possible_dispatch');").unwrap();
        assert_eq!(acknowledge_spawn_attachment_locked(&mut db,1,"spawn",2,10,None,None).unwrap(),None);
        assert!(acknowledge_spawn_attachment_locked(&mut db,1,"spawn",2,10,None,Some(9)).unwrap().is_some());
        assert_eq!(acknowledge_spawn_attachment_locked(&mut db,1,"spawn",2,9,None,Some(10)).unwrap(),None);
        // Delivery to a retained live agent is acknowledged as a prompt, not allocation.
        db.execute_batch("UPDATE autopilot_circuit_run_steps SET attempt=3;
            INSERT INTO circuit_effects VALUES(1,'spawn',3,'spawn','possible_dispatch');").unwrap();
        assert!(claim_effect_locked(&mut db,1,&EffectIntent { attempt:3,..intent.clone() }).unwrap().is_none(), "a crash after delivery must never replay it");
        assert!(acknowledge_spawn_attachment_locked(&mut db,1,"spawn",3,10,None,Some(10)).unwrap().is_some());
        assert!(history_inner(&db,1).unwrap().last().unwrap().detail.contains("Prompt delivered to retained agent"));
        db.execute_batch("UPDATE autopilot_circuit_run_steps SET attempt=4").unwrap();
        assert_eq!(acknowledge_spawn_attachment_locked(&mut db,1,"spawn",4,10,None,Some(10)).unwrap(),None,"missing dispatch claim");
    }

    #[test]
    fn prompt_acknowledgement_survives_restart_and_fences_cancelled_or_retried_steps() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut db = Connection::open(file.path()).unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'feedback',1,'running');
            INSERT INTO circuit_effects VALUES(1,'feedback',1,'prompt','possible_dispatch');").unwrap();
        assert_eq!(acknowledge_prompt_delivery_locked(&mut db, 1, "feedback", 2).unwrap(), None);
        db.execute_batch("CREATE TRIGGER reject_prompt_ack BEFORE INSERT ON circuit_run_history
            WHEN NEW.kind='effect_result' BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
        assert!(acknowledge_prompt_delivery_locked(&mut db, 1, "feedback", 1).is_err());
        assert_eq!(db.query_row("SELECT state FROM circuit_effects", [], |r| r.get::<_, String>(0)).unwrap(), "possible_dispatch");
        db.execute_batch("DROP TRIGGER reject_prompt_ack").unwrap();
        for state in ["cancelled", "failed", "completed"] {
            db.execute("UPDATE autopilot_circuit_runs SET state=?1", [state]).unwrap();
            assert_eq!(acknowledge_prompt_delivery_locked(&mut db, 1, "feedback", 1).unwrap(), None);
        }
        db.execute("UPDATE autopilot_circuit_runs SET state='paused'", []).unwrap();
        let revision = acknowledge_prompt_delivery_locked(&mut db, 1, "feedback", 1).unwrap().unwrap();
        assert_eq!(acknowledge_prompt_delivery_locked(&mut db, 1, "feedback", 1).unwrap(), None);
        append_history(&db, 1, Some("feedback"), Some(1), "native_hook", "{}", None, None).unwrap();
        assert!(revision_inner(&db, 1).unwrap() > revision, "later receipts may invalidate the projection commit");
        drop(db);
        let mut db = Connection::open(file.path()).unwrap();
        assert_eq!(db.query_row("SELECT state FROM circuit_effects", [], |r| r.get::<_, String>(0)).unwrap(), "acknowledged");
        assert_eq!(db.query_row("SELECT status FROM autopilot_circuit_run_steps", [], |r| r.get::<_, String>(0)).unwrap(), "running",
            "delivery receipt survives independently of step projection");
        db.execute("UPDATE autopilot_circuit_runs SET state='running'", []).unwrap();
        assert!(claim_effect_locked(&mut db, 1, &EffectIntent {
            node_id: "feedback".into(), attempt: 1, kind: EffectKind::Prompt,
        }).unwrap().is_none(), "recovery cannot dispatch the acknowledged prompt again");
    }

    #[test]
    fn effect_claim_survives_reopen_and_is_never_replayed() {
        for kind in [EffectKind::Github, EffectKind::Prompt, EffectKind::Spawn] {
            let file = tempfile::NamedTempFile::new().unwrap();
            let mut db = Connection::open(file.path()).unwrap();
            crate::db::init_schema(&db).unwrap();
            db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');").unwrap();
            let intent = EffectIntent {
                node_id: "comment".into(),
                attempt: 1,
                kind,
            };
            let op = super::super::CircuitStepOp {
                node_id: "comment".into(),
                status: "running".into(),
                attempt: 1,
                outcome: None,
                error: None,
                agent_node_id: None,
                fresh_attempt: false,
            };
            commit_transition_locked(
                &mut db,
                1,
                None,
                "{}",
                &[op],
                EvidenceWrite {
                    intents: std::slice::from_ref(&intent),
                    ..Default::default()
                },
            )
            .unwrap();
            drop(db);
            let mut reopened = Connection::open(file.path()).unwrap();
            assert!(claim_effect_locked(&mut reopened, 1, &intent).unwrap().is_some(),
                "an intent that crashed before dispatch remains claimable after restart");
            drop(reopened);
            let mut reopened = Connection::open(file.path()).unwrap();
            assert!(claim_effect_locked(&mut reopened, 1, &intent)
                .unwrap()
                .is_none());
            assert_eq!(
                reopened
                    .query_row("SELECT state FROM circuit_effects", [], |r| r
                        .get::<_, String>(0))
                    .unwrap(),
                "possible_dispatch"
            );
            assert_eq!(reopened.query_row("SELECT COUNT(*) FROM circuit_run_history WHERE kind='effect_possible_dispatch'", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
        }
    }

    #[test]
    fn open_pr_target_is_saved_only_after_claim_and_survives_reopen() {
        use crate::autopilot::circuit::model::{
            CircuitGraph, CircuitNode, CircuitNodeKind, GithubActionKind,
        };
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut db = Connection::open(file.path()).unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'open_pr',1,'running');
            INSERT INTO circuit_effects VALUES(1,'open_pr',1,'github','intent');").unwrap();
        let graph = CircuitGraph {
            version: 1,
            blueprint: None,
            nodes: vec![CircuitNode {
                id: "open_pr".into(),
                kind: CircuitNodeKind::GithubAction {
                    action: GithubActionKind::OpenPr,
                    open_pr_policy: None,
                    label: None,
                    comment: None,
                },
            }],
            edges: vec![],
        };
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [graph.to_json().unwrap()],
        )
        .unwrap();
        let target = r#"{"owner":"example","repo":"buildmesh","head":"feature/circuit"}"#;
        assert!(record_effect_target_locked(&mut db, 1, "open_pr", 1, target).is_err());
        db.execute(
            "UPDATE circuit_effects SET state='possible_dispatch' WHERE run_id=1 AND node_id='open_pr'",
            [],
        )
        .unwrap();
        let revision = record_effect_target_locked(&mut db, 1, "open_pr", 1, target).unwrap();
        assert_eq!(revision, history_inner(&db, 1).unwrap().last().unwrap().id);
        assert_eq!(latest_effect_target_inner(&db, 1, "open_pr", 1).unwrap().as_deref(), Some(target));
        drop(db);

        let reopened = Connection::open(file.path()).unwrap();
        assert_eq!(latest_effect_target_inner(&reopened, 1, "open_pr", 1).unwrap().as_deref(), Some(target));
        assert_eq!(
            reopened.query_row("SELECT COUNT(*) FROM circuit_run_history WHERE kind='effect_target'", [], |row| row.get::<_, i64>(0)).unwrap(),
            1
        );
    }

    #[test]
    fn open_pr_recheck_completion_reconciles_the_unknown_effect() {
        use crate::autopilot::circuit::model::{
            CircuitGraph, CircuitNode, CircuitNodeKind, GithubActionKind,
        };
        use crate::autopilot::circuit::stepper::{
            advance, CircuitEvent, RunState, RunView, StepStatus, StepView,
        };
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'open_pr',1,'running');
            INSERT INTO circuit_effects VALUES(1,'open_pr',1,'github','uncertain');").unwrap();
        let graph = CircuitGraph {
            version: 1,
            blueprint: None,
            nodes: vec![CircuitNode {
                id: "open_pr".into(),
                kind: CircuitNodeKind::GithubAction {
                    action: GithubActionKind::OpenPr,
                    open_pr_policy: None,
                    label: None,
                    comment: None,
                },
            }],
            edges: vec![],
        };
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [graph.to_json().unwrap()],
        )
        .unwrap();
        let mut view = RunView {
            run_id: 1,
            state: RunState::Running,
            graph,
            context: crate::autopilot::circuit::context::CircuitContext::default(),
            steps: vec![StepView {
                node_id: "open_pr".into(),
                attempt: 1,
                status: StepStatus::Running,
                outcome: None,
                error: None,
                agent_node_id: None,
            }],
        };
        let result = advance(&mut view, &CircuitEvent::GithubActionResult {
            node_id: "open_pr".into(),
            success: true,
            pr_number: Some(314),
            pr_url: Some("https://github.com/example/buildmesh/pull/314".into()),
            pr_head_ref: Some("feature/circuit".into()),
            pr_title: Some("Implementation".into()),
            error: None,
        });
        let writes = result.step_writes.iter().map(|write| super::super::CircuitStepOp {
            node_id: write.node_id.clone(),
            status: write.status.as_db_str().into(),
            attempt: write.attempt,
            outcome: write.outcome.map(|outcome| outcome.map(|value| value.as_db_str().into())),
            error: write.error.clone(),
            agent_node_id: None,
            fresh_attempt: write.fresh_attempt,
        }).collect::<Vec<_>>();
        let reconciled = ReconciledEffect {
            intent: EffectIntent {
                node_id: "open_pr".into(),
                attempt: 1,
                kind: EffectKind::Github,
            },
            detail: "Read-only GitHub lookup found open pull request #314.".into(),
        };
        commit_transition_locked(
            &mut db,
            1,
            result.run_state_changed.then_some(view.state.as_db_str()),
            &view.context.to_json().unwrap(),
            &writes,
            EvidenceWrite {
                reconciled_effects: std::slice::from_ref(&reconciled),
                expected: result.expected.as_ref(),
                ..Default::default()
            },
        )
        .unwrap();
        let stored = super::super::ledger::get_circuit_run_inner(&db, 1).unwrap().unwrap();
        let context = crate::autopilot::circuit::context::CircuitContext::from_json(&stored.context_json).unwrap();
        assert_eq!(stored.state, "completed");
        assert_eq!(context.get("pr.number"), Some("314"));
        assert_eq!(context.get("pr.head_ref"), Some("feature/circuit"));
        assert_eq!(super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap()[0].status, "completed");
        assert_eq!(
            db.query_row("SELECT state FROM circuit_effects", [], |row| row.get::<_, String>(0)).unwrap(),
            "acknowledged",
            "a matching read-only PR observation reconciles the prior uncertain dispatch"
        );
        assert_eq!(
            history_inner(&db, 1).unwrap().last().unwrap().kind,
            "effect_reconciled"
        );
        assert!(history_inner(&db, 1)
            .unwrap()
            .last()
            .unwrap()
            .detail
            .contains("#314"));
    }

    #[test]
    fn open_pr_recheck_after_not_performed_reconciles_without_losing_attestation() {
        use crate::autopilot::circuit::model::{
            CircuitGraph, CircuitNode, CircuitNodeKind, GithubActionKind,
        };
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'open_pr',1,'unverified');
            INSERT INTO circuit_effects VALUES(1,'open_pr',1,'github','uncertain');
            INSERT INTO circuit_run_history(run_id,node_id,attempt,kind,detail) VALUES(1,'open_pr',1,'effect_possible_dispatch','github');
            INSERT INTO circuit_run_history(run_id,node_id,attempt,kind,detail) VALUES(1,'open_pr',1,'effect_target','{\"owner\":\"example\",\"repo\":\"buildmesh\",\"head\":\"feature/circuit\"}');").unwrap();
        let graph = CircuitGraph {
            version: 1,
            blueprint: None,
            nodes: vec![CircuitNode {
                id: "open_pr".into(),
                kind: CircuitNodeKind::GithubAction {
                    action: GithubActionKind::OpenPr,
                    open_pr_policy: None,
                    label: None,
                    comment: None,
                },
            }],
            edges: vec![],
        };
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [graph.to_json().unwrap()],
        )
        .unwrap();

        let initial = evidence_view_inner(&db, 1).unwrap();
        let mut request = CheckpointRequest {
            run_id: 1,
            node_id: "open_pr".into(),
            attempt: 1,
            expected_revision: initial.entries.last().unwrap().id,
            action: CheckpointAction::NotPerformed,
            reason: "GitHub confirmed no matching pull request existed".into(),
        };
        record_outcome_locked(&mut db, &request).unwrap();

        let attested = evidence_view_inner(&db, 1).unwrap();
        assert!(attested.checkpoints[0]
            .actions
            .iter()
            .any(|action| matches!(action, CheckpointAction::Recheck)));
        request.expected_revision = attested.entries.last().unwrap().id;
        request.action = CheckpointAction::Recheck;
        request.reason = "Verify the saved repository branch without creating a pull request".into();
        record_outcome_locked(&mut db, &request).unwrap();

        let completion = super::super::CircuitStepOp {
            node_id: "open_pr".into(),
            status: "completed".into(),
            attempt: 1,
            outcome: Some(Some("completed".into())),
            error: None,
            agent_node_id: None,
            fresh_attempt: false,
        };
        let reconciled = ReconciledEffect {
            intent: EffectIntent {
                node_id: "open_pr".into(),
                attempt: 1,
                kind: EffectKind::Github,
            },
            detail: "Read-only GitHub lookup found open pull request #314.".into(),
        };
        commit_transition_locked(
            &mut db,
            1,
            None,
            "{}",
            &[completion],
            EvidenceWrite {
                reconciled_effects: std::slice::from_ref(&reconciled),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(
            db.query_row("SELECT state FROM circuit_effects", [], |row| row.get::<_, String>(0)).unwrap(),
            "acknowledged"
        );
        let history = history_inner(&db, 1).unwrap();
        assert!(history.iter().any(|entry| {
            entry.kind == "operator_attestation"
                && entry.detail.contains("NotPerformed")
                && entry.detail.contains("GitHub confirmed no matching pull request existed")
        }));
        assert_eq!(history.last().unwrap().kind, "effect_reconciled");
    }
}
