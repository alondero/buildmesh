//! Durable external-effect claims and append-only Circuit Run History.

use crate::db::SqlResult;
use rusqlite::{params, Connection, OptionalExtension};

const OBSERVATION_FRESHNESS_REJECTION_PREFIX: &str = "Observation freshness fence rejected:";

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
}

pub fn history(run_id: i64) -> Result<CircuitEvidenceView, String> {
    let db = crate::db::read_conn();
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let view = evidence_view_inner(&tx, run_id)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(view)
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
        let view = crate::autopilot::circuit::stepper::RunView {
            run_id, graph: graph.clone(), state: crate::autopilot::circuit::stepper::RunState::from_db_str(&run.state),
            context: context.clone(), steps: steps.iter().map(|step| crate::autopilot::circuit::stepper::StepView {
                node_id: step.node_id.clone(), attempt: step.attempt,
                status: crate::autopilot::circuit::stepper::StepStatus::from_db_str(&step.status),
                outcome: step.outcome.as_deref().and_then(crate::autopilot::circuit::model::StepOutcome::from_db_str),
                error: step.error_message.clone(), agent_node_id: step.agent_node_id,
            }).collect(),
        };
        for step in steps {
            if let Some(agent_id) = step.agent_node_id.or_else(|| view.resolve_target_agent(&step.node_id)) {
                if let Some(agent) = crate::db::agent_node::get_agent_node_by_id_inner(db, agent_id).optional().map_err(|e| e.to_string())? {
                    let deadline_ms = view.evidence_deadline_ms(&step.node_id);
                    coverage.push(CircuitStepObservationCoverage {
                        node_id: step.node_id.clone(), attempt: step.attempt,
                        platform: format!("{} host / {} launch", std::env::consts::OS, agent.env),
                        capabilities: crate::services::circuit_worker::observer_policy::for_agent(&agent), deadline_ms,
                        waits_active: run.state == "running" && !matches!(step.status.as_str(), "completed" | "failed" | "cancelled"),
                        human_waits: context.get(&format!("node.{}.evidence.{}", step.node_id, step.attempt))
                            .and_then(|json| serde_json::from_str::<crate::autopilot::circuit::observation::WorkEvidence>(json).ok())
                            .map(|evidence| evidence.human_waits).unwrap_or_default(),
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
                ) => vec![CheckpointAction::Recheck],
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
    let mut stmt = db.prepare("SELECT id,node_id,attempt,kind,detail,observed_at FROM circuit_run_history WHERE run_id=?1 ORDER BY id")?;
    let rows = stmt
        .query_map([run_id], |r| {
            Ok(CircuitHistoryEntry {
                id: r.get(0)?,
                node_id: r.get(1)?,
                attempt: r.get(2)?,
                kind: r.get(3)?,
                detail: r.get(4)?,
                observed_at: r.get(5)?,
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

fn receive_native_hook_locked(
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
        // Receipt arrival can follow a newer PTY submission. Only the persisted
        // turn-start binding may correlate a terminal hook with user input.
        let binding: Option<Option<String>> = tx.query_row(
            "SELECT json_extract(detail,'$.input_stamp') FROM circuit_run_history
             WHERE run_id=?1 AND kind='native_hook_received'
             AND json_extract(detail,'$.agent_node_id')=?2
             AND json_extract(detail,'$.session_incarnation')=?3
             AND json_extract(detail,'$.hook.session_id')=?4
             AND json_extract(detail,'$.hook.turn_id')=?5
             AND json_extract(detail,'$.hook.event')='UserPromptSubmit'
             AND json_extract(detail,'$.submission_correlated')=1
             ORDER BY id LIMIT 1",
            params![run_id, receipt.agent_node_id, receipt.session_incarnation, receipt.hook.session_id, receipt.hook.turn_id],
            |row| row.get(0)).optional().map_err(|e| e.to_string())?;
        if receipt.hook.event != "UserPromptSubmit" || binding.is_some() {
            receipt.submission_correlated = binding.as_ref().is_some_and(|stamp| stamp.is_some());
            receipt.input_stamp = binding.flatten();
        } else if !receipt.submission_correlated || !receipt.turn_fenced || receipt.hook.turn_id.is_none()
            || receipt.hook.session_id.is_none() || receipt.session_incarnation.is_none() {
            receipt.input_stamp = None;
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
            )
            .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

pub(crate) fn native_hook_receipts(run_id: i64, after: i64) -> SqlResult<Vec<CircuitHistoryEntry>> {
    let db = crate::db::read_conn();
    let mut stmt = db.prepare("SELECT id,node_id,attempt,kind,detail,observed_at FROM circuit_run_history WHERE run_id=?1 AND id>?2 AND kind='native_hook_received' ORDER BY id LIMIT 512")?;
    let rows = stmt.query_map(params![run_id, after], |r| {
        Ok(CircuitHistoryEntry {
            id: r.get(0)?,
            node_id: r.get(1)?,
            attempt: r.get(2)?,
            kind: r.get(3)?,
            detail: r.get(4)?,
            observed_at: r.get(5)?,
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
        super::ledger::commit_circuit_advance_inner(
            &tx,
            request.run_id,
            None,
            Some(&context.to_json()?),
            &[op],
        )
        .map_err(|e| e.to_string())?;
        append_history(
            &tx,
            request.run_id,
            Some(&request.node_id),
            Some(request.attempt),
            "evidence_recheck",
            request.reason.trim(),
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
    let gates = super::ledger::list_circuit_run_steps_inner(&tx, request.run_id)
        .map_err(|e| e.to_string())?;
    if graph.has_ancestor_matching(&request.node_id, |node| {
        matches!(
            node.kind,
            CircuitNodeKind::CollaboratorCheck {
                require_approval: true
            } | CircuitNodeKind::ReviewVerdict { .. }
        ) && !gates.iter().any(|s| {
            s.node_id == node.id
                && s.status == "completed"
                && s.outcome.as_deref() == Some("completed")
        })
    }) {
        return Err(
            "Separate permission and review approval gates must be satisfied first.".into(),
        );
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
    let op = super::CircuitStepOp {
        node_id: request.node_id.clone(),
        status: status.into(),
        attempt,
        outcome,
        error: Some(Some(reason.clone())),
        agent_node_id: None,
        fresh_attempt: attempt != step.attempt,
    };
    super::ledger::commit_circuit_advance_inner(
        &tx,
        request.run_id,
        None,
        Some(&context.to_json()?),
        &[op],
    )
        .map_err(|e| e.to_string())?;
    append_history(
        &tx,
        request.run_id,
        Some(&request.node_id),
        Some(request.attempt),
        "operator_attestation",
        &reason,
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

#[derive(Debug, Clone)]
pub struct EffectIntent {
    pub node_id: String,
    pub attempt: i32,
    pub kind: String,
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
        append_history(&tx,run_id,None,None,"queue_wait",&detail)?;
    }
    tx.commit()
}

pub(super) fn pin_graph(db: &Connection, run_id: i64) -> SqlResult<()> {
    use sha2::{Digest, Sha256};
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
    }).to_string())?;
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
                }).to_string())?;
            }
            append_history(db,run_id,Some(node),attempt,"continuation_effect",&serde_json::json!({
                "effect":"continuation_prompt","state":state,"ordinal":next.get(&count_key)
            }).to_string())?;
        }
    }
    let capacity_keys: std::collections::BTreeSet<&String> = previous.keys().chain(next.keys()).filter(|key|key.starts_with("node.") && key.ends_with(".capacity_wait")).collect();
    for key in capacity_keys {
        if previous.get(key) != next.get(key) {
            let node = key.strip_prefix("node.").and_then(|key|key.strip_suffix(".capacity_wait"));
            append_history(db,run_id,node,None,"step_capacity_wait",&serde_json::json!({"before":previous.get(key),"after":next.get(key)}).to_string())?;
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
            let attempt = next.get(&format!("{prefix}attempt")).and_then(|value|value.parse().ok());
            append_history(db,run_id,Some(node),attempt,"evidence_window_changed",&serde_json::json!({"before":before,"after":after}).to_string())?;
        }
    }
    Ok(())
}

pub(super) fn run_graph(
    db: &Connection,
    run_id: i64,
) -> Result<crate::autopilot::circuit::model::CircuitGraph, String> {
    let json: String = db
        .query_row(
            "SELECT COALESCE(s.graph_json,c.graph_json)
        FROM autopilot_circuit_runs r JOIN autopilot_circuits c ON c.id=r.circuit_id
        LEFT JOIN circuit_run_snapshots s ON s.run_id=r.id WHERE r.id=?1",
            [run_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    crate::autopilot::circuit::model::CircuitGraph::from_json(&json)
}

pub(super) fn append_history(
    db: &Connection,
    run_id: i64,
    node_id: Option<&str>,
    attempt: Option<i32>,
    kind: &str,
    detail: &str,
) -> SqlResult<()> {
    db.execute("INSERT INTO circuit_run_history (run_id,node_id,attempt,kind,detail) VALUES (?1,?2,?3,?4,?5)",
        params![run_id, node_id, attempt, kind, crate::secret_scrubber::SecretScrubber::scrub(detail)])?;
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
    pub reconciled_effects: &'a [ReconciledEffect],
    pub observations: &'a [crate::autopilot::circuit::observation::RecordedObservation],
    pub classifications: &'a [crate::autopilot::circuit::observation::RecordedClassification],
    pub expected: Option<&'a crate::autopilot::circuit::stepper::TransitionFence>,
}

fn commit_transition_locked(
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
    super::ledger::commit_circuit_advance_inner(&tx, run_id, state, Some(context), steps)?;
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
        )?;
    }
    for classification in evidence.classifications {
        let detail = serde_json::to_string(classification).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        append_history(&tx, run_id, Some(&classification.step_id), Some(classification.attempt), "classification", &detail)?;
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
        if !completed || effect.intent.kind != "github" || !is_open_pr {
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
            params![run_id,intent.node_id,intent.attempt,intent.kind])?;
        if inserted == 1 {
            append_history(
                &tx,
                run_id,
                Some(&intent.node_id),
                Some(intent.attempt),
                "effect_intent",
                &intent.kind,
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
                    else { "Agent allocation attached; foreground and owned work completion still require evidence" }}).to_string())?;
    }
    let revision = revision_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(Some(revision))
}

pub fn claim_effect(run_id: i64, intent: &EffectIntent) -> SqlResult<Option<i64>> {
    let mut db = crate::db::write_conn();
    claim_effect_locked(&mut db, run_id, intent)
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
        params![run_id,intent.node_id,intent.attempt,intent.kind])? == 1;
    if claimed {
        append_history(
            &tx,
            run_id,
            Some(&intent.node_id),
            Some(intent.attempt),
            "effect_possible_dispatch",
            &intent.kind,
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
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'pending');").unwrap();
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
        let mut receipt = NativeReceipt { agent_node_id: 9, input_stamp: Some("1:2".into()), session_incarnation: Some("1000".into()), source_id: "event-1".into(), received_at_ms: 1, turn_fenced: true, explicit_turn_mismatch: false, submission_correlated: false,
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
        let mut receipt = NativeReceipt { agent_node_id: 9, input_stamp: Some("input-a".into()), session_incarnation: Some("1000".into()),
            source_id: "start-a".into(), received_at_ms: 1000, turn_fenced: true, explicit_turn_mismatch: false, submission_correlated: true,
            hook: NativeHook::parse("claude", br#"{"session_id":"session","prompt_id":"a","hook_event_name":"UserPromptSubmit"}"#).unwrap() };
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        drop(db);
        let mut db = Connection::open(file.path()).unwrap();
        // Input B was submitted, but its start hook has not arrived.
        receipt.input_stamp = Some("input-b".into());
        receipt.hook.event = "Stop".into();
        receipt.source_id = "stop-a".into();
        receipt.received_at_ms = 2000;
        receipt.hook.final_report = Some("Old approval".into());
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        let history = history_inner(&db, 1).unwrap();
        let old: NativeReceipt = serde_json::from_str(&history[1].detail).unwrap();
        assert_eq!(old.input_stamp.as_deref(), Some("input-a"));
        receipt.hook.event = "UserPromptSubmit".into();
        receipt.hook.final_report = None;
        receipt.hook.turn_id = Some("b".into());
        receipt.source_id = "start-b".into();
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        receipt.hook.event = "Stop".into();
        receipt.source_id = "stop-b".into();
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        let history = history_inner(&db, 1).unwrap();
        let current: NativeReceipt = serde_json::from_str(&history[3].detail).unwrap();
        assert_eq!(current.input_stamp.as_deref(), Some("input-b"));
        // An unobserved start cannot acquire authority from receipt-time input.
        receipt.hook.turn_id = Some("missing-start".into());
        receipt.source_id = "stop-c".into();
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        let history = history_inner(&db, 1).unwrap();
        let missing: NativeReceipt = serde_json::from_str(&history[4].detail).unwrap();
        assert_eq!(missing.input_stamp, None);
        // The real hook transport has no native acknowledgement of submission.
        // A delayed first start A arriving after input B cannot create a binding.
        receipt.hook.event = "UserPromptSubmit".into();
        receipt.hook.turn_id = Some("delayed-first-start".into());
        receipt.source_id = "delayed-start".into();
        receipt.submission_correlated = false;
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        receipt.hook.event = "Stop".into();
        receipt.source_id = "delayed-stop".into();
        receive_native_hook_locked(&mut db, &receipt).unwrap();
        let history = history_inner(&db, 1).unwrap();
        for entry in &history[5..] {
            let receipt: NativeReceipt = serde_json::from_str(&entry.detail).unwrap();
            assert_eq!(receipt.input_stamp, None);
            assert!(!receipt.submission_correlated);
        }
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
                transcript_guard: None,
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
            kind: "github".into(),
        };
        let write = || EvidenceWrite {
            input_guard: None,
            intents: std::slice::from_ref(&intent),
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
            intent: EffectIntent { node_id: "open_pr".into(), attempt: 1, kind: "github".into() },
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
        let intent = EffectIntent { node_id:"spawn".into(),attempt:1,kind:"spawn".into() };
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
    fn effect_claim_survives_reopen_and_is_never_replayed() {
        for kind in ["github", "prompt", "spawn"] {
            let file = tempfile::NamedTempFile::new().unwrap();
            let mut db = Connection::open(file.path()).unwrap();
            crate::db::init_schema(&db).unwrap();
            db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');").unwrap();
            let intent = EffectIntent {
                node_id: "comment".into(),
                attempt: 1,
                kind: kind.into(),
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
            assert!(claim_effect_locked(&mut db, 1, &intent).unwrap().is_some());
            drop(db);
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
                kind: "github".into(),
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
                kind: "github".into(),
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
