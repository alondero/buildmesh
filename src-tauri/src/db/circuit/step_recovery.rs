//! Manual recovery for a failed Circuit Run.
//!
//! A person looking at a failed run needs two things: to know what they can do,
//! and to do it. Both come from one eligibility decision ([`build_recovery`]),
//! so the buttons the UI offers are exactly the ones the command accepts.
//!
//! * **Retry** runs the failed step again as a new attempt.
//! * **Continue** records that the person did the step's work themselves
//!   (an operator attestation) and lets the run move on to the next stage.
//!
//! Either reopens the run: it re-enters the admission queue like any pending
//! run and the worker carries on from the step ledger. Earlier history is kept.
//! A review verdict, a pull-request lookup and a spawn can never be attested,
//! because an attestation must not stand in for evidence the engine requires.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::evidence::{append_history, run_graph, DISPOSITION_APPLIED, SOURCE_OPERATOR};
use crate::circuit::model::{CircuitGraph, CircuitNodeKind, GithubActionKind};
use crate::models::{AutopilotCircuitRun, AutopilotCircuitRunStep, SessionStatus};

/// Written on every sibling step the stepper cancels when a run fails. It says
/// nothing about why, so it never counts as the failure and is cleared on reopen.
const SIBLING_SWEEP_NOTE: &str = "Cancelled because the circuit run";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "RecoveryAction.ts")]
pub enum RecoveryAction {
    Retry,
    Continue,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "RunRecoveryOption.ts")]
pub struct RunRecoveryOption {
    pub action: RecoveryAction,
    pub available: bool,
    /// Why the action is not available, in plain words and with what to do about it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unavailable_reason: Option<String>,
}

/// What a person can do about a failed run, anchored on the step that failed.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "RunRecovery.ts")]
pub struct RunRecovery {
    pub node_id: String,
    #[ts(as = "i32")]
    pub attempt: i32,
    /// `failed`, or `cancelled` when the agent the step was waiting on was closed.
    pub status: String,
    pub error: Option<String>,
    pub options: Vec<RunRecoveryOption>,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "RecoveryRequest.ts")]
pub struct RecoveryRequest {
    #[ts(as = "i32")]
    pub run_id: i64,
    pub node_id: String,
    pub attempt: i32,
    /// The latest history id the person saw, so a stale click is refused.
    #[ts(as = "i32")]
    pub expected_revision: i64,
    pub action: RecoveryAction,
    /// Required to continue (it is an attestation); optional for a retry.
    #[serde(default)]
    pub reason: String,
}

/// The step that says why the run failed: the first failed step, else the first
/// step cancelled with a reason of its own (an agent closed under it).
pub(crate) fn problem_step(steps: &[AutopilotCircuitRunStep]) -> Option<&AutopilotCircuitRunStep> {
    let has_error = |step: &&AutopilotCircuitRunStep| {
        step.error_message
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
    };
    steps
        .iter()
        .find(|step| step.status == "failed")
        .or_else(|| {
            steps.iter().filter(has_error).find(|step| {
                step.status == "cancelled"
                    && !step
                        .error_message
                        .as_deref()
                        .unwrap_or("")
                        .starts_with(SIBLING_SWEEP_NOTE)
            })
        })
}

/// The agent a step works through, as far as recovery cares.
enum AgentCondition {
    /// No agent is attached to or targeted by the step.
    Missing,
    /// The agent node no longer exists.
    Gone,
    /// The agent node exists but was closed; resuming it from Archive helps.
    Closed,
    /// An agent is attached or targeted and could take the step.
    Live,
}

fn agent_condition(db: &Connection, agent: Option<i64>) -> Result<AgentCondition, String> {
    let Some(id) = agent else {
        return Ok(AgentCondition::Missing);
    };
    match crate::db::agent_node::get_agent_node_by_id_inner(db, id).optional() {
        Ok(None) => Ok(AgentCondition::Gone),
        Ok(Some(node)) => Ok(
            if matches!(
                node.status,
                SessionStatus::Archived | SessionStatus::Lost | SessionStatus::Error
            ) {
                AgentCondition::Closed
            } else {
                AgentCondition::Live
            },
        ),
        Err(error) => Err(error.to_string()),
    }
}

fn retry_refusal(kind: &CircuitNodeKind, agent: &AgentCondition) -> Option<String> {
    match kind {
        CircuitNodeKind::Manual
        | CircuitNodeKind::Interval { .. }
        | CircuitNodeKind::GithubIssueLabel { .. }
        | CircuitNodeKind::GithubPullRequestLabel { .. }
        | CircuitNodeKind::CollaboratorCheck { .. }
        | CircuitNodeKind::RetryLimit { .. }
        | CircuitNodeKind::AllCompleted
        | CircuitNodeKind::AnyCompleted => {
            Some("This step is a trigger or a join, so there is nothing to run again.".into())
        }
        // A new attempt starts a fresh agent, so an agent still open for this
        // step would be orphaned rather than replaced.
        CircuitNodeKind::SpawnAgentNode { .. } => match agent {
            AgentCondition::Live => Some(
                "This step's agent is still open. Close it first, or carry on in the agent itself."
                    .into(),
            ),
            _ => None,
        },
        CircuitNodeKind::InjectPty { .. }
        | CircuitNodeKind::AwaitAgentTurn { .. }
        | CircuitNodeKind::LlmTurnClassifier { .. }
        | CircuitNodeKind::ReviewVerdict { .. } => match agent {
            AgentCondition::Live => None,
            AgentCondition::Closed => Some(
                "The agent for this step was closed. Resume it from Archive, then retry.".into(),
            ),
            AgentCondition::Gone | AgentCondition::Missing => {
                Some("The agent for this step no longer exists, so it cannot be run again.".into())
            }
        },
        _ => None,
    }
}

fn continue_refusal(kind: &CircuitNodeKind, agent: &AgentCondition) -> Option<String> {
    match kind {
        CircuitNodeKind::InjectPty { .. } => None,
        CircuitNodeKind::AwaitAgentTurn { .. } | CircuitNodeKind::LlmTurnClassifier { .. } => {
            match agent {
                AgentCondition::Live | AgentCondition::Closed => None,
                AgentCondition::Gone | AgentCondition::Missing => Some(
                    "The agent for this step no longer exists, so there is nothing to continue with."
                        .into(),
                ),
            }
        }
        CircuitNodeKind::ReviewVerdict { .. } => Some(
            "A review approval cannot be recorded on the reviewer's behalf. Use Review again."
                .into(),
        ),
        CircuitNodeKind::SpawnAgentNode { .. } => Some(
            "An agent cannot be recorded as started when it was not. Retry the step instead."
                .into(),
        ),
        CircuitNodeKind::GithubAction {
            action: GithubActionKind::OpenPr | GithubActionKind::ConfirmPrMerged,
            ..
        } => Some(
            "The pull request must be found on GitHub, which cannot be attested. Retry once it exists."
                .into(),
        ),
        CircuitNodeKind::GithubAction { .. }
        | CircuitNodeKind::Notify { .. }
        | CircuitNodeKind::SetNodeStatus { .. }
        | CircuitNodeKind::CloseAgentNode { .. } => None,
        CircuitNodeKind::DeterministicVerification { .. } => Some(
            "A verification result cannot be attested. Retry the step to run it again.".into(),
        ),
        _ => Some("This step cannot be marked done by hand.".into()),
    }
}

/// Reasons the whole run cannot be reopened right now, whatever the step.
fn reopen_blocker(
    db: &Connection,
    run: &AutopilotCircuitRun,
    steps: &[AutopilotCircuitRunStep],
    graph: &CircuitGraph,
) -> Result<Option<String>, String> {
    if let Some(source) = run.source_agent_node_id {
        if let Some(other) =
            super::ledger::find_live_run_for_source_inner(db, source).map_err(|e| e.to_string())?
        {
            return Ok(Some(format!("Run #{other} is already using this agent.")));
        }
    }
    let closing: bool = db
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM autopilot_circuit_run_steps s
                JOIN agent_node_lifecycle_leases l ON l.node_id = s.agent_node_id
                JOIN agent_nodes a ON a.id = s.agent_node_id
                WHERE s.run_id = ?1 AND l.retired = 0 AND a.status != 'archived'
                  AND (l.cleanup_requested = 1 OR l.cleanup_generation IS NOT NULL))",
            [run.id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if closing {
        return Ok(Some(
            "Autopilot is still closing one of this run's agents. Try again in a moment.".into(),
        ));
    }
    // A step cancelled by the failure sweep is recreated when the run carries
    // on. If it had launched an agent that is still open, a second one would
    // start beside it.
    for step in steps.iter().filter(|step| {
        step.status == "cancelled"
            && step
                .error_message
                .as_deref()
                .unwrap_or("")
                .starts_with(SIBLING_SWEEP_NOTE)
    }) {
        let is_spawn = matches!(
            graph.node(&step.node_id).map(|node| &node.kind),
            Some(CircuitNodeKind::SpawnAgentNode { .. })
        );
        if is_spawn
            && matches!(
                agent_condition(db, step.agent_node_id)?,
                AgentCondition::Live
            )
        {
            return Ok(Some(format!(
                "The {} agent is still open from before the failure. Close it first.",
                step.node_id
            )));
        }
    }
    Ok(None)
}

/// What can be done about this run, or `None` when it is not a failed run with a
/// failed step (a review that simply did not approve has its own "Review again").
pub(crate) fn build_recovery(
    db: &Connection,
    run: &AutopilotCircuitRun,
    graph: &CircuitGraph,
    steps: &[AutopilotCircuitRunStep],
) -> Result<Option<RunRecovery>, String> {
    if run.state != "failed" {
        return Ok(None);
    }
    let Some(step) = problem_step(steps) else {
        return Ok(None);
    };
    let Some(node) = graph.node(&step.node_id) else {
        return Ok(None);
    };
    let view = super::evidence::recovery_view(db, run)?;
    let agent = step
        .agent_node_id
        .or_else(|| view.resolve_target_agent(&step.node_id));
    let condition = agent_condition(db, agent)?;
    let blocker = reopen_blocker(db, run, steps, graph)?;
    let option = |action: RecoveryAction, refusal: Option<String>| {
        let unavailable_reason = blocker.clone().or(refusal);
        RunRecoveryOption {
            action,
            available: unavailable_reason.is_none(),
            unavailable_reason,
        }
    };
    Ok(Some(RunRecovery {
        node_id: step.node_id.clone(),
        attempt: step.attempt,
        status: step.status.clone(),
        error: step.error_message.clone(),
        options: vec![
            option(RecoveryAction::Retry, retry_refusal(&node.kind, &condition)),
            option(
                RecoveryAction::Continue,
                continue_refusal(&node.kind, &condition),
            ),
        ],
    }))
}

/// Reopen a failed run at its failed step, as the person asked.
pub fn recover_failed_run(request: &RecoveryRequest) -> Result<(), String> {
    let mut db = crate::db::write_conn();
    recover_failed_run_locked(&mut db, request)
}

pub(crate) fn recover_failed_run_locked(
    db: &mut Connection,
    request: &RecoveryRequest,
) -> Result<(), String> {
    let reason = request.reason.trim();
    if request.action == RecoveryAction::Continue && reason.is_empty() {
        return Err("Say what you did to finish this step, so the history shows it.".into());
    }
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let revision: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(id),0) FROM circuit_run_history WHERE run_id=?1",
            [request.run_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if revision != request.expected_revision {
        return Err("The run changed. Refresh it before acting.".into());
    }
    let run = super::ledger::get_circuit_run_inner(&tx, request.run_id)
        .map_err(|e| e.to_string())?
        .ok_or("Run no longer exists.")?;
    if run.state != "failed" {
        return Err("Only a failed run can be recovered this way.".into());
    }
    let steps =
        super::ledger::list_circuit_run_steps_inner(&tx, run.id).map_err(|e| e.to_string())?;
    let graph = run_graph(&tx, run.id)?;
    let recovery = build_recovery(&tx, &run, &graph, &steps)?
        .filter(|recovery| {
            recovery.node_id == request.node_id && recovery.attempt == request.attempt
        })
        .ok_or("This failure changed. Refresh before acting.")?;
    let option = recovery
        .options
        .iter()
        .find(|option| option.action == request.action)
        .expect("both actions are always listed");
    if let Some(reason) = &option.unavailable_reason {
        return Err(reason.clone());
    }
    let kind = graph
        .node(&request.node_id)
        .map(|node| node.kind.clone())
        .ok_or("The step is no longer in this blueprint.")?;

    // The failed step's own working state is cleared so it is judged afresh;
    // every other step and every earlier history entry is kept.
    let mut context: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&run.context_json).map_err(|e| e.to_string())?;
    let own_prefix = format!("node.{}.", request.node_id);
    context.retain(|key, _| !key.starts_with(&own_prefix));
    context.insert("operator.recovered".into(), "1".into());

    match request.action {
        RecoveryAction::Retry => {
            // A spawn starts a new agent; the old association would mislabel it.
            let spawn = matches!(kind, CircuitNodeKind::SpawnAgentNode { .. });
            tx.execute(
                "UPDATE autopilot_circuit_run_steps
                 SET status = 'pending_slot', attempt = attempt + 1, outcome = NULL,
                     error_message = NULL, started_at = datetime('now'), completed_at = NULL,
                     agent_node_id = CASE WHEN ?3 THEN NULL ELSE agent_node_id END,
                     parent_agent_node_id = CASE WHEN ?3 THEN NULL ELSE parent_agent_node_id END
                 WHERE run_id = ?1 AND node_id = ?2",
                params![run.id, request.node_id, spawn],
            )
            .map_err(|e| e.to_string())?;
            let detail = if reason.is_empty() {
                "Operator retried this step as a new attempt.".to_string()
            } else {
                format!("Operator retried this step as a new attempt: {reason}")
            };
            append_history(
                &tx,
                run.id,
                Some(&request.node_id),
                Some(request.attempt + 1),
                "operator_retry",
                &detail,
                Some(SOURCE_OPERATOR),
                Some(DISPOSITION_APPLIED),
            )
            .map_err(|e| e.to_string())?;
        }
        RecoveryAction::Continue => {
            let effect_kind = match kind {
                CircuitNodeKind::InjectPty { .. } => Some("prompt"),
                CircuitNodeKind::GithubAction { .. } => Some("github"),
                _ => None,
            };
            if let Some(effect_kind) = effect_kind {
                tx.execute(
                    "INSERT INTO circuit_effects VALUES (?1,?2,?3,?4,'attested_completed')
                     ON CONFLICT(run_id,node_id,attempt,kind) DO UPDATE SET state='attested_completed'",
                    params![run.id, request.node_id, request.attempt, effect_kind],
                )
                .map_err(|e| e.to_string())?;
            }
            tx.execute(
                "UPDATE autopilot_circuit_run_steps
                 SET status = 'completed', outcome = 'completed', error_message = NULL,
                     completed_at = datetime('now')
                 WHERE run_id = ?1 AND node_id = ?2",
                params![run.id, request.node_id],
            )
            .map_err(|e| e.to_string())?;
            context.insert(
                format!("node.{}.status", request.node_id),
                "completed".into(),
            );
            append_history(
                &tx,
                run.id,
                Some(&request.node_id),
                Some(request.attempt),
                "operator_attestation",
                &format!("Operator-recorded outcome (Completed): {reason}"),
                Some(SOURCE_OPERATOR),
                Some("completed"),
            )
            .map_err(|e| e.to_string())?;
        }
    }

    // Steps the failure itself cancelled are recreated when they become eligible
    // again; left in place they would fail the run a second time at once.
    tx.execute(
        "DELETE FROM autopilot_circuit_run_steps
         WHERE run_id = ?1 AND node_id != ?2 AND status = 'cancelled'
           AND error_message LIKE ?3",
        params![run.id, request.node_id, format!("{SIBLING_SWEEP_NOTE}%")],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE autopilot_circuit_runs
         SET state = 'pending', context_json = ?2, updated_at = datetime('now'),
             queue_position = (SELECT COALESCE(MAX(queue_position), 0) + 1
                               FROM autopilot_circuit_runs WHERE mesh_id = ?3)
         WHERE id = ?1 AND state = 'failed'",
        params![
            run.id,
            serde_json::to_string(&context).map_err(|e| e.to_string())?,
            run.mesh_id
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuit::model::CircuitGraph;

    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        let graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run")
            .to_json()
            .unwrap();
        conn.execute(
            "INSERT INTO meshes (id, name, path) VALUES (1, 'm', '/repo')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO autopilot_circuits (id, mesh_id, name, graph_json) VALUES (1, 1, 'c', ?1)",
            [graph],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO agent_nodes (id, mesh_id, name, path, status) VALUES
                (10, 1, 'implementer', '/repo', 'ready'), (11, 1, 'reviewer', '/repo', 'ready');",
        )
        .unwrap();
        conn
    }

    fn add_run(conn: &Connection, id: i64, state: &str, source: Option<i64>) {
        conn.execute(
            "INSERT INTO autopilot_circuit_runs (id, circuit_id, mesh_id, trigger_identity, state, context_json, source_agent_node_id)
             VALUES (?1, 1, 1, ?2, ?3, ?4, ?5)",
            params![
                id,
                format!("issue:{id}"),
                state,
                r#"{"node.finish.output":"old","node.implementer.output":"kept","pr.number":"7"}"#,
                source
            ],
        )
        .unwrap();
    }

    fn add_step(
        conn: &Connection,
        run: i64,
        node: &str,
        status: &str,
        agent: Option<i64>,
        error: Option<&str>,
    ) {
        let outcome = match status {
            "completed" => Some("completed"),
            "failed" => Some("failed"),
            "cancelled" => Some("cancelled"),
            _ => None,
        };
        conn.execute(
            "INSERT INTO autopilot_circuit_run_steps (run_id, node_id, status, outcome, error_message, agent_node_id, attempt, started_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, datetime('now'), datetime('now'))",
            params![run, node, status, outcome, error, agent],
        )
        .unwrap();
    }

    /// The finish prompt could not be delivered to a live implementer.
    fn failed_finish_run(conn: &Connection) {
        add_run(conn, 1, "failed", None);
        add_step(conn, 1, "trigger", "completed", None, None);
        add_step(conn, 1, "implementer", "completed", Some(10), None);
        add_step(
            conn,
            1,
            "implementation_classifier",
            "completed",
            None,
            None,
        );
        add_step(
            conn,
            1,
            "finish",
            "failed",
            None,
            Some("Prompt delivery failed"),
        );
        add_step(
            conn,
            1,
            "finish_classifier",
            "cancelled",
            None,
            Some("Cancelled because the circuit run failed."),
        );
        append_history(
            conn,
            1,
            None,
            None,
            "run_transition",
            "failed",
            Some("circuit_worker"),
            Some("applied"),
        )
        .unwrap();
    }

    fn revision(conn: &Connection, run: i64) -> i64 {
        conn.query_row(
            "SELECT COALESCE(MAX(id),0) FROM circuit_run_history WHERE run_id=?1",
            [run],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn request(
        conn: &Connection,
        node: &str,
        action: RecoveryAction,
        reason: &str,
    ) -> RecoveryRequest {
        RecoveryRequest {
            run_id: 1,
            node_id: node.into(),
            attempt: 1,
            expected_revision: revision(conn, 1),
            action,
            reason: reason.into(),
        }
    }

    fn recover(
        conn: &mut Connection,
        node: &str,
        action: RecoveryAction,
        reason: &str,
    ) -> Result<(), String> {
        let request = request(conn, node, action, reason);
        recover_failed_run_locked(conn, &request)
    }

    fn recovery(conn: &Connection) -> Option<RunRecovery> {
        let run = super::super::ledger::get_circuit_run_inner(conn, 1)
            .unwrap()
            .unwrap();
        let graph = run_graph(conn, 1).unwrap();
        let steps = super::super::ledger::list_circuit_run_steps_inner(conn, 1).unwrap();
        build_recovery(conn, &run, &graph, &steps).unwrap()
    }

    fn option(recovery: &RunRecovery, action: RecoveryAction) -> &RunRecoveryOption {
        recovery
            .options
            .iter()
            .find(|option| option.action == action)
            .unwrap()
    }

    fn step_row(conn: &Connection, node: &str) -> (String, i32, Option<String>, Option<String>) {
        conn.query_row(
            "SELECT status, attempt, outcome, error_message FROM autopilot_circuit_run_steps WHERE run_id=1 AND node_id=?1",
            [node],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
    }

    fn run_state(conn: &Connection) -> String {
        conn.query_row(
            "SELECT state FROM autopilot_circuit_runs WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn a_failed_step_with_a_live_agent_offers_retry_and_continue() {
        let conn = fixture();
        failed_finish_run(&conn);

        let recovery = recovery(&conn).expect("a failed run with a failed step is recoverable");

        assert_eq!(recovery.node_id, "finish");
        assert_eq!(recovery.error.as_deref(), Some("Prompt delivery failed"));
        assert!(option(&recovery, RecoveryAction::Retry).available);
        assert!(option(&recovery, RecoveryAction::Continue).available);
    }

    #[test]
    fn retry_reopens_the_run_with_a_new_attempt_and_keeps_everything_else() {
        let mut conn = fixture();
        failed_finish_run(&conn);
        let before = revision(&conn, 1);

        recover(&mut conn, "finish", RecoveryAction::Retry, "").unwrap();

        assert_eq!(run_state(&conn), "pending", "the run re-enters the queue");
        let (status, attempt, outcome, error) = step_row(&conn, "finish");
        assert_eq!(
            (status.as_str(), attempt, outcome, error),
            ("pending_slot", 2, None, None)
        );
        assert_eq!(
            step_row(&conn, "implementer").0,
            "completed",
            "finished work is not redone"
        );
        let swept: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM autopilot_circuit_run_steps WHERE run_id=1 AND node_id='finish_classifier'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            swept, 0,
            "a step cancelled by the failure is recreated, not left to fail the run again"
        );

        let context: std::collections::BTreeMap<String, String> = serde_json::from_str(
            &conn
                .query_row(
                    "SELECT context_json FROM autopilot_circuit_runs WHERE id=1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
        )
        .unwrap();
        assert!(
            !context.contains_key("node.finish.output"),
            "the step is judged afresh"
        );
        assert_eq!(
            context.get("node.implementer.output").map(String::as_str),
            Some("kept")
        );
        assert_eq!(context.get("pr.number").map(String::as_str), Some("7"));
        assert_eq!(
            context.get("operator.recovered").map(String::as_str),
            Some("1")
        );

        let history: Vec<(String, Option<i32>, Option<String>)> = conn
            .prepare(
                "SELECT kind, attempt, source FROM circuit_run_history WHERE run_id=1 AND id > ?1",
            )
            .unwrap()
            .query_map([before], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            history,
            vec![("operator_retry".into(), Some(2), Some("operator".into()))]
        );
        let entries: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM circuit_run_history WHERE run_id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            entries,
            before + 1,
            "earlier history is untouched and one entry is added"
        );
    }

    #[test]
    fn continue_records_the_operators_word_and_completes_the_step() {
        let mut conn = fixture();
        failed_finish_run(&conn);

        let missing_reason =
            recover(&mut conn, "finish", RecoveryAction::Continue, "  ").unwrap_err();
        assert!(
            missing_reason.contains("Say what you did"),
            "{missing_reason}"
        );
        assert_eq!(run_state(&conn), "failed", "nothing changed");

        recover(
            &mut conn,
            "finish",
            RecoveryAction::Continue,
            "I pasted the finish prompt myself",
        )
        .unwrap();

        let (status, _, outcome, error) = step_row(&conn, "finish");
        assert_eq!(
            (status.as_str(), outcome.as_deref(), error),
            ("completed", Some("completed"), None)
        );
        assert_eq!(run_state(&conn), "pending");
        let detail: String = conn
            .query_row(
                "SELECT detail FROM circuit_run_history WHERE run_id=1 AND kind='operator_attestation'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(detail.contains("I pasted the finish prompt myself"));
        let effect: String = conn
            .query_row(
                "SELECT state FROM circuit_effects WHERE run_id=1 AND node_id='finish' AND kind='prompt'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(effect, "attested_completed");
    }

    #[test]
    fn a_closed_agent_blocks_retry_until_it_is_resumed() {
        let mut conn = fixture();
        add_run(&conn, 1, "failed", None);
        add_step(&conn, 1, "implementer", "completed", Some(10), None);
        add_step(
            &conn,
            1,
            "implementation_classifier",
            "cancelled",
            None,
            Some("piloted agent node was closed"),
        );
        conn.execute("UPDATE agent_nodes SET status='archived' WHERE id=10", [])
            .unwrap();

        let closed = recovery(&conn).unwrap();
        assert_eq!(closed.node_id, "implementation_classifier");
        assert_eq!(closed.status, "cancelled");
        let retry = option(&closed, RecoveryAction::Retry);
        assert!(!retry.available);
        assert!(retry
            .unavailable_reason
            .as_deref()
            .unwrap()
            .contains("Resume it from Archive"));
        assert!(
            option(&closed, RecoveryAction::Continue).available,
            "the person can still vouch for finished work"
        );
        let refused = recover(
            &mut conn,
            "implementation_classifier",
            RecoveryAction::Retry,
            "",
        )
        .unwrap_err();
        assert!(refused.contains("Resume it from Archive"));
        assert_eq!(run_state(&conn), "failed");

        conn.execute("UPDATE agent_nodes SET status='running' WHERE id=10", [])
            .unwrap();
        assert!(option(&recovery(&conn).unwrap(), RecoveryAction::Retry).available);
    }

    #[test]
    fn a_deleted_agent_cannot_be_retried_or_continued_with() {
        let conn = fixture();
        add_run(&conn, 1, "failed", None);
        add_step(&conn, 1, "implementer", "completed", Some(999), None);
        add_step(
            &conn,
            1,
            "implementation_classifier",
            "cancelled",
            None,
            Some("Piloted agent node 999 is no longer available (deleted, archived or missing)"),
        );

        let recovery = recovery(&conn).unwrap();
        for action in [RecoveryAction::Retry, RecoveryAction::Continue] {
            let option = option(&recovery, action);
            assert!(!option.available, "{action:?}");
            assert!(option
                .unavailable_reason
                .as_deref()
                .unwrap()
                .contains("no longer exists"));
        }
    }

    #[test]
    fn a_review_approval_can_never_be_recorded_on_the_reviewers_behalf() {
        let mut conn = fixture();
        add_run(&conn, 1, "failed", None);
        add_step(&conn, 1, "implementer", "completed", Some(10), None);
        add_step(&conn, 1, "reviewer", "completed", Some(11), None);
        add_step(
            &conn,
            1,
            "review_classifier",
            "failed",
            None,
            Some("classifier unavailable"),
        );

        let recovery = recovery(&conn).unwrap();
        assert_eq!(recovery.node_id, "review_classifier");
        let cont = option(&recovery, RecoveryAction::Continue);
        assert!(!cont.available);
        assert!(cont
            .unavailable_reason
            .as_deref()
            .unwrap()
            .contains("Review again"));
        assert!(
            option(&recovery, RecoveryAction::Retry).available,
            "re-reading the reviewer's report is fine"
        );

        let refused = recover(
            &mut conn,
            "review_classifier",
            RecoveryAction::Continue,
            "looks good to me",
        )
        .unwrap_err();
        assert!(refused.contains("Review again"));
        assert_eq!(run_state(&conn), "failed");
    }

    #[test]
    fn a_pull_request_lookup_and_a_spawn_cannot_be_attested_but_can_be_retried() {
        let conn = fixture();
        add_run(&conn, 1, "failed", None);
        add_step(&conn, 1, "implementer", "completed", Some(10), None);
        add_step(
            &conn,
            1,
            "open_pr",
            "failed",
            None,
            Some("no open pull request for the branch"),
        );
        let pr = recovery(&conn).unwrap();
        assert!(!option(&pr, RecoveryAction::Continue).available);
        assert!(option(&pr, RecoveryAction::Retry).available);

        conn.execute(
            "DELETE FROM autopilot_circuit_run_steps WHERE node_id='open_pr'",
            [],
        )
        .unwrap();
        add_step(&conn, 1, "reviewer", "failed", None, Some("spawn failed"));
        let spawn = recovery(&conn).unwrap();
        assert!(!option(&spawn, RecoveryAction::Continue).available);
        assert!(
            option(&spawn, RecoveryAction::Retry).available,
            "no agent is open for it, so it can start fresh"
        );
    }

    #[test]
    fn retrying_a_spawn_detaches_the_old_agent_so_the_new_one_is_not_mislabelled() {
        let mut conn = fixture();
        add_run(&conn, 1, "failed", None);
        add_step(&conn, 1, "implementer", "completed", Some(10), None);
        add_step(
            &conn,
            1,
            "reviewer",
            "failed",
            Some(11),
            Some("process exited"),
        );
        conn.execute("UPDATE agent_nodes SET status='archived' WHERE id=11", [])
            .unwrap();

        recover(&mut conn, "reviewer", RecoveryAction::Retry, "").unwrap();

        let agent: Option<i64> = conn
            .query_row(
                "SELECT agent_node_id FROM autopilot_circuit_run_steps WHERE run_id=1 AND node_id='reviewer'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(agent, None);
        assert_eq!(step_row(&conn, "reviewer").1, 2);
    }

    #[test]
    fn a_review_that_simply_did_not_approve_has_no_step_recovery() {
        let conn = fixture();
        add_run(&conn, 1, "failed", None);
        add_step(&conn, 1, "reviewer", "completed", Some(11), None);
        add_step(&conn, 1, "review_classifier", "completed", None, None);
        assert!(
            recovery(&conn).is_none(),
            "that case has its own Review again"
        );
    }

    #[test]
    fn only_a_failed_run_can_be_recovered_and_a_stale_click_is_refused() {
        let mut conn = fixture();
        failed_finish_run(&conn);
        let stale = request(&conn, "finish", RecoveryAction::Retry, "");
        append_history(&conn, 1, None, None, "observation", "{}", None, None).unwrap();
        let error = recover_failed_run_locked(&mut conn, &stale).unwrap_err();
        assert!(error.contains("changed"), "{error}");

        for state in ["running", "completed", "cancelled", "pending"] {
            conn.execute(
                "UPDATE autopilot_circuit_runs SET state=?1 WHERE id=1",
                [state],
            )
            .unwrap();
            let error = recover(&mut conn, "finish", RecoveryAction::Retry, "").unwrap_err();
            assert!(error.contains("Only a failed run"), "{state}: {error}");
        }

        conn.execute(
            "UPDATE autopilot_circuit_runs SET state='failed' WHERE id=1",
            [],
        )
        .unwrap();
        let mut elsewhere = request(&conn, "finish_classifier", RecoveryAction::Retry, "");
        elsewhere.node_id = "finish_classifier".into();
        let error = recover_failed_run_locked(&mut conn, &elsewhere).unwrap_err();
        assert!(error.contains("This failure changed"), "{error}");
    }

    #[test]
    fn the_run_is_not_reopened_while_another_run_uses_its_source_or_an_agent_is_still_closing() {
        let conn = fixture();
        add_run(&conn, 1, "failed", Some(10));
        add_step(&conn, 1, "implementer", "completed", Some(10), None);
        add_step(&conn, 1, "finish", "failed", None, Some("boom"));
        conn.execute(
            "INSERT INTO autopilot_circuit_runs (id, circuit_id, mesh_id, trigger_identity, state, context_json, source_agent_node_id)
             VALUES (2, 1, 1, 'issue:2', 'running', '{}', 10)",
            [],
        )
        .unwrap();
        let busy = recovery(&conn).unwrap();
        for option in &busy.options {
            assert!(option
                .unavailable_reason
                .as_deref()
                .unwrap()
                .contains("Run #2"));
        }

        conn.execute("DELETE FROM autopilot_circuit_runs WHERE id=2", [])
            .unwrap();
        conn.execute(
            "INSERT INTO agent_node_lifecycle_leases (node_id, cleanup_requested) VALUES (10, 1)",
            [],
        )
        .unwrap();
        let closing = recovery(&conn).unwrap();
        assert!(closing.options.iter().all(|option| option
            .unavailable_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("still closing"))));
    }

    #[test]
    fn the_attention_view_carries_the_recovery_and_the_revision_to_act_against() {
        let conn = fixture();
        failed_finish_run(&conn);

        let attention = super::super::evidence::attention_inner(&conn, 1).unwrap();

        assert_eq!(attention.revision, revision(&conn, 1));
        assert!(
            attention.checkpoints.is_empty(),
            "a failed run has no live checkpoint"
        );
        let recovery = attention
            .recovery
            .expect("the failed step is offered for recovery");
        assert_eq!(recovery.node_id, "finish");
        let json = serde_json::to_value(&recovery).unwrap();
        assert_eq!(json["options"][0]["action"], "retry");
        assert_eq!(json["options"][1]["action"], "continue");
        assert!(
            json["options"][0].get("unavailable_reason").is_none(),
            "an available option carries no refusal"
        );
    }

    #[test]
    fn the_attention_view_lists_unverified_steps_with_their_allowed_actions_for_a_running_run() {
        let conn = fixture();
        add_run(&conn, 1, "running", None);
        add_step(&conn, 1, "implementer", "completed", Some(10), None);
        add_step(
            &conn,
            1,
            "finish",
            "unverified",
            Some(10),
            Some("Prompt delivery is unverified"),
        );

        let attention = super::super::evidence::attention_inner(&conn, 1).unwrap();

        assert!(
            attention.recovery.is_none(),
            "recovery is only for a failed run"
        );
        assert_eq!(attention.checkpoints.len(), 1);
        assert_eq!(attention.checkpoints[0].node_id, "finish");
        assert!(!attention.checkpoints[0].actions.is_empty());
    }

    #[test]
    fn a_still_open_agent_from_a_swept_spawn_blocks_reopening() {
        let conn = fixture();
        add_run(&conn, 1, "failed", None);
        add_step(&conn, 1, "implementer", "completed", Some(10), None);
        add_step(&conn, 1, "finish", "failed", None, Some("boom"));
        add_step(
            &conn,
            1,
            "reviewer",
            "cancelled",
            Some(11),
            Some("Cancelled because the circuit run failed."),
        );

        let recovery = recovery(&conn).unwrap();
        for option in &recovery.options {
            let reason = option.unavailable_reason.as_deref().unwrap();
            assert!(
                reason.contains("reviewer") && reason.contains("Close it first"),
                "{reason}"
            );
        }
    }
}
