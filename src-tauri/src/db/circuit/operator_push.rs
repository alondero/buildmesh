//! Operator push: move a stuck agent wait or classifier step on by hand.
//!
//! A Circuit can wait on evidence that will never arrive: a background task
//! the harness reported whose completion hook was lost, a turn whose report
//! never landed, or a classifier that could not decide. An evidence recheck
//! cannot help when the evidence itself is stale. A push lets the person say
//! what the step's outcome was, and records that they overrode the evidence.
//!
//! Eligibility is one rule, [`RunView::operator_push_outcomes`]: the attention
//! view reads it to decide what to offer and the write enforces it again
//! against the revision the person acted on. A push is an Operator-recorded
//! Outcome: it never supplies a review verdict or a permission decision.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::evidence::{
    append_history, recovery_view, revision_moved, HistoryEvent, SOURCE_OPERATOR,
};
use crate::circuit::model::StepOutcome;
use crate::circuit::observation::{CircuitObservationBlocker, WorkEvidence};
use crate::circuit::stepper::RunView;
use crate::db::DbError;

/// The outcome a person can give a pushed step. A subset of the routed gate
/// outcomes: a person can say the work finished, is still in progress, or
/// needs someone, but cannot record a failure or a check result this way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "StepPushOutcome.ts")]
pub enum StepPushOutcome {
    Completed,
    Working,
    Blocked,
}

impl StepPushOutcome {
    fn step_outcome(self) -> StepOutcome {
        match self {
            Self::Completed => StepOutcome::Completed,
            Self::Working => StepOutcome::Working,
            Self::Blocked => StepOutcome::Blocked,
        }
    }

    fn from_step_outcome(outcome: StepOutcome) -> Option<Self> {
        match outcome {
            StepOutcome::Completed => Some(Self::Completed),
            StepOutcome::Working => Some(Self::Working),
            StepOutcome::Blocked => Some(Self::Blocked),
            _ => None,
        }
    }
}

/// One step a person can push on right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitStepPush.ts")]
pub struct CircuitStepPush {
    pub node_id: String,
    pub attempt: i32,
    /// The routed outcomes the person may choose, `completed` first.
    pub outcomes: Vec<StepPushOutcome>,
    /// The evidence the push would set aside, in plain words. `None` when
    /// nothing is holding the step and the push only saves waiting.
    pub overrides: Option<String>,
    /// Choosing completed starts another review pass.
    pub starts_next_review: bool,
    /// Choosing completed uses the last review round.
    pub ends_review: bool,
    /// Review again can add another round after the run stops. False when
    /// recovery would reject this graph as a custom circuit.
    pub review_again: bool,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitStepPushRequest.ts")]
pub struct CircuitStepPushRequest {
    #[ts(as = "i32")]
    pub run_id: i64,
    pub node_id: String,
    pub attempt: i32,
    #[ts(as = "i32")]
    pub expected_revision: i64,
    pub outcome: StepPushOutcome,
    /// Optional note for the run history.
    pub reason: String,
}

/// Every step of this view a person can push on. Empty unless the run is
/// running, because [`RunView::operator_push_outcomes`] is.
pub(crate) fn pushes(view: &RunView, continuation_supported: bool) -> Vec<CircuitStepPush> {
    view.steps
        .iter()
        .filter_map(|step| {
            let outcomes: Vec<_> = view
                .operator_push_outcomes(&step.node_id)
                .into_iter()
                .filter_map(StepPushOutcome::from_step_outcome)
                .collect();
            (!outcomes.is_empty()).then(|| {
                let ends_review = view.completion_ends_review(&step.node_id);
                CircuitStepPush {
                    node_id: step.node_id.clone(),
                    attempt: step.attempt,
                    outcomes,
                    overrides: holding_evidence(view, &step.node_id).map(|b| b.message()),
                    starts_next_review: view.completion_starts_next_review(&step.node_id),
                    ends_review,
                    review_again: ends_review && continuation_supported,
                }
            })
        })
        .collect()
}

/// What is currently holding the step: blocking work or input evidence first,
/// then the observer's own reason for not deciding yet.
fn holding_evidence(view: &RunView, node_id: &str) -> Option<CircuitObservationBlocker> {
    view.report_blocker(node_id).or_else(|| {
        view.context
            .get(&format!("node.{node_id}.observation_blocker"))
            .filter(|json| !json.is_empty())
            .and_then(|json| serde_json::from_str(json).ok())
    })
}

pub fn push_step(request: &CircuitStepPushRequest) -> Result<(), DbError> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut db = crate::db::write_conn();
    push_step_locked(&mut db, request, now_ms)
}

pub(crate) fn push_step_locked(
    db: &mut Connection,
    request: &CircuitStepPushRequest,
    now_ms: i64,
) -> Result<(), DbError> {
    let note = request.reason.trim();
    let tx = db.transaction()?;
    if revision_moved(&tx, request.run_id, request.expected_revision)? {
        return Err(DbError::conflict(
            "The run changed. Refresh it before acting.",
        ));
    }
    let run = super::ledger::get_circuit_run_inner(&tx, request.run_id)?
        .ok_or_else(|| DbError::not_found("Run no longer exists."))?;
    if run.state != "running" {
        return Err(DbError::invalid("Only a running run can be moved on."));
    }
    let mut view = recovery_view(&tx, &run)?;
    let outcome = request.outcome.step_outcome();
    let Some(step) = view
        .step(&request.node_id)
        .filter(|step| step.attempt == request.attempt)
        .cloned()
    else {
        return Err(DbError::conflict(
            "This step changed. Refresh it before acting.",
        ));
    };
    if !view
        .operator_push_outcomes(&request.node_id)
        .contains(&outcome)
    {
        return Err(DbError::conflict(
            "This step can no longer be moved on that way. Refresh it before acting.",
        ));
    }
    let overridden = holding_evidence(&view, &request.node_id);
    let agent = step
        .agent_node_id
        .or_else(|| view.resolve_target_agent(&request.node_id));
    if let Some(agent) = agent {
        settle_stale_evidence(&mut view, agent, now_ms)?;
    }
    let node = &request.node_id;
    view.context
        .set(&format!("node.{node}.status"), outcome.as_db_str());
    view.context.set(&format!("node.{node}.wait.attempt"), "");
    view.context
        .set(&format!("node.{node}.observation_blocker"), "");
    view.context.set(&format!("node.{node}.recheck_only"), "0");
    let cleared_lifecycle = match agent {
        Some(agent) => clear_blocking_lifecycle(&tx, agent)?,
        None => false,
    };
    let commit = super::ledger::commit_circuit_advance_inner(
        &tx,
        run.id,
        None,
        Some(&view.context.to_json()?),
        &[super::CircuitStepOp {
            node_id: step.node_id.clone(),
            status: "completed".into(),
            attempt: step.attempt,
            outcome: Some(Some(outcome.as_db_str().into())),
            error: Some(None),
            agent_node_id: None,
            fresh_attempt: false,
        }],
    )?;
    if !commit.applied {
        return Err(DbError::conflict(
            "The run finished before this step could be moved on.",
        ));
    }
    let mut detail = format!("Operator moved this step on as {}.", outcome.as_db_str());
    if let Some(blocker) = &overridden {
        detail.push_str(&format!(" Overrode: {}", blocker.message()));
    }
    if cleared_lifecycle {
        detail.push_str(" Cleared the agent's stale background-work or input report.");
    }
    if !note.is_empty() {
        detail.push_str(&format!(" Note: {note}"));
    }
    append_history(
        &tx,
        HistoryEvent {
            run_id: run.id,
            node_id: Some(&step.node_id),
            attempt: Some(step.attempt),
            kind: "operator_override",
            detail: &detail,
            source: Some(SOURCE_OPERATOR),
            disposition: Some(outcome.as_db_str()),
        },
    )?;
    Ok(tx.commit()?)
}

/// Mark the outstanding work and open input waits on every step reporting on
/// `agent` as finished. This is the same scope [`RunView::report_blocker`]
/// reads, so the evidence the person overrode cannot hold the successor gates
/// on that agent. New observations still record new work after this point.
fn settle_stale_evidence(view: &mut RunView, agent: i64, now_ms: i64) -> Result<(), DbError> {
    let keys: Vec<(String, String)> = view
        .steps
        .iter()
        .filter(|step| {
            step.agent_node_id
                .or_else(|| view.resolve_target_agent(&step.node_id))
                == Some(agent)
        })
        .map(|step| {
            (
                format!("node.{}.evidence.{}", step.node_id, step.attempt),
                format!("node.{}.human_wait", step.node_id),
            )
        })
        .collect();
    for (evidence_key, legacy_wait_key) in keys {
        if view.context.get(&legacy_wait_key) == Some("1") {
            view.context.set(&legacy_wait_key, "");
        }
        let Some(json) = view.context.get(&evidence_key) else {
            continue;
        };
        // Unreadable evidence is a conflict, which the eligibility rule refuses.
        let Ok(mut evidence) = serde_json::from_str::<WorkEvidence>(json) else {
            continue;
        };
        let open_work = evidence.children.values().any(|terminal| !terminal);
        let open_waits = evidence
            .human_waits
            .iter()
            .any(|wait| wait.resolved_at_ms.is_none());
        if !open_work && !open_waits {
            continue;
        }
        evidence
            .children
            .values_mut()
            .for_each(|terminal| *terminal = true);
        for wait in &mut evidence.human_waits {
            wait.resolved_at_ms.get_or_insert(now_ms);
        }
        let encoded = serde_json::to_string(&evidence)?;
        view.context.set(&evidence_key, encoded);
    }
    Ok(())
}

/// Drop the agent's last lifecycle report when it is the one vetoing the
/// Circuit (background work running, or a permission or question pending).
/// The next harness report replaces it as usual. Returns whether it cleared.
fn clear_blocking_lifecycle(db: &Connection, agent: i64) -> Result<bool, DbError> {
    let blocker = crate::db::agent_node::circuit_lifecycle_blocker_inner(db, agent)?;
    if !matches!(
        blocker,
        Some(
            CircuitObservationBlocker::KnownWorkOutstanding
                | CircuitObservationBlocker::HumanResponseRequired
        )
    ) {
        return Ok(false);
    }
    db.execute(
        "UPDATE agent_nodes SET lifecycle_snapshot=NULL WHERE id=?1",
        [agent],
    )?;
    Ok(true)
}
