//! Atomically commit stepper transitions, evidence writes, and external-effect intents.

use super::*;
/// State, step projection and effect intent land in one transaction.
pub fn commit_transition(
    run_id: i64,
    state: Option<&str>,
    context: &str,
    steps: &[crate::db::circuit::CircuitStepOp],
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
    steps: &[crate::db::circuit::CircuitStepOp],
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
        let current = crate::db::circuit::ledger::get_circuit_run_inner(&tx, run_id)?;
        let durable = crate::db::circuit::ledger::list_circuit_run_steps_inner(&tx, run_id)?;
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
    let commit = crate::db::circuit::ledger::commit_circuit_advance_inner(
        &tx,
        run_id,
        state,
        Some(context),
        steps,
    )?;
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
