//! Record operator outcomes under the existing transaction and observation freshness fences.

use super::*;
pub fn record_outcome(request: &CheckpointRequest) -> Result<(), DbError> {
    if matches!(request.action, CheckpointAction::ManualDelivered) {
        return crate::services::circuit_prompt::attest(request);
    }
    let mut db = crate::db::write_conn();
    record_outcome_locked(&mut db, request)
}

pub(in crate::db::circuit) fn record_outcome_locked(
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
    let run = crate::db::circuit::ledger::get_circuit_run_inner(&tx, request.run_id)?
        .ok_or_else(|| DbError::not_found("Run no longer exists."))?;
    if run.state != "running" {
        return Err(DbError::invalid(
            "Only an active run can resolve a checkpoint.",
        ));
    }
    let step = crate::db::circuit::ledger::list_circuit_run_steps_inner(&tx, request.run_id)?
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
        let commit = crate::db::circuit::ledger::commit_circuit_advance_inner(
            &tx,
            run.id,
            None,
            Some(&view.context.to_json()?),
            &[crate::db::circuit::CircuitStepOp {
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
        crate::db::circuit::ledger::commit_circuit_advance_inner(
            &tx,
            run.id,
            None,
            Some(&view.context.to_json()?),
            &[crate::db::circuit::CircuitStepOp {
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
        let mut context = super::super::artifacts::load_context(&tx, request.run_id)?;
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
        // The recheck clears the step's error. Keep the checkpoint it is
        // rechecking, so a recheck that finds nothing new can restore it.
        context.set(
            &format!("node.{}.recheck_checkpoint", step.node_id),
            step.error_message.clone().unwrap_or_default(),
        );
        let op = crate::db::circuit::CircuitStepOp {
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
        let commit = crate::db::circuit::ledger::commit_circuit_advance_inner(
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
    let mut context = super::super::artifacts::load_context(&tx, request.run_id)?;
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
    let op = crate::db::circuit::CircuitStepOp {
        node_id: request.node_id.clone(),
        status: status.into(),
        attempt,
        outcome,
        error: Some((status != "completed").then(|| reason.clone())),
        agent_node_id: None,
        fresh_attempt: attempt != step.attempt,
    };
    let commit = crate::db::circuit::ledger::commit_circuit_advance_inner(
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
