//! Assemble Circuit evidence views and resolve pending capabilities after releasing DB reads.

use super::*;
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
pub(super) type PendingCapabilities = Vec<Option<crate::circuit::strategy::HarnessSelector>>;

pub(super) fn resolve_pending_capabilities(
    view: &mut CircuitEvidenceView,
    pending: PendingCapabilities,
) {
    for (row, stored) in view.coverage.iter_mut().zip(pending) {
        if let Some(selector) = stored {
            row.capabilities =
                crate::services::circuit_worker::observer_policy::for_selector(&selector);
        }
    }
}

pub(in crate::db::circuit) fn recovery_view(
    db: &Connection,
    run: &crate::models::AutopilotCircuitRun,
) -> Result<crate::circuit::stepper::RunView, DbError> {
    use crate::circuit::{
        context::CircuitContext,
        model::StepOutcome,
        stepper::{RunState, RunView, StepStatus, StepView},
    };
    let steps = crate::db::circuit::ledger::list_circuit_run_steps_inner(db, run.id)?;
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
pub(super) fn checkpoint_actions(
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
pub(super) fn evidence_view_unresolved(
    db: &Connection,
    run_id: i64,
) -> Result<(CircuitEvidenceView, PendingCapabilities), DbError> {
    let mut pending = PendingCapabilities::new();
    let entries = history_inner(db, run_id)?;
    let run = crate::db::circuit::ledger::get_circuit_run_inner(db, run_id)?
        .ok_or_else(|| DbError::not_found("Run no longer exists."))?;
    let mut checkpoints = Vec::new();
    let mut coverage = Vec::new();
    {
        let graph = run_graph(db, run_id)?;
        let context = crate::circuit::context::CircuitContext::from_json(&run.context_json)?;
        let steps = crate::db::circuit::ledger::list_circuit_run_steps_inner(db, run_id)?;
        let view = recovery_view(db, &run)?;
        let continuation =
            crate::db::circuit::recovery::review_continuation_supported(&run, &graph);
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
    let run = crate::db::circuit::ledger::get_circuit_run_inner(db, run_id)?
        .ok_or_else(|| DbError::not_found("Run no longer exists."))?;
    let graph = run_graph(db, run_id)?;
    let steps = crate::db::circuit::ledger::list_circuit_run_steps_inner(db, run_id)?;
    let mut checkpoints = Vec::new();
    let mut pushes = Vec::new();
    let continuation = crate::db::circuit::recovery::review_continuation_supported(&run, &graph);
    if run.state == "running" {
        let view = recovery_view(db, &run)?;
        pushes = crate::db::circuit::operator_push::pushes(&view, continuation);
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
        recovery: crate::db::circuit::step_recovery::build_recovery(db, &run, &graph, &steps)?,
        pushes,
    })
}

pub(in crate::db::circuit) fn history_inner(
    db: &Connection,
    run_id: i64,
) -> SqlResult<Vec<CircuitHistoryEntry>> {
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
