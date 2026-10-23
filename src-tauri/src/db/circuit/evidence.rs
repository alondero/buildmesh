//! Durable external-effect claims and append-only Circuit Run History.

use crate::db::{DbError, SqlResult};
use rusqlite::{params, Connection, OptionalExtension};

pub(crate) mod prompt_delivery;

const OBSERVATION_FRESHNESS_REJECTION_PREFIX: &str = "Observation freshness fence rejected:";

pub(crate) fn restore_projection_conflicts(
    run_id: i64,
    node_id: &str,
    attempt: i32,
    evidence: &mut crate::circuit::observation::WorkEvidence,
) -> SqlResult<bool> {
    let conn = crate::db::read_conn()?;
    restore_projection_conflicts_inner(&conn, run_id, node_id, attempt, evidence)
}

fn restore_projection_conflicts_inner(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    evidence: &mut crate::circuit::observation::WorkEvidence,
) -> SqlResult<bool> {
    let mut query = db.prepare(
        "SELECT detail FROM circuit_run_history
        WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='observation'
          AND source='agent_status_projection' AND disposition='conflicting'",
    )?;
    let rows = query.query_map(params![run_id, node_id, attempt], |row| {
        row.get::<_, String>(0)
    })?;
    let mut changed = false;
    for row in rows {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&row?) else {
            continue;
        };
        if let Some(observation) = value
            .get("observation")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
        {
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
    value: crate::circuit::observation::ObservationDisposition,
) -> &'static str {
    use crate::circuit::observation::ObservationDisposition as D;
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
    if binding {
        DISPOSITION_WAITING
    } else {
        DISPOSITION_RESOLVED
    }
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
    /// Completing this step starts another review pass.
    pub starts_next_review: bool,
    /// Completing this step uses the last review round.
    pub ends_review: bool,
    /// Review again can add another round after the run stops. False when
    /// recovery would reject this graph as a custom circuit.
    pub review_again: bool,
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
    pub human_waits: Vec<crate::circuit::observation::HumanWait>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub observation_blocker: Option<crate::circuit::observation::CircuitObservationBlocker>,
}

pub fn history(run_id: i64) -> Result<CircuitEvidenceView, DbError> {
    let db = crate::db::read_conn()?;
    let tx = db.unchecked_transaction()?;
    let (mut view, pending) = evidence_view_unresolved(&tx, run_id)?;
    tx.commit()?;
    // Resolving a harness reads preferences (profiles, launch selections),
    // which can cold-load `preferences.json`. Release the connection first so
    // no filesystem I/O happens while it is checked out.
    drop(db);
    resolve_pending_capabilities(&mut view, pending);
    Ok(view)
}

/// The stored provider string each coverage row still has to resolve into
/// diagnostics, aligned with `CircuitEvidenceView::coverage`. `None` marks a
/// row whose agent record is gone.
type PendingCapabilities = Vec<Option<crate::circuit::strategy::HarnessSelector>>;

fn resolve_pending_capabilities(view: &mut CircuitEvidenceView, pending: PendingCapabilities) {
    for (row, stored) in view.coverage.iter_mut().zip(pending) {
        if let Some(selector) = stored {
            row.capabilities =
                crate::services::circuit_worker::observer_policy::for_selector(&selector);
        }
    }
}

pub(super) fn recovery_view(
    db: &Connection,
    run: &crate::models::AutopilotCircuitRun,
) -> Result<crate::circuit::stepper::RunView, DbError> {
    use crate::circuit::{
        context::CircuitContext,
        model::StepOutcome,
        stepper::{RunState, RunView, StepStatus, StepView},
    };
    let steps = super::ledger::list_circuit_run_steps_inner(db, run.id)?;
    let mut view = RunView {
        run_id: run.id,
        graph: run_graph(db, run.id)?,
        state: RunState::from_db_str(&run.state),
        context: CircuitContext::from_json(&run.context_json)?,
        steps: steps
            .into_iter()
            .map(|step| StepView {
                node_id: step.node_id,
                attempt: step.attempt,
                status: StepStatus::from_db_str(&step.status),
                outcome: step.outcome.as_deref().and_then(StepOutcome::from_db_str),
                error: step.error_message,
                agent_node_id: step.agent_node_id,
            })
            .collect(),
    };
    for step in view.steps.clone() {
        let agent_id = step
            .agent_node_id
            .or_else(|| view.resolve_target_agent(&step.node_id));
        if let Some(agent_id) = agent_id {
            if let Some(blocker) =
                crate::db::agent_node::circuit_lifecycle_blocker_inner(db, agent_id)?
            {
                let encoded = serde_json::to_string(&blocker)?;
                view.context.set_lifecycle_blocker(&step.node_id, encoded);
            }
        }
    }
    Ok(view)
}

/// What a person may record against one Unverified step. Shared by the history
/// view and the run card's attention query so both offer the same actions.
fn checkpoint_actions(
    db: &Connection,
    run_id: i64,
    graph: &crate::circuit::model::CircuitGraph,
    view: &crate::circuit::stepper::RunView,
    step: &crate::models::AutopilotCircuitRunStep,
) -> Result<Vec<CheckpointAction>, DbError> {
    use crate::circuit::model::CircuitNodeKind;
    let actions = match graph.node(&step.node_id).map(|n| &n.kind) {
        Some(kind @ (CircuitNodeKind::GithubAction { .. } | CircuitNodeKind::InjectPty { .. })) => {
            let not_performed: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM circuit_effects WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND state='not_performed')",
                params![run_id,step.node_id,step.attempt], |r| r.get(0))?;
            let mut actions = vec![CheckpointAction::NotPerformed];
            if matches!(
                kind,
                CircuitNodeKind::GithubAction {
                    action: crate::circuit::model::GithubActionKind::OpenPr,
                    ..
                }
            ) {
                let has_target = has_effect_target(db, run_id, &step.node_id, step.attempt)?;
                if has_target {
                    actions.insert(0, CheckpointAction::Recheck);
                }
            } else if !view.report_has_known_blockers(&step.node_id) {
                actions.insert(0, CheckpointAction::Completed);
            }
            if not_performed {
                actions.push(CheckpointAction::Retry);
            }
            actions
        }
        Some(CircuitNodeKind::SpawnAgentNode { .. }) if step.agent_node_id.is_none() => {
            let not_performed: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM circuit_effects WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='spawn' AND state='not_performed')",
                params![run_id,step.node_id,step.attempt], |row| row.get(0))?;
            let mut actions = vec![CheckpointAction::Recheck, CheckpointAction::NotPerformed];
            if not_performed {
                actions.push(CheckpointAction::Retry);
            }
            actions
        }
        Some(CircuitNodeKind::ReviewVerdict { .. }) => {
            use crate::circuit::model::StepOutcome;
            let mut actions = vec![CheckpointAction::Recheck];
            if view.can_offer_review(&step.node_id, StepOutcome::Completed) {
                actions.push(CheckpointAction::ReviewPass);
            }
            if view.can_offer_review(&step.node_id, StepOutcome::Working) {
                actions.push(CheckpointAction::ReviewFail);
            }
            actions
        }
        Some(
            CircuitNodeKind::LlmTurnClassifier { .. }
            | CircuitNodeKind::AwaitAgentTurn { .. }
            | CircuitNodeKind::SpawnAgentNode { .. },
        ) => {
            let mut actions = vec![CheckpointAction::Recheck];
            if view.can_attest_completion(&step.node_id) {
                actions.push(CheckpointAction::Completed);
            }
            actions
        }
        _ => vec![],
    };
    let mut actions = actions;
    if prompt_delivery::available(db, run_id, &step.node_id, step.attempt)? {
        actions.insert(0, CheckpointAction::ManualDelivered);
    }
    Ok(actions)
}

/// Build the evidence view from database rows only. Coverage rows carry a
/// placeholder capability until `resolve_pending_capabilities` fills it in, so
/// the caller can release its connection before any preference I/O happens.
fn evidence_view_unresolved(
    db: &Connection,
    run_id: i64,
) -> Result<(CircuitEvidenceView, PendingCapabilities), DbError> {
    let mut pending = PendingCapabilities::new();
    let entries = history_inner(db, run_id)?;
    let run = super::ledger::get_circuit_run_inner(db, run_id)?
        .ok_or_else(|| DbError::not_found("Run no longer exists."))?;
    let mut checkpoints = Vec::new();
    let mut coverage = Vec::new();
    {
        let graph = run_graph(db, run_id)?;
        let context = crate::circuit::context::CircuitContext::from_json(&run.context_json)?;
        let steps = super::ledger::list_circuit_run_steps_inner(db, run_id)?;
        let view = recovery_view(db, &run)?;
        let continuation = super::recovery::review_continuation_supported(&run, &graph);
        for step in steps {
            if let Some(agent_id) = step
                .agent_node_id
                .or_else(|| view.resolve_target_agent(&step.node_id))
            {
                let agent =
                    crate::db::agent_node::get_agent_node_by_id_inner(db, agent_id).optional()?;
                let (platform, capabilities, stored) = match agent {
                    Some(agent) => (
                        format!("{} host / {} launch", std::env::consts::OS, agent.env),
                        crate::services::circuit_worker::observer_policy::for_missing_agent(),
                        Some(crate::circuit::strategy::selector_for_agent(&agent)),
                    ),
                    None => (
                        "Agent record unavailable".to_string(),
                        crate::services::circuit_worker::observer_policy::for_missing_agent(),
                        None,
                    ),
                };
                pending.push(stored);
                let deadline_ms = view.evidence_deadline_ms(&step.node_id);
                coverage.push(CircuitStepObservationCoverage {
                    node_id: step.node_id.clone(),
                    attempt: step.attempt,
                    platform,
                    capabilities,
                    deadline_ms,
                    observation_blocker: view
                        .context
                        .lifecycle_blocker(&step.node_id)
                        .or_else(|| {
                            (step.status == "unverified")
                                .then(|| {
                                    context
                                        .get(&format!("node.{}.observation_blocker", step.node_id))
                                })
                                .flatten()
                        })
                        .and_then(|json| serde_json::from_str(json).ok()),
                    waits_active: run.state == "running"
                        && !matches!(step.status.as_str(), "completed" | "failed" | "cancelled"),
                    human_waits: context
                        .get(&format!("node.{}.evidence.{}", step.node_id, step.attempt))
                        .and_then(|json| {
                            serde_json::from_str::<crate::circuit::observation::WorkEvidence>(json)
                                .ok()
                        })
                        .map(|evidence| {
                            evidence
                                .human_waits
                                .into_iter()
                                .filter(|wait| wait.source != "agent_status_projection")
                                .collect()
                        })
                        .unwrap_or_default(),
                });
            }
            if run.state != "running" || step.status != "unverified" {
                continue;
            }
            let actions = checkpoint_actions(db, run_id, &graph, &view, &step)?;
            let ends_review = view.completion_ends_review(&step.node_id);
            checkpoints.push(CircuitCheckpoint {
                node_id: step.node_id.clone(),
                attempt: step.attempt,
                actions,
                starts_next_review: view.completion_starts_next_review(&step.node_id),
                ends_review,
                review_again: ends_review && continuation,
            });
        }
    }
    Ok((
        CircuitEvidenceView {
            entries,
            checkpoints,
            coverage,
        },
        pending,
    ))
}

/// Everything the run card needs to tell a person what to do next, without the
/// (potentially very long) history: the unverified steps with their allowed
/// actions, how a failed run can be recovered, and the revision to act against.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitRunAttention.ts")]
pub struct CircuitRunAttention {
    #[ts(as = "i32")]
    pub revision: i64,
    pub checkpoints: Vec<CircuitCheckpoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub recovery: Option<super::step_recovery::RunRecovery>,
    /// Running or unverified steps a person can move on by hand, overriding
    /// stale evidence. See [`super::operator_push`].
    pub pushes: Vec<super::operator_push::CircuitStepPush>,
}

pub fn attention(run_id: i64) -> Result<CircuitRunAttention, DbError> {
    let db = crate::db::read_conn()?;
    let tx = db.unchecked_transaction()?;
    let view = attention_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(view)
}

pub(crate) fn attention_inner(
    db: &Connection,
    run_id: i64,
) -> Result<CircuitRunAttention, DbError> {
    let run = super::ledger::get_circuit_run_inner(db, run_id)?
        .ok_or_else(|| DbError::not_found("Run no longer exists."))?;
    let graph = run_graph(db, run_id)?;
    let steps = super::ledger::list_circuit_run_steps_inner(db, run_id)?;
    let mut checkpoints = Vec::new();
    let mut pushes = Vec::new();
    let continuation = super::recovery::review_continuation_supported(&run, &graph);
    if run.state == "running" {
        let view = recovery_view(db, &run)?;
        pushes = super::operator_push::pushes(&view, continuation);
        for step in steps.iter().filter(|step| step.status == "unverified") {
            let ends_review = view.completion_ends_review(&step.node_id);
            checkpoints.push(CircuitCheckpoint {
                node_id: step.node_id.clone(),
                attempt: step.attempt,
                actions: checkpoint_actions(db, run_id, &graph, &view, step)?,
                starts_next_review: view.completion_starts_next_review(&step.node_id),
                ends_review,
                review_again: ends_review && continuation,
            });
        }
    }
    Ok(CircuitRunAttention {
        revision: revision_inner(db, run_id)?,
        checkpoints,
        recovery: super::step_recovery::build_recovery(db, &run, &graph, &steps)?,
        pushes,
    })
}

pub(super) fn history_inner(db: &Connection, run_id: i64) -> SqlResult<Vec<CircuitHistoryEntry>> {
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
) -> Result<(), DbError> {
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
    prompt: &crate::circuit::delivery::PreparedPrompt,
) -> Result<i64, DbError> {
    let db = crate::db::write_conn();
    record_prompt_submission_locked(
        &db,
        run_id,
        node_id,
        attempt,
        prompt.node_id(),
        prompt.text(),
    )
}

pub(crate) fn record_prompt_submission_locked(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    agent_node_id: i64,
    prompt: &str,
) -> Result<i64, DbError> {
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
        HistoryEvent {
            run_id,
            node_id: Some(node_id),
            attempt: Some(attempt),
            kind: "prompt_submitted",
            detail: &serde_json::to_string(&submission)?,
            source: Some(SOURCE_PROMPT_SUBMISSION),
            disposition: Some(DISPOSITION_RECORDED),
        },
    )?;
    // This history entry advances the same revision used by transition fences.
    // Return it under the writer lock so the dispatcher's own write cannot
    // invalidate its subsequent PromptDelivered transition.
    Ok(revision_inner(db, run_id)?)
}

/// The SQL `newest_submission_seq` runs, kept as a constant so the plan test
/// in this module pins the statement production actually executes.
///
/// SQLite matches an expression index against the expression the query uses,
/// so the `json_extract(detail,'$.agent_node_id')` term here has to stay the
/// same expression `idx_circuit_history_submission` indexes. A changed JSON
/// path or a different function stops the match, which puts the full
/// `circuit_run_history` scan back on this hot path (issue #1932).
const NEWEST_SUBMISSION_SEQ_SQL: &str =
    "SELECT COALESCE(MAX(json_extract(detail,'$.submission_seq')),0)
         FROM circuit_run_history
         WHERE kind='prompt_submitted' AND json_extract(detail,'$.agent_node_id')=?1";

fn newest_submission_seq(db: &Connection, agent_node_id: i64) -> SqlResult<i64> {
    db.query_row(NEWEST_SUBMISSION_SEQ_SQL, params![agent_node_id], |row| {
        row.get(0)
    })
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

/// A wait may start after the prompt's start hook already arrived for its
/// upstream delivery step. Inherit only the newest submission for this agent,
/// from a retained ancestor attempt; sibling and retired attempts stay fenced.
fn upstream_submission_owner(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    agent: i64,
) -> SqlResult<Option<(String, i32)>> {
    let newest = newest_submission_seq(db, agent)?;
    let mut query = db.prepare("SELECT h.node_id,h.attempt FROM circuit_run_history h
        JOIN autopilot_circuit_run_steps s ON s.run_id=h.run_id AND s.node_id=h.node_id AND s.attempt=h.attempt
        WHERE h.run_id=?1 AND h.kind='prompt_submitted' AND json_extract(h.detail,'$.agent_node_id')=?2
        AND json_extract(h.detail,'$.submission_seq')=?3 ORDER BY h.id DESC")?;
    let rows = query.query_map(params![run_id, agent, newest], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?))
    })?;
    let graph =
        run_graph(db, run_id).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    for row in rows {
        let (owner, owner_attempt) = row?;
        // Attempts are local to a step. A cycle cannot authorize a different
        // attempt of this same step through the ancestor branch.
        let eligible = if owner == node_id {
            owner_attempt == attempt
        } else {
            graph.is_ancestor(node_id, &owner)
        };
        if eligible {
            return Ok(Some((owner, owner_attempt)));
        }
    }
    Ok(None)
}

/// The binding a previously recorded, correlated turn start established for
/// this receipt's turn token. `Stop` carries the token but not the prompt
/// text, so it can only inherit a binding made when the turn began.
fn recorded_turn_binding(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    receipt: &crate::services::circuit_worker::native_hooks::NativeReceipt,
) -> SqlResult<Option<SubmissionBinding>> {
    let local = recorded_turn_binding_at(db, run_id, node_id, attempt, receipt)?;
    if local.is_some() {
        return Ok(local);
    }
    let Some((owner, owner_attempt)) =
        upstream_submission_owner(db, run_id, node_id, attempt, receipt.agent_node_id)?
    else {
        return Ok(None);
    };
    recorded_turn_binding_at(db, run_id, &owner, owner_attempt, receipt)
}

fn recorded_turn_binding_at(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    receipt: &crate::services::circuit_worker::native_hooks::NativeReceipt,
) -> SqlResult<Option<SubmissionBinding>> {
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
) -> SqlResult<Option<SubmissionBinding>> {
    let hook = &receipt.hook;
    let (Some(turn_id), Some(digest)) = (hook.turn_id.as_deref(), hook.prompt_digest.as_deref())
    else {
        return Ok(None);
    };
    let Some(stamp) = receipt.input_stamp.clone() else {
        return Ok(None);
    };
    let Some((owner, owner_attempt)) =
        upstream_submission_owner(db, run_id, node_id, attempt, receipt.agent_node_id)?
    else {
        return Ok(None);
    };
    let node_id = owner.as_str();
    let attempt = owner_attempt;
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
        .optional()?
        .and_then(|detail| serde_json::from_str(&detail).ok());
    let Some(submission) = submission else {
        return Ok(None);
    };
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
             WHERE run_id=?1 AND kind='native_hook_received'
             AND json_extract(detail,'$.agent_node_id')=?2
             AND json_extract(detail,'$.session_incarnation') IS ?3
             AND json_extract(detail,'$.hook.event')='UserPromptSubmit'
             AND json_extract(detail,'$.submission_correlated')=1
             AND json_extract(detail,'$.submission_seq')=?4",
        )?;
        let rows = stmt.query_map(
            params![
                run_id,
                receipt.agent_node_id,
                receipt.session_incarnation,
                submission.submission_seq
            ],
            |row| row.get::<_, String>(0),
        )?;
        rows.collect::<SqlResult<Vec<_>>>()?
    };
    if !claimed.iter().any(|turn| turn == turn_id) && !claimed.is_empty() {
        return Ok(None);
    }
    Ok(Some((Some(stamp), Some(submission.submission_seq))))
}

pub(crate) fn receive_native_hook_locked(
    db: &mut Connection,
    receipt: &crate::services::circuit_worker::native_hooks::NativeReceipt,
) -> Result<(), DbError> {
    let tx = db.transaction()?;
    let run_ids = {
        let mut stmt = tx.prepare("SELECT id FROM autopilot_circuit_runs r WHERE r.state IN ('running','paused')
            AND (r.source_agent_node_id=?1 OR EXISTS (SELECT 1 FROM autopilot_circuit_run_steps s WHERE s.run_id=r.id AND s.agent_node_id=?1))")?;
        let rows = stmt.query_map([receipt.agent_node_id], |r| r.get::<_, i64>(0))?;
        rows.collect::<SqlResult<Vec<_>>>()?
    };
    let mut targets = Vec::new();
    for run_id in run_ids {
        use crate::circuit::{
            context::CircuitContext,
            model::StepOutcome,
            stepper::{RunState, RunView, StepStatus, StepView},
        };
        let run = super::ledger::get_circuit_run_inner(&tx, run_id)?
            .ok_or_else(|| DbError::not_found("Run no longer exists"))?;
        let rows = super::ledger::list_circuit_run_steps_inner(&tx, run_id)?;
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
        let binding: Option<SubmissionBinding> = if receipt.hook.event == "UserPromptSubmit" {
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
        let detail = serde_json::to_string(&receipt)?;
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM circuit_run_history WHERE run_id=?1 AND node_id=?2 AND attempt=?3
            AND kind='native_hook_received' AND json_extract(detail,'$.source_id')=?4)",
            params![run_id,node_id,attempt,receipt.source_id], |r| r.get(0))?;
        if !exists {
            append_history(
                &tx,
                HistoryEvent {
                    run_id,
                    node_id: Some(&node_id),
                    attempt: Some(attempt),
                    kind: "native_hook_received",
                    detail: &detail,
                    source: Some(SOURCE_NATIVE_HOOK),
                    disposition: Some(DISPOSITION_RECEIVED),
                },
            )?;
        }
    }
    Ok(tx.commit()?)
}

pub(crate) fn native_hook_receipts(run_id: i64, after: i64) -> SqlResult<Vec<CircuitHistoryEntry>> {
    let db = crate::db::read_conn()?;
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
    ManualDelivered,
    Completed,
    NotPerformed,
    Retry,
    Recheck,
    ReviewPass,
    ReviewFail,
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

pub fn record_outcome(request: &CheckpointRequest) -> Result<(), DbError> {
    if matches!(request.action, CheckpointAction::ManualDelivered) {
        return crate::services::circuit_prompt::attest(request);
    }
    let mut db = crate::db::write_conn();
    record_outcome_locked(&mut db, request)
}

pub(super) fn record_outcome_locked(
    db: &mut Connection,
    request: &CheckpointRequest,
) -> Result<(), DbError> {
    // For an acknowledgement the note annotates the history entry when given;
    // every branch below therefore renders a sensible sentence without one. A
    // review verdict is different: it is a person's judgement about someone
    // else's work, so the evidence is part of the verdict and is required.
    let note = request.reason.trim();
    if note.is_empty()
        && matches!(
            request.action,
            CheckpointAction::ReviewPass | CheckpointAction::ReviewFail
        )
    {
        return Err(DbError::invalid(
            "Give the evidence behind your review decision.",
        ));
    }
    let tx = db.transaction()?;
    if revision_moved(&tx, request.run_id, request.expected_revision)? {
        return Err(DbError::conflict(
            "The run changed. Refresh its evidence before acting.",
        ));
    }
    let run = super::ledger::get_circuit_run_inner(&tx, request.run_id)?
        .ok_or_else(|| DbError::not_found("Run no longer exists."))?;
    if run.state != "running" {
        return Err(DbError::invalid(
            "Only an active run can resolve a checkpoint.",
        ));
    }
    let step = super::ledger::list_circuit_run_steps_inner(&tx, request.run_id)?
        .into_iter()
        .find(|s| {
            s.node_id == request.node_id && s.attempt == request.attempt && s.status == "unverified"
        })
        .ok_or_else(|| DbError::conflict("This checkpoint changed. Refresh before acting."))?;
    let graph = run_graph(&tx, run.id)?;
    // Review decisions are explicitly attributed to the operator, never the agent.
    if matches!(
        request.action,
        CheckpointAction::ReviewPass | CheckpointAction::ReviewFail
    ) {
        use crate::circuit::model::StepOutcome;
        let passed = matches!(request.action, CheckpointAction::ReviewPass);
        let outcome = if passed {
            StepOutcome::Completed
        } else {
            StepOutcome::Working
        };
        let mut view = recovery_view(&tx, &run)?;
        if let Err(reason) = view.check_review_decision(&step.node_id, outcome) {
            return Err(DbError::invalid(format!(
                "This review cannot accept that verdict. {reason}"
            )));
        }
        let verdict = if passed {
            "approved"
        } else {
            "changes_requested"
        };
        view.context.set(
            &format!("node.{}.status", step.node_id),
            if passed { "completed" } else { "working" },
        );
        view.context.set(
            &format!("node.{}.operator_review_note", step.node_id),
            request.reason.trim(),
        );
        // The old evaluated report may belong to an earlier attempt. Keep its
        // immutable history entry; this decision's displayed evidence is the note.
        view.context.set(
            &format!("node.{}.evaluated_output", step.node_id),
            request.reason.trim(),
        );
        view.context
            .set(&format!("node.{}.review_verdict", step.node_id), verdict);
        view.context.set(
            &format!("node.{}.review_verdict_attempt", step.node_id),
            step.attempt.to_string(),
        );
        view.context.set(
            &format!("node.{}.review_verdict_source", step.node_id),
            "operator",
        );
        view.context
            .set(&format!("node.{}.wait.attempt", step.node_id), "");
        let commit = super::ledger::commit_circuit_advance_inner(
            &tx,
            run.id,
            None,
            Some(&view.context.to_json()?),
            &[super::CircuitStepOp {
                node_id: step.node_id.clone(),
                status: "completed".into(),
                attempt: step.attempt,
                outcome: Some(Some(if passed { "completed" } else { "working" }.into())),
                error: Some(None),
                agent_node_id: None,
                fresh_attempt: false,
            }],
        )?;
        if !commit.applied {
            return Err(DbError::conflict(
                "The run changed before the verdict was recorded.",
            ));
        }
        append_history(
            &tx,
            HistoryEvent {
                run_id: run.id,
                node_id: Some(&step.node_id),
                attempt: Some(step.attempt),
                kind: "operator_review",
                detail: &format!("Human review {verdict}: {}", request.reason.trim()),
                source: Some(SOURCE_OPERATOR),
                disposition: Some(verdict),
            },
        )?;
        return Ok(tx.commit()?);
    }
    // A completion attestation cannot authorize tool use or supply a review verdict.
    use crate::circuit::model::CircuitNodeKind;
    if matches!(request.action, CheckpointAction::Completed)
        && matches!(
            graph.node(&request.node_id).map(|n| &n.kind),
            Some(
                CircuitNodeKind::SpawnAgentNode { .. }
                    | CircuitNodeKind::AwaitAgentTurn { .. }
                    | CircuitNodeKind::LlmTurnClassifier { .. }
            )
        )
        && (step.agent_node_id.is_some()
            || !matches!(
                graph.node(&request.node_id).map(|n| &n.kind),
                Some(CircuitNodeKind::SpawnAgentNode { .. })
            ))
    {
        let mut view = recovery_view(&tx, &run)?;
        if !view.can_attest_completion(&request.node_id) {
            return Err(DbError::invalid("Resolve outstanding work, input requests and conflicting evidence before recording completion."));
        }
        let reason = if note.is_empty() {
            "Operator recorded this checkpoint as completed.".to_string()
        } else {
            format!("Operator-recorded completion: {note}")
        };
        view.context
            .set(&format!("node.{}.status", step.node_id), "completed");
        view.context
            .set(&format!("node.{}.wait.attempt", step.node_id), "");
        view.context
            .set(&format!("node.{}.observation_blocker", step.node_id), "");
        // Leave native evidence untouched. The next worker tick schedules
        // successors, retaining their independent approval and report gates.
        super::ledger::commit_circuit_advance_inner(
            &tx,
            run.id,
            None,
            Some(&view.context.to_json()?),
            &[super::CircuitStepOp {
                node_id: step.node_id.clone(),
                status: "completed".into(),
                attempt: step.attempt,
                outcome: Some(Some("completed".into())),
                error: Some(None),
                agent_node_id: None,
                fresh_attempt: false,
            }],
        )?;
        append_history(
            &tx,
            HistoryEvent {
                run_id: run.id,
                node_id: Some(&step.node_id),
                attempt: Some(step.attempt),
                kind: "operator_attestation",
                detail: &reason,
                source: Some(SOURCE_OPERATOR),
                disposition: Some("completed"),
            },
        )?;
        return Ok(tx.commit()?);
    }
    if matches!(request.action, CheckpointAction::Recheck) {
        let kind = graph.node(&request.node_id).map(|n| &n.kind);
        let open_pr = matches!(
            kind,
            Some(CircuitNodeKind::GithubAction {
                action: crate::circuit::model::GithubActionKind::OpenPr,
                ..
            })
        );
        let has_open_pr_target = if open_pr {
            has_effect_target(&tx, request.run_id, &request.node_id, request.attempt)?
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
            return Err(DbError::invalid("This action has no authoritative automatic evidence recheck. Inspect the external result."));
        }
        let mut context = crate::circuit::context::CircuitContext::from_json(&run.context_json)?;
        context.set(&format!("node.{}.wait.attempt", step.node_id), "");
        context.set(&format!("node.{}.evaluated_attempt", step.node_id), "");
        context.set(
            &format!("node.{}.classifier_error.{}", step.node_id, step.attempt),
            "",
        );
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
        )?;
        if !commit.applied {
            return Err(DbError::conflict(
                "Circuit run became terminal before evidence recheck commit",
            ));
        }
        append_history(
            &tx,
            HistoryEvent {
                run_id: request.run_id,
                node_id: Some(&request.node_id),
                attempt: Some(request.attempt),
                kind: "evidence_recheck",
                detail: if note.is_empty() {
                    "Operator requested an evidence recheck."
                } else {
                    note
                },
                source: Some(SOURCE_OPERATOR),
                disposition: Some(DISPOSITION_REQUESTED),
            },
        )?;
        return Ok(tx.commit()?);
    }
    let mut context = crate::circuit::context::CircuitContext::from_json(&run.context_json)?;
    context.set(&format!("node.{}.recheck_only", request.node_id), "0");
    let effect_kind = match graph.node(&request.node_id).map(|n| &n.kind) {
        Some(CircuitNodeKind::GithubAction { .. }) => "github",
        Some(CircuitNodeKind::InjectPty { .. }) => "prompt",
        Some(CircuitNodeKind::SpawnAgentNode { .. }) if step.agent_node_id.is_none() => "spawn",
        _ => return Err(DbError::invalid("This checkpoint requires authoritative agent evidence; an external-action attestation cannot resolve it.")),
    };
    if effect_kind == "spawn" && matches!(request.action, CheckpointAction::Completed) {
        return Err(DbError::invalid("Agent identity must be reconciled; attestation cannot establish a spawn attachment or completion."));
    }
    if matches!(request.action, CheckpointAction::Completed)
        && matches!(
            graph.node(&request.node_id).map(|n| &n.kind),
            Some(CircuitNodeKind::GithubAction {
                action: crate::circuit::model::GithubActionKind::OpenPr,
                ..
            })
        )
    {
        return Err(DbError::invalid(
            "The pull request identity must be reconciled before this step can advance.",
        ));
    }
    // This step was already admitted by the graph. Requiring every ancestor
    // verdict to be approved deadlocks the changes-requested feedback route
    // and alternative approval branches. Current human requests still block.
    let recovery = recovery_view(&tx, &run)?;
    if recovery.report_has_known_blockers(&request.node_id) {
        return Err(DbError::invalid("Resolve outstanding work, input requests and conflicting evidence before recording an outcome."));
    }
    let prior: Option<String> = tx.query_row("SELECT state FROM circuit_effects WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind=?4",
        params![request.run_id,request.node_id,request.attempt,effect_kind], |r| r.get(0)).optional()?;
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
                )?;
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
                )?;
                ("unverified", step.attempt, None)
            }
            CheckpointAction::Retry if prior.as_deref() == Some("not_performed") => {
                ("pending_slot", step.attempt + 1, Some(None))
            }
            CheckpointAction::Retry => return Err(DbError::invalid(
                "The earlier action must be recorded as not performed before a deliberate retry.",
            )),
            CheckpointAction::Recheck
            | CheckpointAction::ManualDelivered
            | CheckpointAction::ReviewPass
            | CheckpointAction::ReviewFail => unreachable!("handled above"),
        };
    let reason = if note.is_empty() {
        format!("Operator-recorded outcome ({:?}).", request.action)
    } else {
        format!("Operator-recorded outcome ({:?}): {note}", request.action)
    };
    // The attestation's disposition is the action the operator recorded.
    let disposition = match request.action {
        CheckpointAction::Completed => "completed",
        CheckpointAction::NotPerformed => "not_performed",
        CheckpointAction::Retry => "retry",
        CheckpointAction::Recheck => "recheck",
        CheckpointAction::ReviewPass | CheckpointAction::ReviewFail => {
            unreachable!("handled above")
        }
        CheckpointAction::ManualDelivered => unreachable!("handled before taking the DB lock"),
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
    )?;
    if !commit.applied {
        return Err(DbError::conflict(
            "Circuit run became terminal before operator outcome commit",
        ));
    }
    append_history(
        &tx,
        HistoryEvent {
            run_id: request.run_id,
            node_id: Some(&request.node_id),
            attempt: Some(request.attempt),
            kind: "operator_attestation",
            detail: &reason,
            source: Some(SOURCE_OPERATOR),
            disposition: Some(disposition),
        },
    )?;
    Ok(tx.commit()?)
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

impl From<crate::circuit::stepper::EffectJournal> for EffectKind {
    fn from(kind: crate::circuit::stepper::EffectJournal) -> Self {
        match kind {
            crate::circuit::stepper::EffectJournal::Spawn => Self::Spawn,
            crate::circuit::stepper::EffectJournal::Prompt => Self::Prompt,
            crate::circuit::stepper::EffectJournal::Github => Self::Github,
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
    MeshCapacity {
        capacity: i64,
    },
    AgentCapacity {
        required: i64,
        available: Option<i64>,
    },
    ReservationUnavailable,
}

pub(crate) fn queue_waits(mesh_id: i64) -> SqlResult<std::collections::HashMap<String, String>> {
    let db = crate::db::read_conn()?;
    queue_waits_inner(&db, mesh_id)
}

fn queue_waits_inner(
    db: &Connection,
    mesh_id: i64,
) -> SqlResult<std::collections::HashMap<String, String>> {
    let mut statement = db.prepare(
        "SELECT CAST(r.id AS TEXT), h.detail
        FROM autopilot_circuit_runs r JOIN circuit_run_history h ON h.id =
        (SELECT MAX(id) FROM circuit_run_history WHERE run_id=r.id AND kind='queue_wait')
        WHERE r.mesh_id=?1 AND r.state='pending'",
    )?;
    let rows = statement.query_map([mesh_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

pub(crate) fn record_queue_wait(run_id: i64, reason: QueueWaitReason) -> SqlResult<()> {
    record_queue_wait_locked(&mut crate::db::write_conn(), run_id, reason)
}

fn record_queue_wait_locked(
    db: &mut Connection,
    run_id: i64,
    reason: QueueWaitReason,
) -> SqlResult<()> {
    let tx = db.transaction()?;
    let pending: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM autopilot_circuit_runs WHERE id=?1 AND state='pending')",
        [run_id],
        |row| row.get(0),
    )?;
    if !pending {
        return Ok(());
    }
    let detail = serde_json::to_string(&reason).expect("queue wait fields");
    let previous: Option<String> = tx.query_row("SELECT detail FROM circuit_run_history WHERE run_id=?1 AND kind='queue_wait' ORDER BY id DESC LIMIT 1", [run_id], |row|row.get(0)).optional()?;
    if previous.as_deref() != Some(&detail) {
        append_history(
            &tx,
            HistoryEvent {
                run_id,
                node_id: None,
                attempt: None,
                kind: "queue_wait",
                detail: &detail,
                source: Some(SOURCE_ADMISSION),
                disposition: Some(DISPOSITION_WAITING),
            },
        )?;
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
    if inserted == 0 {
        return Ok(());
    }
    let (graph, context): (String, String) = db.query_row("SELECT s.graph_json,r.context_json FROM circuit_run_snapshots s JOIN autopilot_circuit_runs r ON r.id=s.run_id WHERE r.id=?1", [run_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let context: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&context).unwrap_or_default();
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
    append_history(
        db,
        HistoryEvent {
            run_id,
            node_id: None,
            attempt: None,
            kind: "configuration_pinned",
            detail: &serde_json::json!({
                "behavior_revision":1,"graph_sha256":hex::encode(Sha256::digest(graph.as_bytes())),
                "reviewers":reviewers
            })
            .to_string(),
            source: Some(SOURCE_RUN_CONFIGURATION),
            disposition: Some(DISPOSITION_APPLIED),
        },
    )?;
    Ok(())
}

/// Record changes to the persisted evidence window alongside its projection.
pub(super) fn record_wait_changes(
    db: &Connection,
    run_id: i64,
    next_context: &str,
) -> SqlResult<()> {
    let previous: String = db.query_row(
        "SELECT context_json FROM autopilot_circuit_runs WHERE id=?1",
        [run_id],
        |row| row.get(0),
    )?;
    let previous: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&previous).unwrap_or_default();
    let next: std::collections::BTreeMap<String, String> =
        serde_json::from_str(next_context).unwrap_or_default();
    let continuation_keys: std::collections::BTreeSet<&String> = previous
        .keys()
        .chain(next.keys())
        .filter(|key| key.starts_with("node.") && key.ends_with(".continuation.delivery"))
        .collect();
    for key in continuation_keys {
        let node = key
            .strip_prefix("node.")
            .and_then(|key| key.strip_suffix(".continuation.delivery"))
            .expect("filtered continuation key");
        let attempt = next
            .get(&format!("node.{node}.continuation.attempt"))
            .and_then(|value| value.parse::<i32>().ok());
        let count_key = format!("node.{node}.continuations.{}", attempt.unwrap_or_default());
        if previous.get(key) != next.get(key) || previous.get(&count_key) != next.get(&count_key) {
            let state = match next.get(key).map(String::as_str) {
                Some("pending") => "intent",
                Some("claimed") => "possible_dispatch",
                Some("delivered") => "acknowledged",
                Some("obsolete") => "not_performed",
                Some("uncertain") => "uncertain",
                _ => continue,
            };
            if state == "possible_dispatch"
                && previous.get(key).map(String::as_str) != Some("pending")
            {
                append_history(
                    db,
                    HistoryEvent {
                        run_id,
                        node_id: Some(node),
                        attempt,
                        kind: "continuation_effect",
                        detail: &serde_json::json!({
                            "effect":"continuation_prompt","state":"intent","ordinal":next.get(&count_key)
                        }).to_string(),
                        source: Some(SOURCE_CIRCUIT_WORKER),
                        disposition: Some(DISPOSITION_INTENT),
                    },
                )?;
            }
            append_history(
                db,
                HistoryEvent {
                    run_id,
                    node_id: Some(node),
                    attempt,
                    kind: "continuation_effect",
                    detail: &serde_json::json!({
                        "effect":"continuation_prompt","state":state,"ordinal":next.get(&count_key)
                    })
                    .to_string(),
                    source: Some(SOURCE_CIRCUIT_WORKER),
                    disposition: Some(state),
                },
            )?;
        }
    }
    let blocker_keys: std::collections::BTreeSet<&String> = previous
        .keys()
        .chain(next.keys())
        .filter(|key| key.starts_with("node.") && key.ends_with(".observation_blocker"))
        .collect();
    for key in blocker_keys {
        if previous.get(key) == next.get(key) {
            continue;
        }
        let node = key
            .strip_prefix("node.")
            .and_then(|key| key.strip_suffix(".observation_blocker"));
        let Some(node) = node else {
            continue;
        };
        let attempt = db
            .query_row(
                "SELECT attempt FROM autopilot_circuit_run_steps WHERE run_id=?1 AND node_id=?2",
                params![run_id, node],
                |row| row.get::<_, i32>(0),
            )
            .optional()?;
        let blocker = next.get(key).and_then(|value| {
            serde_json::from_str::<crate::circuit::observation::CircuitObservationBlocker>(value)
                .ok()
        });
        let detail = serde_json::json!({"blocker": blocker, "message": blocker.as_ref().map(|blocker| blocker.message())});
        append_history(
            db,
            HistoryEvent {
                run_id,
                node_id: Some(node),
                attempt,
                kind: "observation_readiness",
                detail: &detail.to_string(),
                source: Some(SOURCE_RECONCILIATION),
                disposition: Some(wait_disposition(blocker.is_some())),
            },
        )?;
    }
    let capacity_keys: std::collections::BTreeSet<&String> = previous
        .keys()
        .chain(next.keys())
        .filter(|key| key.starts_with("node.") && key.ends_with(".capacity_wait"))
        .collect();
    for key in capacity_keys {
        if previous.get(key) != next.get(key) {
            let node = key
                .strip_prefix("node.")
                .and_then(|key| key.strip_suffix(".capacity_wait"));
            // Identity: the parked step's current attempt, so the operator
            // surface can name which execution is waiting (issue #1909).
            let attempt = match node {
                Some(node) => db.query_row(
                    "SELECT attempt FROM autopilot_circuit_run_steps WHERE run_id=?1 AND node_id=?2",
                    params![run_id, node], |r| r.get::<_, i32>(0),
                ).optional()?,
                None => None,
            };
            // A freed window (`agent_limit` false, or the key gone) is a
            // resolution, not an active wait (issue #1909). Rows written
            // before ADR 0042 also carried a `circuit_limit` flag; it is
            // never read here because nothing sets it any more.
            let window = next
                .get(key)
                .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok());
            let binding = window
                .as_ref()
                .is_some_and(|value| value["agent_limit"].as_bool().unwrap_or(false));
            append_history(
                db,
                HistoryEvent {
                    run_id,
                    node_id: node,
                    attempt,
                    kind: "step_capacity_wait",
                    detail: &serde_json::json!({"before":previous.get(key),"after":next.get(key)})
                        .to_string(),
                    source: Some(SOURCE_CAPACITY),
                    disposition: Some(wait_disposition(binding)),
                },
            )?;
        }
    }
    let nodes: std::collections::BTreeSet<&str> = previous
        .keys()
        .chain(next.keys())
        .filter_map(|key| key.strip_prefix("node.")?.strip_suffix(".wait.attempt"))
        .collect();
    for node in nodes {
        let prefix = format!("node.{node}.wait.");
        let fields = [
            "attempt",
            "timeout_ms",
            "since_ms",
            "observed",
            "explicit_budget",
        ];
        let project = |context: &std::collections::BTreeMap<String,String>| -> std::collections::BTreeMap<&str,Option<String>> {
            fields.iter().map(|field|(*field,context.get(&format!("{prefix}{field}")).cloned())).collect()
        };
        let before = project(&previous);
        let after = project(&next);
        if before != after {
            // Identity survives a resolution: a cleared window writes an empty
            // attempt, so fall back to the prior window's attempt rather than
            // dropping the identity the wait was parked on (issue #1909 review).
            let attempt = next
                .get(&format!("{prefix}attempt"))
                .and_then(|value| value.parse::<i32>().ok())
                .or_else(|| {
                    previous
                        .get(&format!("{prefix}attempt"))
                        .and_then(|value| value.parse::<i32>().ok())
                });
            // A window whose (possibly cleared) attempt is empty is a
            // resolution, not an active wait.
            let binding = after
                .get("attempt")
                .and_then(|value| value.as_deref())
                .is_some_and(|value| !value.is_empty());
            append_history(
                db,
                HistoryEvent {
                    run_id,
                    node_id: Some(node),
                    attempt,
                    kind: "evidence_window_changed",
                    detail: &serde_json::json!({"before":before,"after":after}).to_string(),
                    source: Some(SOURCE_RECONCILIATION),
                    disposition: Some(wait_disposition(binding)),
                },
            )?;
        }
    }
    Ok(())
}

pub(super) fn run_graph(
    db: &Connection,
    run_id: i64,
) -> Result<crate::circuit::model::CircuitGraph, DbError> {
    let json = run_graph_json(db, run_id)?;
    Ok(crate::circuit::model::CircuitGraph::from_json(&json)?)
}

pub(super) fn run_graph_json(db: &Connection, run_id: i64) -> SqlResult<String> {
    db.query_row(
        "SELECT COALESCE(s.graph_json,c.graph_json)
        FROM autopilot_circuit_runs r JOIN autopilot_circuits c ON c.id=r.circuit_id
        LEFT JOIN circuit_run_snapshots s ON s.run_id=r.id WHERE r.id=?1",
        [run_id],
        |r| r.get(0),
    )
}

/// One Circuit Run History event: the single row [`append_history`] writes.
///
/// `source` and `disposition` are the provenance vocabulary above; both are
/// nullable for pre-v45 rows (issue #1909 / #1847). `observed_at` defaults to
/// the append time, which for these synchronous events is the observed time.
///
/// A history event is a named thing rather than a tuple: every call site
/// spells out its provenance fields instead of trusting a positional order,
/// so a field added later cannot be transposed into the wrong column.
pub(super) struct HistoryEvent<'a> {
    pub(super) run_id: i64,
    pub(super) node_id: Option<&'a str>,
    pub(super) attempt: Option<i32>,
    pub(super) kind: &'a str,
    pub(super) detail: &'a str,
    pub(super) source: Option<&'a str>,
    pub(super) disposition: Option<&'a str>,
}

/// Append one Circuit Run History event.
pub(super) fn append_history(db: &Connection, event: HistoryEvent<'_>) -> SqlResult<()> {
    let HistoryEvent {
        run_id,
        node_id,
        attempt,
        kind,
        detail,
        source,
        disposition,
    } = event;
    db.execute("INSERT INTO circuit_run_history (run_id,node_id,attempt,kind,detail,source,disposition) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![run_id, node_id, attempt, kind, crate::secret_scrubber::SecretScrubber::scrub(detail), source, disposition])?;
    Ok(())
}

pub(super) fn revision_inner(db: &Connection, run_id: i64) -> SqlResult<i64> {
    db.query_row(
        "SELECT COALESCE(MAX(id),0) FROM circuit_run_history WHERE run_id=?1",
        [run_id],
        |r| r.get(0),
    )
}

/// Whether history since `expected` contains a fact the person has not seen.
///
/// Duplicate observations are the same fact recorded again. They must not
/// refuse an operator action the person already refreshed for. Any other row
/// still refuses, including a new observation and a row whose disposition was
/// never set. A revision ahead of the ledger is treated as moved.
pub(super) fn revision_moved(db: &Connection, run_id: i64, expected: i64) -> SqlResult<bool> {
    let current = revision_inner(db, run_id)?;
    if current == expected {
        return Ok(false);
    }
    if expected > current {
        return Ok(true);
    }
    // `IS`, not `=`. A missing disposition is unknown under `=`, so `NOT`
    // would drop that row and a stale click would succeed.
    db.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM circuit_run_history
            WHERE run_id=?1 AND id>?2
              AND NOT (kind='observation' AND disposition IS ?3)
        )",
        params![
            run_id,
            expected,
            observation_disposition_str(
                crate::circuit::observation::ObservationDisposition::Duplicate
            )
        ],
        |row| row.get(0),
    )
}

pub fn observation_revision(run: &crate::models::AutopilotCircuitRun) -> SqlResult<i64> {
    let db = crate::db::read_conn()?;
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
    if evidence
        .input_guard
        .and_then(|guard| guard.transcript_guard.as_ref())
        .is_some_and(|snapshot| !snapshot.is_current())
    {
        return Err(observation_freshness_rejection(
            "Native transcript changed before evidence commit; recheck required",
        ));
    }
    if evidence
        .input_guard
        .and_then(|guard| guard.report_guard.as_ref())
        .is_some_and(|snapshot| !snapshot.is_current())
    {
        return Err(observation_freshness_rejection(
            "Agent report changed before evidence commit; recheck required",
        ));
    }
    let mut db = crate::db::write_conn();
    let result = if let Some(guard) = evidence.input_guard {
        let mut revision = None;
        let mut commit_error = None;
        let accepted = crate::agent::process::PROCESS_REGISTRY.commit_recovered_turn(
            guard.agent_node_id,
            &guard.input_stamp,
            guard.observed_at_ms,
            || match commit_transition_locked(&mut db, run_id, state, context, steps, evidence) {
                Ok(committed_revision) => {
                    revision = Some(committed_revision);
                    Ok(true)
                }
                Err(error) => {
                    let message = error.to_string();
                    commit_error = Some(error);
                    Err(message)
                }
            },
        );
        if let Some(error) = commit_error {
            Err(error)
        } else if !accepted.map_err(rusqlite::Error::InvalidParameterName)? {
            Err(observation_freshness_rejection(
                "agent input or session changed before evidence commit",
            ))
        } else {
            revision.ok_or(rusqlite::Error::InvalidQuery)
        }
    } else {
        commit_transition_locked(&mut db, run_id, state, context, steps, evidence)
    };
    drop(db);
    if result.is_ok() && state.is_some_and(crate::circuit::vocabulary::RunState::is_terminal_db_str)
    {
        crate::services::circuit_worker::wake_circuit_worker();
    }
    result
}

#[derive(Default)]
pub struct EvidenceWrite<'a> {
    pub input_guard: Option<&'a crate::circuit::stepper::ObservationInputFence>,
    pub intents: &'a [EffectIntent],
    pub agent_status_effects: &'a [AgentStatusEffect],
    pub reconciled_effects: &'a [ReconciledEffect],
    pub observations: &'a [crate::circuit::observation::RecordedObservation],
    pub classifications: &'a [crate::circuit::observation::RecordedClassification],
    pub expected: Option<&'a crate::circuit::stepper::TransitionFence>,
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
        // Requests and background work can arrive while interpretation runs.
        // Recheck under the writer transaction. Receipt-only writes remain
        // allowed so child evidence can be reconciled; only a classification
        // can hand off a report, and its lifecycle veto is checked here.
        if !evidence.classifications.is_empty() {
            if let Some(blocker) =
                crate::db::agent_node::circuit_lifecycle_blocker_inner(&tx, guard.agent_node_id)?
            {
                return Err(observation_freshness_rejection(&blocker.message()));
            }
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
    let commit =
        super::ledger::commit_circuit_advance_inner(&tx, run_id, state, Some(context), steps)?;
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
        let configured_status = match graph
            .as_ref()
            .and_then(|graph| graph.node(&effect.node_id))
            .map(|node| &node.kind)
        {
            Some(crate::circuit::model::CircuitNodeKind::SetNodeStatus { status, .. }) => {
                match status {
                    crate::circuit::model::SessionStatusKind::Running => {
                        crate::models::SessionStatus::Running
                    }
                    crate::circuit::model::SessionStatusKind::Idle => {
                        crate::models::SessionStatus::Idle
                    }
                    crate::circuit::model::SessionStatusKind::Completed => {
                        crate::models::SessionStatus::Completed
                    }
                }
            }
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
            HistoryEvent {
                run_id,
                node_id: Some(&effect.node_id),
                attempt: Some(effect.attempt),
                kind: "effect_result",
                detail: &serde_json::json!({
                    "effect": "set_node_status",
                    "state": "acknowledged",
                    "agent_node_id": effect.agent_node_id,
                    "status": effect.status.to_db_str(),
                    "meaning": "Agent status and Circuit step committed atomically"
                })
                .to_string(),
                source: Some(SOURCE_CIRCUIT_WORKER),
                disposition: Some(DISPOSITION_ACKNOWLEDGED),
            },
        )?;
    }
    for observation in evidence.observations {
        if observation.disposition == crate::circuit::observation::ObservationDisposition::Duplicate
        {
            continue;
        }
        let identity = &observation.observation.identity;
        let detail = serde_json::to_string(observation)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        append_history(
            &tx,
            HistoryEvent {
                run_id,
                node_id: Some(&identity.step_id),
                attempt: Some(identity.attempt),
                kind: "observation",
                detail: &detail,
                source: Some(observation.observation.source.as_str()),
                disposition: Some(observation_disposition_str(observation.disposition)),
            },
        )?;
    }
    for classification in evidence.classifications {
        let detail = serde_json::to_string(classification)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        append_history(
            &tx,
            HistoryEvent {
                run_id,
                node_id: Some(&classification.step_id),
                attempt: Some(classification.attempt),
                kind: "classification",
                detail: &detail,
                source: Some(SOURCE_CLASSIFIER),
                disposition: Some(DISPOSITION_INTERPRETED),
            },
        )?;
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
            Some(crate::circuit::model::CircuitNodeKind::GithubAction {
                action: crate::circuit::model::GithubActionKind::OpenPr,
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
                HistoryEvent {
                    run_id,
                    node_id: Some(&effect.intent.node_id),
                    attempt: Some(effect.intent.attempt),
                    kind: "effect_reconciled",
                    detail: &effect.detail,
                    source: Some(SOURCE_GITHUB),
                    disposition: Some(DISPOSITION_RECONCILED),
                },
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
                HistoryEvent {
                    run_id,
                    node_id: Some(&intent.node_id),
                    attempt: Some(intent.attempt),
                    kind: "effect_intent",
                    detail: intent.kind.as_db_str(),
                    source: Some(SOURCE_CIRCUIT_WORKER),
                    disposition: Some(DISPOSITION_INTENT),
                },
            )?;
        }
    }
    let revision = revision_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(revision)
}

/// Must commit before any bytes are sent. A repeated claim never dispatches.
pub(crate) fn acknowledge_spawn_attachment(
    run_id: i64,
    node_id: &str,
    attempt: i32,
    agent_node_id: i64,
    parent_agent_node_id: Option<i64>,
    expected_agent_node_id: Option<i64>,
) -> SqlResult<Option<i64>> {
    let mut db = crate::db::write_conn();
    acknowledge_spawn_attachment_locked(
        &mut db,
        run_id,
        node_id,
        attempt,
        agent_node_id,
        parent_agent_node_id,
        expected_agent_node_id,
    )
}

fn acknowledge_spawn_attachment_locked(
    db: &mut Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    agent_node_id: i64,
    parent_agent_node_id: Option<i64>,
    expected_agent_node_id: Option<i64>,
) -> SqlResult<Option<i64>> {
    let tx = db.transaction()?;
    let updated = tx.execute("UPDATE autopilot_circuit_run_steps SET agent_node_id=?4,parent_agent_node_id=?5
        WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND status='running'
        AND EXISTS(SELECT 1 FROM circuit_effects e WHERE e.run_id=?1 AND e.node_id=?2 AND e.attempt=?3 AND e.kind='spawn'
            AND ((e.state='possible_dispatch' AND agent_node_id IS ?6 AND (agent_node_id IS NULL OR agent_node_id=?4 OR ?3>1))
                OR (e.state='acknowledged' AND agent_node_id=?4)))
        AND EXISTS(SELECT 1 FROM autopilot_circuit_runs WHERE id=?1 AND state='running')",
        params![run_id,node_id,attempt,agent_node_id,parent_agent_node_id,expected_agent_node_id])?;
    if updated == 0 {
        return Ok(None);
    }
    let acknowledged = tx.execute("INSERT INTO circuit_effects(run_id,node_id,attempt,kind,state) VALUES (?1,?2,?3,'spawn','acknowledged')
        ON CONFLICT(run_id,node_id,attempt,kind) DO UPDATE SET state='acknowledged' WHERE state='possible_dispatch'",
        params![run_id,node_id,attempt])?;
    if acknowledged > 0 {
        append_history(
            &tx,
            HistoryEvent {
                run_id,
                node_id: Some(node_id),
                attempt: Some(attempt),
                kind: "effect_result",
                detail: &serde_json::json!({"effect":"spawn","state":"acknowledged","agent_node_id":agent_node_id,
                    "meaning":if expected_agent_node_id == Some(agent_node_id) { "Prompt delivered to retained agent; completion still requires evidence" }
                        else { "Agent allocation attached; foreground and owned work completion still require evidence" }}).to_string(),
                source: Some(SOURCE_CIRCUIT_WORKER),
                disposition: Some(DISPOSITION_ACKNOWLEDGED),
            },
        )?;
    }
    let revision = revision_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(Some(revision))
}

pub fn claim_effect(run_id: i64, intent: &EffectIntent) -> SqlResult<Option<i64>> {
    let mut db = crate::db::write_conn();
    claim_effect_locked(&mut db, run_id, intent)
}

pub(crate) fn acknowledge_prompt_delivery(
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> SqlResult<Option<i64>> {
    let mut db = crate::db::write_conn();
    acknowledge_prompt_delivery_locked(&mut db, run_id, node_id, attempt)
}

fn acknowledge_prompt_delivery_locked(
    db: &mut Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> SqlResult<Option<i64>> {
    let tx = db.transaction()?;
    let updated = tx.execute("UPDATE circuit_effects SET state='acknowledged'
        WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='prompt' AND state='possible_dispatch'
        AND EXISTS(SELECT 1 FROM autopilot_circuit_runs r JOIN autopilot_circuit_run_steps s ON s.run_id=r.id
            WHERE r.id=?1 AND r.state IN ('running','paused') AND s.node_id=?2 AND s.attempt=?3 AND s.status='running')",
        params![run_id, node_id, attempt])?;
    if updated == 0 {
        return Ok(None);
    }
    append_history(
        &tx,
        HistoryEvent {
            run_id,
            node_id: Some(node_id),
            attempt: Some(attempt),
            kind: "effect_result",
            detail: "acknowledged",
            source: Some(SOURCE_CIRCUIT_WORKER),
            disposition: Some(DISPOSITION_ACKNOWLEDGED),
        },
    )?;
    let revision = revision_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(Some(revision))
}

pub(crate) fn prompt_delivery_acknowledged(
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> SqlResult<bool> {
    let db = crate::db::read_conn()?;
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM circuit_effects
        WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='prompt' AND state='acknowledged')",
        params![run_id, node_id, attempt],
        |row| row.get(0),
    )
}

pub(crate) fn record_effect_target(
    run_id: i64,
    node_id: &str,
    attempt: i32,
    detail: &str,
) -> Result<i64, DbError> {
    let mut db = crate::db::write_conn();
    record_effect_target_locked(&mut db, run_id, node_id, attempt, detail)
}

fn record_effect_target_locked(
    db: &mut Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    detail: &str,
) -> Result<i64, DbError> {
    let tx = db.transaction()?;
    let graph = run_graph(&tx, run_id)?;
    if !matches!(
        graph.node(node_id).map(|node| &node.kind),
        Some(crate::circuit::model::CircuitNodeKind::GithubAction {
            action: crate::circuit::model::GithubActionKind::OpenPr,
            ..
        })
    ) {
        return Err(DbError::invalid(
            "Effect target is only supported for an OpenPr step.",
        ));
    }
    let dispatch_claimed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM circuit_effects e
             JOIN autopilot_circuit_runs r ON r.id=e.run_id
             JOIN autopilot_circuit_run_steps s ON s.run_id=e.run_id AND s.node_id=e.node_id
             WHERE e.run_id=?1 AND e.node_id=?2 AND e.attempt=?3 AND e.kind='github'
               AND e.state='possible_dispatch' AND r.state='running'
               AND s.attempt=?3 AND s.status='running')",
        params![run_id, node_id, attempt],
        |row| row.get(0),
    )?;
    if !dispatch_claimed {
        return Err(DbError::conflict(
            "OpenPr dispatch claim changed before its target was recorded.",
        ));
    }
    append_history(
        &tx,
        HistoryEvent {
            run_id,
            node_id: Some(node_id),
            attempt: Some(attempt),
            kind: "effect_target",
            detail,
            source: Some(SOURCE_GITHUB),
            disposition: Some(DISPOSITION_RECORDED),
        },
    )?;
    let revision = revision_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(revision)
}

pub(crate) fn latest_effect_target(
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> SqlResult<Option<String>> {
    let db = crate::db::read_conn()?;
    latest_effect_target_inner(&db, run_id, node_id, attempt)
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

fn has_effect_target(db: &Connection, run_id: i64, node_id: &str, attempt: i32) -> SqlResult<bool> {
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
            HistoryEvent {
                run_id,
                node_id: Some(&intent.node_id),
                attempt: Some(intent.attempt),
                kind: "effect_possible_dispatch",
                detail: intent.kind.as_db_str(),
                source: Some(SOURCE_CIRCUIT_WORKER),
                disposition: Some(DISPOSITION_POSSIBLE_DISPATCH),
            },
        )?;
    }
    let revision = revision_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(claimed.then_some(revision))
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod tests;
