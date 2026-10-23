//! Durable identity for the payload retained outside the database. No prompt
//! text is placed in Circuit history.
use super::*;
use crate::circuit::model::CircuitNodeKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeliveryKind {
    Inject,
    Initial,
    Continuation,
    Nudge,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PayloadIdentity {
    pub agent_node_id: i64,
    pub digest: String,
    pub kind: DeliveryKind,
    pub turn: Option<(String, i32)>,
    pub submission_seq: i64,
    pub prepared_at_ms: i64,
}

#[derive(Clone)]
pub(crate) struct InitialDeliveryFence {
    pub run_id: i64,
    pub node_id: String,
    pub attempt: i32,
    pub identity: PayloadIdentity,
}

pub(crate) fn initial_fence(agent: i64) -> Result<Option<InitialDeliveryFence>, DbError> {
    // Capture launch ownership under the writer, alongside the dispatcher's
    // other launch claims; a saturated reader pool must not abandon a spawn.
    let db = crate::db::write_conn();
    let mut query = db.prepare(
        "SELECT s.run_id,s.node_id,s.attempt FROM autopilot_circuit_run_steps s
        JOIN autopilot_circuit_runs r ON r.id=s.run_id
        WHERE r.state='running' AND s.status='running' AND s.agent_node_id=?1",
    )?;
    let rows = query.query_map([agent], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i32>(2)?,
        ))
    })?;
    for row in rows {
        let (run_id, node_id, attempt) = row?;
        if let Some(identity) = latest(&db, run_id, &node_id, attempt)?
            .filter(|p| p.kind == DeliveryKind::Initial && p.agent_node_id == agent)
        {
            return Ok(Some(InitialDeliveryFence {
                run_id,
                node_id,
                attempt,
                identity,
            }));
        }
    }
    Ok(None)
}

fn initial_pending_inner(db: &Connection, fence: &InitialDeliveryFence) -> Result<bool, DbError> {
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM autopilot_circuit_run_steps s
        JOIN autopilot_circuit_runs r ON r.id=s.run_id WHERE r.state='running' AND s.status='running'
        AND s.run_id=?1 AND s.node_id=?2 AND s.attempt=?3 AND s.agent_node_id=?4
        AND NOT EXISTS(SELECT 1 FROM circuit_effects e WHERE e.run_id=s.run_id AND e.node_id=s.node_id
            AND e.attempt=s.attempt AND e.kind='prompt' AND e.state='acknowledged'))",
        params![fence.run_id,fence.node_id,fence.attempt,fence.identity.agent_node_id],|r|r.get(0))?;
    Ok(exists
        && latest(db, fence.run_id, &fence.node_id, fence.attempt)?.as_ref()
            == Some(&fence.identity)
        && newest_submission_seq(db, fence.identity.agent_node_id)?
            == fence.identity.submission_seq)
}

pub(crate) fn initial_pending(fence: &InitialDeliveryFence) -> Result<bool, DbError> {
    let db = crate::db::read_conn()?;
    initial_pending_inner(&db, fence)
}

#[derive(Debug, Clone, serde::Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitPromptRequest.ts")]
pub struct CircuitPromptRequest {
    #[ts(as = "i32")]
    pub run_id: i64,
    pub node_id: String,
    pub attempt: i32,
    #[ts(as = "i32")]
    pub expected_revision: i64,
}

pub(crate) fn retain(
    run_id: i64,
    node_id: &str,
    attempt: i32,
    identity: &mut PayloadIdentity,
    text: &str,
) -> Result<i64, DbError> {
    let mut db = crate::db::write_conn();
    let tx = db.transaction()?;
    record_prompt_submission_locked(&tx, run_id, node_id, attempt, identity.agent_node_id, text)?;
    identity.submission_seq = newest_submission_seq(&tx, identity.agent_node_id)?;
    append_history(
        &tx,
        HistoryEvent {
            run_id,
            node_id: Some(node_id),
            attempt: Some(attempt),
            kind: "prompt_payload",
            detail: &serde_json::to_string(identity)?,
            source: Some(SOURCE_PROMPT_SUBMISSION),
            disposition: Some(DISPOSITION_RECORDED),
        },
    )?;
    let revision = revision_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(revision)
}

fn latest(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> Result<Option<PayloadIdentity>, DbError> {
    let detail: Option<String> = db
        .query_row(
            "SELECT detail FROM circuit_run_history WHERE run_id=?1 AND node_id=?2 AND attempt=?3
         AND kind='prompt_payload' ORDER BY id DESC LIMIT 1",
            params![run_id, node_id, attempt],
            |r| r.get(0),
        )
        .optional()?;
    detail
        .map(|text| serde_json::from_str(&text).map_err(DbError::from))
        .transpose()
}

fn current(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> Result<Option<PayloadIdentity>, DbError> {
    let Some(identity) = latest(db, run_id, node_id, attempt)? else {
        return Ok(None);
    };
    let run = super::super::ledger::get_circuit_run_inner(db, run_id)?
        .ok_or_else(|| DbError::not_found("Run no longer exists."))?;
    if run.state != "running" {
        return Ok(None);
    }
    let view = recovery_view(db, &run)?;
    let Some(step) = view.step(node_id).filter(|s| {
        s.attempt == attempt && s.status == crate::circuit::stepper::StepStatus::Unverified
    }) else {
        return Ok(None);
    };
    let initial_uncertain: bool = if identity.kind == DeliveryKind::Initial {
        db.query_row("SELECT (EXISTS(SELECT 1 FROM circuit_effects WHERE run_id=?1 AND node_id=?2 AND attempt=?3
            AND kind='spawn' AND state IN ('possible_dispatch','uncertain'))
            OR EXISTS(SELECT 1 FROM circuit_run_history WHERE run_id=?1 AND node_id=?2 AND attempt=?3
                AND kind='prompt_delivery_uncertain' AND id>(SELECT MAX(id) FROM circuit_run_history
                    WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='prompt_payload')))
            AND NOT EXISTS(SELECT 1 FROM circuit_effects WHERE run_id=?1 AND node_id=?2 AND attempt=?3
                AND kind='prompt' AND state='acknowledged')",
            params![run_id,node_id,attempt],|r|r.get(0))?
    } else {
        false
    };
    let graph_kind = view.graph.node(node_id).map(|n| &n.kind);
    let kind_matches = match identity.kind {
        DeliveryKind::Inject => matches!(graph_kind, Some(CircuitNodeKind::InjectPty { .. })),
        DeliveryKind::Initial => {
            initial_uncertain && matches!(graph_kind, Some(CircuitNodeKind::SpawnAgentNode { .. }))
        }
        DeliveryKind::Continuation => {
            view.context
                .get(&format!("node.{node_id}.continuation.delivery"))
                == Some("uncertain")
        }
        DeliveryKind::Nudge => {
            view.context.get(&format!("node.{node_id}.nudge.delivery")) == Some("uncertain")
        }
    };
    let target = step
        .agent_node_id
        .or_else(|| view.resolve_target_agent(node_id));
    if !kind_matches
        || target != Some(identity.agent_node_id)
        || newest_submission_seq(db, identity.agent_node_id)? != identity.submission_seq
    {
        return Ok(None);
    }
    Ok(Some(identity))
}

pub(super) fn available(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> Result<bool, DbError> {
    Ok(current(db, run_id, node_id, attempt)?.is_some())
}

pub(crate) fn payload_identity(request: &CircuitPromptRequest) -> Result<PayloadIdentity, DbError> {
    let db = crate::db::read_conn()?;
    validate(&db, request)
}

pub(crate) fn park_initial(fence: &InitialDeliveryFence, reason: &str) -> Result<bool, DbError> {
    let mut db = crate::db::write_conn();
    let tx = db.transaction()?;
    if !initial_pending_inner(&tx, fence)? {
        return Ok(false);
    }
    let InitialDeliveryFence {
        run_id,
        node_id,
        attempt,
        ..
    } = fence;
    super::super::ledger::commit_circuit_advance_inner(
        &tx,
        *run_id,
        None,
        None,
        &[super::super::CircuitStepOp {
            node_id: node_id.clone(),
            status: "unverified".into(),
            attempt: *attempt,
            outcome: Some(None),
            error: Some(Some(format!("Prompt delivery is unverified: {reason}"))),
            agent_node_id: None,
            fresh_attempt: false,
        }],
    )?;
    append_history(
        &tx,
        HistoryEvent {
            run_id: *run_id,
            node_id: Some(node_id),
            attempt: Some(*attempt),
            kind: "prompt_delivery_uncertain",
            detail: reason,
            source: Some(SOURCE_CIRCUIT_WORKER),
            disposition: Some("uncertain"),
        },
    )?;
    tx.commit()?;
    Ok(true)
}

fn validate(db: &Connection, request: &CircuitPromptRequest) -> Result<PayloadIdentity, DbError> {
    if revision_moved(db, request.run_id, request.expected_revision)? {
        return Err(DbError::conflict("The run changed. Refresh before acting."));
    }
    current(db, request.run_id, &request.node_id, request.attempt)?.ok_or_else(|| {
        DbError::conflict(
            "This prompt checkpoint changed or has no retained payload. Refresh before acting.",
        )
    })
}

pub(crate) fn attest(
    request: &CheckpointRequest,
    identity: &PayloadIdentity,
) -> Result<crate::agent::session_lifecycle::LifecycleChangedPayload, DbError> {
    let mut db = crate::db::write_conn();
    attest_locked(&mut db, request, identity)
}

fn attest_locked(
    db: &mut Connection,
    request: &CheckpointRequest,
    identity: &PayloadIdentity,
) -> Result<crate::agent::session_lifecycle::LifecycleChangedPayload, DbError> {
    let tx = db.transaction()?;
    let current = validate(
        &tx,
        &CircuitPromptRequest {
            run_id: request.run_id,
            node_id: request.node_id.clone(),
            attempt: request.attempt,
            expected_revision: request.expected_revision,
        },
    )?;
    if &current != identity {
        return Err(DbError::conflict(
            "The prepared prompt changed. Refresh before acting.",
        ));
    }
    let run = super::super::ledger::get_circuit_run_inner(&tx, request.run_id)?.unwrap();
    let mut view = recovery_view(&tx, &run)?;
    let node = &request.node_id;
    // Resolve old human waits while retaining the independently fenced native
    // turn and report evidence. New input requests can still block later turns.
    let delivered_at_ms: Option<i64> = tx.query_row("SELECT MIN(json_extract(detail,'$.received_at_ms')) FROM circuit_run_history
        WHERE run_id=?1 AND kind='native_hook_received' AND json_extract(detail,'$.agent_node_id')=?2
        AND json_extract(detail,'$.submission_seq')=?3 AND json_extract(detail,'$.hook.event')='UserPromptSubmit'
        AND json_extract(detail,'$.submission_correlated')=1",
        params![request.run_id,identity.agent_node_id,identity.submission_seq],|r|r.get(0))?;
    let cutoff = delivered_at_ms.unwrap_or(identity.prepared_at_ms);
    if matches!(
        crate::db::agent_node::circuit_lifecycle_blocker_inner(&tx, identity.agent_node_id)?,
        Some(crate::circuit::observation::CircuitObservationBlocker::HumanResponseRequired)
    ) {
        let snapshot: Option<String> = tx.query_row(
            "SELECT lifecycle_snapshot FROM agent_nodes WHERE id=?1",
            [identity.agent_node_id],
            |r| r.get(0),
        )?;
        if snapshot
            .and_then(|s| {
                serde_json::from_str::<crate::agent::session_lifecycle::LifecycleChangedPayload>(&s)
                    .ok()
            })
            .and_then(|p| chrono::DateTime::parse_from_rfc3339(&p.timestamp).ok())
            .is_some_and(|at| at.timestamp_millis() <= cutoff)
        {
            tx.execute(
                "UPDATE agent_nodes SET lifecycle_snapshot=NULL WHERE id=?1",
                [identity.agent_node_id],
            )?;
        }
    }
    for step in view.steps.clone() {
        if step
            .agent_node_id
            .or_else(|| view.resolve_target_agent(&step.node_id))
            == Some(identity.agent_node_id)
        {
            view.context.set_lifecycle_blocker(&step.node_id, "");
            view.context
                .set(&format!("node.{}.human_wait", step.node_id), "");
            let key = format!("node.{}.evidence.{}", step.node_id, step.attempt);
            if let Some(mut evidence) = view.context.get(&key).and_then(|json| {
                serde_json::from_str::<crate::circuit::observation::WorkEvidence>(json).ok()
            }) {
                for wait in &mut evidence.human_waits {
                    if wait.observed_at_ms <= cutoff {
                        wait.resolved_at_ms.get_or_insert(cutoff);
                    }
                }
                view.context.set(&key, serde_json::to_string(&evidence)?);
            }
        }
    }
    view.context.set(&format!("node.{node}.recheck_only"), "0");
    view.context
        .set(&format!("node.{node}.observation_blocker"), "");
    if identity.kind == DeliveryKind::Nudge {
        view.context
            .set(&format!("node.{node}.nudge.delivery"), "delivered");
    } else if identity.kind == DeliveryKind::Continuation {
        view.context
            .set(&format!("node.{node}.continuation.delivery"), "delivered");
    } else {
        // The normal acknowledgement observer completes InjectPty and starts
        // its successor in one transition, retaining a receipt target until then.
        view.context.set(
            &format!("node.{node}.prompt_delivery.{}", request.attempt),
            if identity.kind == DeliveryKind::Inject {
                "intent"
            } else {
                "acknowledged"
            },
        );
    }
    tx.execute("INSERT INTO circuit_effects(run_id,node_id,attempt,kind,state) VALUES(?1,?2,?3,'prompt','acknowledged')
        ON CONFLICT(run_id,node_id,attempt,kind) DO UPDATE SET state='acknowledged'",
        params![request.run_id, node, request.attempt])?;
    crate::db::agent_node::update_agent_node_status_inner(
        &tx,
        identity.agent_node_id,
        crate::models::SessionStatus::Running,
    )?;
    let mut lifecycle = crate::agent::session_lifecycle::LifecycleChangedPayload::new(
        identity.agent_node_id,
        crate::agent::session_lifecycle::LifecycleKind::WorkResumed,
        crate::models::SessionStatus::Running,
        &crate::agent::session_lifecycle::HookSignalDetail::default(),
        "Operator confirmed manual prompt delivery",
    );
    lifecycle.timestamp = tx.query_row(
        "SELECT status_changed_at FROM agent_nodes WHERE id=?1",
        [identity.agent_node_id],
        |r| r.get(0),
    )?;
    super::super::ledger::commit_circuit_advance_inner(
        &tx,
        run.id,
        None,
        Some(&view.context.to_json()?),
        &[super::super::CircuitStepOp {
            node_id: node.clone(),
            status: "running".into(),
            attempt: request.attempt,
            outcome: Some(None),
            error: Some(None),
            agent_node_id: None,
            fresh_attempt: false,
        }],
    )?;
    let reason = if request.reason.trim().is_empty() {
        "Operator confirmed manual prompt delivery.".to_string()
    } else {
        format!(
            "Operator confirmed manual prompt delivery: {}",
            request.reason.trim()
        )
    };
    append_history(
        &tx,
        HistoryEvent {
            run_id: run.id,
            node_id: Some(node),
            attempt: Some(request.attempt),
            kind: "operator_attestation",
            detail: &reason,
            source: Some(SOURCE_OPERATOR),
            disposition: Some("manual_delivered"),
        },
    )?;
    tx.commit()?;
    Ok(lifecycle)
}

#[cfg(test)]
#[path = "prompt_delivery_tests.rs"]
mod tests;
