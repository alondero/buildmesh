//! Review extensions borrow the retained implementation agent and reuse its
//! Circuit Run. Earlier findings and replaced graph snapshots remain archived.

use crate::circuit::lineage::{
    RecoveryAncestry, RunLineage, RECOVERY_PARENT_KEY, REVIEW_EXTENDED_KEY,
};
use crate::circuit::model::{CircuitGraph, CircuitNodeKind};
use crate::circuit::review_continuation::{
    ReviewContinuation, ReviewContinuationRefusal as Refusal, ReviewRunNodes,
};
use crate::circuit::stepper::{resolve_target_agent, StepStatus, StepView};
use crate::db::DbError;
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug)]
enum EligibilityError {
    Refused(Refusal),
    Database(DbError),
}
impl From<Refusal> for EligibilityError {
    fn from(value: Refusal) -> Self {
        Self::Refused(value)
    }
}
impl From<DbError> for EligibilityError {
    fn from(value: DbError) -> Self {
        Self::Database(value)
    }
}
impl From<rusqlite::Error> for EligibilityError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Database(value.into())
    }
}
impl From<String> for EligibilityError {
    fn from(_: String) -> Self {
        Self::Refused(Refusal::InvalidGraph)
    }
}
impl From<EligibilityError> for DbError {
    fn from(value: EligibilityError) -> Self {
        match value {
            EligibilityError::Database(error) => error,
            EligibilityError::Refused(reason) => match reason {
                Refusal::RunMissing | Refusal::SourceMissing => {
                    DbError::not_found(reason.message())
                }
                Refusal::RunNotFailed
                | Refusal::UnsupportedGraph
                | Refusal::BeforeReview
                | Refusal::ReviewerDefinitionMissing
                | Refusal::RoundLimitMissing
                | Refusal::InvalidGraph
                | Refusal::InvalidContext => DbError::invalid(reason.message()),
                _ => DbError::conflict(reason.message()),
            },
        }
    }
}

#[derive(Debug)]
pub(crate) struct ReviewRecovery {
    pub run_id: i64,
    pub source_id: i64,
    pub graph: CircuitGraph,
    pub name: String,
    pub frozen_launches: Vec<(String, String)>,
}

pub(crate) enum ContinuationTarget {
    Existing(i64),
    Failed(i64),
}

/// Review findings and the conversation remain available until approval or
/// explicit disposal. Other helpers still follow the ordinary failure cleanup.
pub(crate) fn retained_reviewers_inner(
    db: &Connection,
    run_id: i64,
) -> crate::db::SqlResult<Vec<i64>> {
    let json = super::evidence::run_graph_json(db, run_id)?;
    let Ok(graph) = CircuitGraph::from_json(&json) else {
        return Ok(Vec::new());
    };
    let steps: Vec<StepView> = super::ledger::list_circuit_run_steps_inner(db, run_id)?
        .into_iter()
        .map(|step| StepView {
            node_id: step.node_id,
            agent_node_id: step.agent_node_id,
            status: StepStatus::from_db_str(&step.status),
            outcome: step
                .outcome
                .as_deref()
                .and_then(crate::circuit::model::StepOutcome::from_db_str),
            error: step.error_message,
            attempt: step.attempt,
        })
        .collect();
    Ok(graph
        .nodes
        .iter()
        .filter(|node| matches!(node.kind, CircuitNodeKind::ReviewVerdict { .. }))
        .filter_map(|node| resolve_target_agent(&graph, &steps, &node.id))
        .collect())
}

/// Resolve the retained reviewer before resuming a stopped process. The
/// extension transaction checks its lifecycle lease again before attaching it.
pub fn review_recovery_reviewer(run_id: i64) -> Result<Option<i64>, DbError> {
    let db = crate::db::read_conn()?;
    let target = match continuation_target_inner(&db, run_id)? {
        ContinuationTarget::Existing(_) => return Ok(None),
        ContinuationTarget::Failed(id) => id,
    };
    retained_reviewer_inner(&db, target).map_err(Into::into)
}

fn retained_reviewer_inner(db: &Connection, run_id: i64) -> Result<Option<i64>, EligibilityError> {
    let reviewer: Option<(i64, bool)> = db
        .query_row(
            "SELECT a.id, COALESCE(l.cleanup_requested,0) = 1 OR l.cleanup_generation IS NOT NULL
         FROM autopilot_circuit_run_steps s JOIN agent_nodes a ON a.id=s.agent_node_id
         LEFT JOIN agent_node_lifecycle_leases l ON l.node_id=a.id
         WHERE s.run_id=?1 AND s.node_id='reviewer' AND a.status != 'archived'",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match reviewer {
        Some((_, true)) => Err(Refusal::ReviewerStopping.into()),
        Some((id, false)) => {
            let piloted_elsewhere: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM autopilot_circuit_runs r
                 WHERE r.id != ?1 AND r.state IN ('pending','running','paused')
                   AND (r.source_agent_node_id=?2 OR EXISTS(SELECT 1 FROM autopilot_circuit_run_steps s
                       WHERE s.run_id=r.id AND s.agent_node_id=?2)))",
                params![run_id, id], |row| row.get(0))?;
            if piloted_elsewhere {
                return Err(Refusal::ReviewerControlledElsewhere.into());
            }
            Ok(Some(id))
        }
        None => Ok(None),
    }
}

fn is_review_extension(state: &str, context_json: &str) -> bool {
    let stored = super::context_store::StoredContext::of_column(context_json);
    matches!(state, "pending" | "running" | "paused" | "completed")
        && RunLineage::is_extended(|key| stored.get(key))
}

pub fn existing_review_target(run_id: i64) -> Result<Option<i64>, DbError> {
    let db = crate::db::read_conn()?;
    let run = super::ledger::get_circuit_run_inner(&db, run_id)?
        .ok_or_else(|| DbError::not_found("This run no longer exists."))?;
    if is_review_extension(&run.state, &run.context_json) {
        return Ok(Some(run_id));
    }
    Ok(match continuation_target_inner(&db, run_id)? {
        ContinuationTarget::Existing(id) => Some(id),
        ContinuationTarget::Failed(_) => None,
    })
}

/// One snapshot batches the compatibility ancestry lookup for every displayed
/// run. Only extracted parent identities are read, not whole run contexts.
pub(crate) struct ReviewContinuationReader<'a> {
    db: &'a Connection,
    successors: std::cell::RefCell<Option<std::collections::HashMap<String, (i64, String)>>>,
}
impl<'a> ReviewContinuationReader<'a> {
    pub(crate) fn new(db: &'a Connection) -> Self {
        Self {
            db,
            successors: std::cell::RefCell::new(None),
        }
    }

    fn successor(&self, parent: i64) -> Result<Option<(i64, String)>, EligibilityError> {
        if self.successors.borrow().is_none() {
            let mut stmt = self.db.prepare(
                "SELECT id,state,json_extract(context_json,?1)
                FROM autopilot_circuit_runs WHERE json_valid(context_json)
                AND json_type(context_json,?1)='text' ORDER BY id",
            )?;
            let rows = stmt.query_map([format!("$.\"{RECOVERY_PARENT_KEY}\"")], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            let mut successors = std::collections::HashMap::new();
            for row in rows {
                let (id, state, parent) = row?;
                // Preserve the durable operation's first-successor selection.
                successors.entry(parent).or_insert((id, state));
            }
            *self.successors.borrow_mut() = Some(successors);
        }
        Ok(self
            .successors
            .borrow()
            .as_ref()
            .and_then(|rows| rows.get(&parent.to_string()).cloned()))
    }

    pub(crate) fn project(
        &self,
        run: &crate::models::AutopilotCircuitRun,
        steps: &[crate::models::AutopilotCircuitRunStep],
    ) -> Result<ReviewContinuation, DbError> {
        project_continuation(self, run, steps)
    }
}

/// Follow recorded generations under the caller's connection. Repeated
/// requests for an older failure must never create a sibling review.
pub(crate) fn continuation_target_inner(
    db: &Connection,
    run_id: i64,
) -> Result<ContinuationTarget, DbError> {
    continuation_target_checked(&ReviewContinuationReader::new(db), run_id).map_err(Into::into)
}

fn continuation_target_checked(
    reader: &ReviewContinuationReader<'_>,
    run_id: i64,
) -> Result<ContinuationTarget, EligibilityError> {
    let db = reader.db;
    let original = super::ledger::get_circuit_run_inner(db, run_id)?
        .ok_or_else(|| EligibilityError::Refused(Refusal::RunMissing))?;
    if original.state != "failed" {
        return Err(Refusal::RunNotFailed.into());
    }
    let mut current = run_id;
    let mut visited = std::collections::HashSet::new();
    loop {
        if !visited.insert(current) {
            return Err(Refusal::InvalidAncestry.into());
        }
        let next = reader.successor(current)?;
        match next {
            None => return Ok(ContinuationTarget::Failed(current)),
            Some((id, state)) if state == "failed" => current = id,
            Some((id, state))
                if matches!(
                    state.as_str(),
                    "pending" | "running" | "paused" | "completed"
                ) =>
            {
                return Ok(ContinuationTarget::Existing(id))
            }
            Some(_) => return Err(Refusal::CancelledSuccessor.into()),
        }
    }
}

pub(crate) fn review_recovery_inner(
    db: &Connection,
    run_id: i64,
    rounds: i32,
) -> Result<ReviewRecovery, DbError> {
    review_recovery_checked(db, run_id, rounds).map_err(Into::into)
}

fn review_recovery_checked(
    db: &Connection,
    run_id: i64,
    rounds: i32,
) -> Result<ReviewRecovery, EligibilityError> {
    if !(1..=10000).contains(&rounds) {
        return Err(Refusal::RoundLimitReached.into());
    }
    let run = super::ledger::get_circuit_run_inner(db, run_id)?
        .ok_or_else(|| EligibilityError::Refused(Refusal::RunMissing))?;
    if run.state != "failed" {
        return Err(Refusal::RunNotFailed.into());
    }
    let context = super::context_store::load_logical(db, run_id)?.runtime();
    let original = super::evidence::run_graph(db, run_id)?;
    let steps = super::ledger::list_circuit_run_steps_inner(db, run_id)?;
    let (source_id, local) = review_source_id(&run, &original, &steps)?;
    let source = crate::db::agent_node::get_agent_node_by_id_inner(db, source_id)
        .optional()?
        .ok_or(Refusal::SourceMissing)?;
    if source.mesh_id != run.mesh_id {
        return Err(Refusal::SourceWrongMesh.into());
    }
    let mut reviewer = original
        .node("reviewer")
        .ok_or_else(|| EligibilityError::Refused(Refusal::ReviewerDefinitionMissing))?
        .kind
        .clone();
    let CircuitNodeKind::SpawnAgentNode {
        prompt,
        provider,
        model,
        effort,
        ..
    } = &mut reviewer
    else {
        return Err(Refusal::ReviewerDefinitionMissing.into());
    };
    // Freeze the original review scope, including PR comment instructions and
    // custom reviewer prompts. Do not replay implementation or publish steps.
    let mut prompt_context = context.clone();
    for key in [
        "source.output",
        "retry.attempt",
        "retry.max_retries",
        "node.reviewer.output",
        "circuit.run_id",
    ] {
        prompt_context.set(key, format!("{{{{{key}}}}}"));
    }
    *prompt = prompt_context.resolve(prompt);
    if crate::circuit::lineage::RunLineage::decode(|key| context.get(key)).is_local_preset() {
        *model = None;
        *effort = None;
        *provider = None;
    }
    *provider = provider
        .clone()
        .filter(|p| !p.trim().is_empty())
        .or_else(|| {
            context
                .get("review.provider")
                .filter(|p| !p.trim().is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            Some(
                source
                    .launch_configuration
                    .as_ref()
                    .map_or_else(|| source.provider.clone(), |c| c.id.clone()),
            )
        });
    let mut graph = CircuitGraph::agent_review(None, None, rounds);
    graph
        .nodes
        .iter_mut()
        .find(|n| n.id == "reviewer")
        .unwrap()
        .kind = reviewer;
    let feedback_id = if local { "feedback" } else { "follow_feedback" };
    if let Some(CircuitNodeKind::InjectPty { prompt, .. }) =
        original.node(feedback_id).map(|n| &n.kind)
    {
        if let CircuitNodeKind::InjectPty {
            prompt: next_prompt,
            ..
        } = &mut graph
            .nodes
            .iter_mut()
            .find(|n| n.id == "feedback")
            .unwrap()
            .kind
        {
            *next_prompt = prompt_context.resolve(prompt);
        }
    } else {
        return Err(Refusal::FeedbackChanged.into());
    }
    graph.validate()?;
    let frozen_launches = context
        .get("review.launch.reviewer")
        .map(|value| vec![("review.launch.reviewer".into(), value.into())])
        .unwrap_or_default();
    Ok(ReviewRecovery {
        run_id,
        source_id,
        graph,
        name: "Continued review".into(),
        frozen_launches,
    })
}

/// Whether Review again accepts this graph.
///
/// This is graph support only. Retained agents and current state are checked
/// by the shared extension preparation. Last-round guidance can use support
/// while the run is still going without claiming it can execute yet.
pub(crate) fn review_continuation_supported(
    run: &crate::models::AutopilotCircuitRun,
    graph: &CircuitGraph,
) -> bool {
    if graph.has_local_review_contract() {
        // A borrowed-source graph needs its run binding to identify the work.
        // This is independent of run state and whether that agent still exists.
        return run.source_agent_node_id.is_some();
    }
    if !graph.is_issue_driven_autopilot_review() {
        return false;
    }
    let canonical = CircuitGraph::issue_driven_autopilot_review("");
    if graph.has_review_topology_of(&canonical) {
        return true;
    }
    let mut compatible = graph.clone();
    crate::circuit::stored_graph::upgrade(
        &mut compatible,
        crate::circuit::stored_graph::UpgradeTarget::Blueprint,
    );
    compatible.has_review_topology_of(&canonical)
}

fn review_source_id(
    run: &crate::models::AutopilotCircuitRun,
    original: &CircuitGraph,
    steps: &[crate::models::AutopilotCircuitRunStep],
) -> Result<(i64, bool), EligibilityError> {
    if run.state != "failed" {
        return Err(Refusal::RunNotFailed.into());
    }
    if original
        .nodes
        .iter()
        .any(|node| matches!(node.kind, CircuitNodeKind::ReviewVerdict { .. }))
        && !matches!(
            original.node("reviewer").map(|node| &node.kind),
            Some(CircuitNodeKind::SpawnAgentNode { .. })
        )
    {
        return Err(Refusal::ReviewerDefinitionMissing.into());
    }
    let local = original.has_local_review_contract();
    if local && run.source_agent_node_id.is_none() {
        return Err(Refusal::SourceMissing.into());
    }
    if !review_continuation_supported(run, original) {
        return Err(Refusal::UnsupportedGraph.into());
    }
    let source_id = if local {
        run.source_agent_node_id
    } else {
        steps
            .iter()
            .find(|step| step.node_id == "implementer")
            .and_then(|step| step.agent_node_id)
    }
    .ok_or_else(|| EligibilityError::Refused(Refusal::SourceMissing))?;
    if !steps.iter().any(|step| {
        original
            .node(&step.node_id)
            .is_some_and(|node| matches!(node.kind, CircuitNodeKind::ReviewVerdict { .. }))
    }) {
        return Err(Refusal::BeforeReview.into());
    }
    let feedback_id = if local { "feedback" } else { "follow_feedback" };
    if !matches!(
        original.node(feedback_id).map(|node| &node.kind),
        Some(CircuitNodeKind::InjectPty { .. })
    ) {
        return Err(Refusal::FeedbackChanged.into());
    }
    Ok((source_id, local))
}

pub fn review_recovery_source(run_id: i64) -> Result<i64, DbError> {
    let db = crate::db::read_conn()?;
    if let Some(run) = super::ledger::get_circuit_run_inner(&db, run_id)? {
        if is_review_extension(&run.state, &run.context_json) {
            return run.source_agent_node_id.ok_or_else(|| {
                DbError::not_found("The implementation agent is no longer retained.")
            });
        }
    }
    let parent = match continuation_target_inner(&db, run_id)? {
        ContinuationTarget::Failed(id) => id,
        ContinuationTarget::Existing(_) => run_id,
    };
    let run = super::ledger::get_circuit_run_inner(&db, parent)?
        .ok_or_else(|| DbError::not_found("This run no longer exists."))?;
    let graph = super::evidence::run_graph(&db, parent)?;
    let steps = super::ledger::list_circuit_run_steps_inner(&db, parent)?;
    let (source_id, _) = review_source_id(&run, &graph, &steps)?;
    let source = crate::db::agent_node::get_agent_node_by_id_inner(&db, source_id).map_err(|_| {
        DbError::not_found(
            "The implementation agent is no longer retained. Recover from the PR branch and start a new review.",
        )
    })?;
    if source.mesh_id != run.mesh_id {
        return Err(DbError::conflict(
            "The implementation agent no longer belongs to this Mesh.",
        ));
    }
    Ok(source_id)
}

/// Re-mint a successor review run for a failed one.
///
/// The user-facing path extends the failed run in place (see
/// [`extend_failed_review`]); only the dev-gated review-continuation fixture
/// mints the successor, so a shipped build drops this.
#[cfg(any(debug_assertions, feature = "dev-tools"))]
pub fn continue_failed_review(run_id: i64, rounds: i32) -> Result<i64, DbError> {
    let mut db = crate::db::write_conn();
    let recovery = review_recovery_inner(&db, run_id, rounds)?;
    super::ledger::create_node_circuit_run_recovery_locked(&mut db, recovery, rounds)
}

/// Extend the allowance of one failed review and queue its next reviewer
/// attempt on the same run. The append-only history keeps the earlier rounds;
/// the step rows are only the engine's current projection.
pub fn extend_failed_review(run_id: i64, additional_rounds: i32) -> Result<i64, DbError> {
    let mut db = crate::db::write_conn();
    extend_failed_review_locked(&mut db, run_id, additional_rounds)
}

pub(crate) fn extend_failed_review_locked(
    db: &mut Connection,
    run_id: i64,
    additional_rounds: i32,
) -> Result<i64, DbError> {
    if !(1..=10).contains(&additional_rounds) {
        return Err(DbError::invalid(
            "Additional review rounds must be between 1 and 10.",
        ));
    }
    let tx = db.transaction()?;
    let plan = match extension_plan_inner(
        &ReviewContinuationReader::new(&tx),
        run_id,
        additional_rounds,
    )? {
        ExtensionPlan::Existing(id) => return Ok(id),
        ExtensionPlan::Extend(plan) => plan,
    };
    let PreparedExtension {
        target,
        run,
        original_graph_json,
        next_attempt,
        next_limit,
        recovery,
        retained_reviewer,
        root_run,
        circuit_name,
    } = *plan;
    // The extension keeps the run's trigger context but starts the
    // reviewer's working state afresh: the node-scoped and evidence-scoped
    // values go with it (issue #2601). The values the extension does keep —
    // the source's report, the trigger body — stay.
    let mut mutation = super::context_store::ContextMutation::new();
    mutation.reset(super::context_store::ContextScope::segment("node"));
    mutation.reset(super::context_store::ContextScope::segment("evidence"));
    mutation.remove("cleanup.pending");
    mutation.put("retry.attempt", next_attempt.to_string());
    mutation.put("retry.max_retries", next_limit.to_string());
    mutation.put("review.round_limit", next_limit.to_string());
    mutation.put(REVIEW_EXTENDED_KEY, "1");
    if run.source_agent_node_id.is_none() {
        mutation.put("review.source_was_circuit_owned", "1");
    }
    mutation.put("circuit.id", root_run.circuit_id.to_string());
    mutation.put("circuit.name", circuit_name);
    mutation.put("source.agent_id", recovery.source_id.to_string());
    let graph_json = recovery.graph.to_json()?;
    use sha2::Digest;
    let graph_sha256 = hex::encode(sha2::Sha256::digest(graph_json.as_bytes()));
    tx.execute(
        "INSERT OR IGNORE INTO circuit_run_snapshot_history (run_id,attempt,graph_json)
        VALUES (?1,?2,?3)",
        params![target, next_attempt, original_graph_json],
    )?;
    tx.execute(
        "INSERT INTO circuit_run_snapshots (run_id,graph_json,behavior_revision) VALUES (?1,?2,1)
        ON CONFLICT(run_id) DO UPDATE SET graph_json=excluded.graph_json,
            behavior_revision=excluded.behavior_revision",
        params![target, graph_json],
    )?;
    tx.execute(
        "DELETE FROM autopilot_circuit_run_steps WHERE run_id=?1",
        [target],
    )?;
    // An extension reviews the retained work as it stands: the source is not
    // asked to publish again before the queued reviewer.
    for node in [
        "trigger",
        "await_source",
        "source_ready",
        "publish",
        "await_publish",
        "publish_ready",
    ] {
        tx.execute("INSERT INTO autopilot_circuit_run_steps (run_id,node_id,status,attempt,outcome,started_at,completed_at)
            VALUES (?1,?2,'completed',?3,'completed',datetime('now'),datetime('now'))",
            params![target, node, next_attempt])?;
    }
    if let Some(reviewer_id) = retained_reviewer {
        tx.execute("INSERT INTO autopilot_circuit_run_steps
            (run_id,node_id,status,attempt,outcome,agent_node_id,parent_agent_node_id,started_at,completed_at)
            VALUES (?1,'reviewer','completed',?2,'completed',?3,?4,datetime('now'),datetime('now'))",
            params![target, next_attempt, reviewer_id, recovery.source_id])?;
        tx.execute(
            "INSERT INTO autopilot_circuit_run_steps (run_id,node_id,status,attempt)
            VALUES (?1,'re_review','pending_slot',?2)",
            params![target, next_attempt],
        )?;
    } else {
        // Older builds already retired the reviewer. Those runs can still
        // continue, but necessarily start a new conversation.
        tx.execute("INSERT INTO autopilot_circuit_run_steps (run_id,node_id,status,attempt) VALUES (?1,'reviewer','pending_slot',?2)",
            params![target, next_attempt])?;
    }
    // The scoped resets clear the reviewer's working state in both halves
    // of the storage: a leftover value would come back on the next read.
    let applied = super::context_store::apply_mutation(&tx, target, &mutation)?;
    tx.execute("UPDATE autopilot_circuit_runs SET circuit_id=?2, source_agent_node_id=?3, state='pending',
        context_json=?4, queue_position=(SELECT COALESCE(MAX(queue_position),0)+1 FROM autopilot_circuit_runs WHERE mesh_id=?5),
        updated_at=datetime('now') WHERE id=?1 AND state='failed'",
        params![target, root_run.circuit_id, recovery.source_id, applied.column_json(), run.mesh_id])
        ?;
    super::evidence::append_history(&tx, super::evidence::HistoryEvent {
        run_id: target,
        node_id: Some("reviewer"),
        attempt: Some(next_attempt),
        kind: "review_extension",
        detail: &serde_json::json!({"additional_rounds":additional_rounds,"round_limit":next_limit,"attempt":next_attempt,"graph_sha256":graph_sha256}).to_string(),
        source: Some(super::evidence::SOURCE_OPERATOR),
        disposition: Some(super::evidence::DISPOSITION_APPLIED),
    })?;
    tx.commit()?;
    Ok(target)
}

enum ExtensionPlan {
    Existing(i64),
    Extend(Box<PreparedExtension>),
}
struct PreparedExtension {
    target: i64,
    run: crate::models::AutopilotCircuitRun,
    original_graph_json: String,
    next_attempt: i32,
    next_limit: i32,
    recovery: ReviewRecovery,
    retained_reviewer: Option<i64>,
    root_run: crate::models::AutopilotCircuitRun,
    circuit_name: String,
}

fn extension_plan_inner(
    reader: &ReviewContinuationReader<'_>,
    run_id: i64,
    additional_rounds: i32,
) -> Result<ExtensionPlan, EligibilityError> {
    let db = reader.db;
    if let Some(run) = super::ledger::get_circuit_run_inner(db, run_id)? {
        if is_review_extension(&run.state, &run.context_json) {
            return Ok(ExtensionPlan::Existing(run_id));
        }
    }
    let target = match continuation_target_checked(reader, run_id)? {
        ContinuationTarget::Existing(id) => return Ok(ExtensionPlan::Existing(id)),
        ContinuationTarget::Failed(id) => id,
    };
    let run = super::ledger::get_circuit_run_inner(db, target)?
        .ok_or_else(|| EligibilityError::Refused(Refusal::RunMissing))?;
    let original_graph_json = super::evidence::run_graph_json(db, target)?;
    let graph = CircuitGraph::from_json(&original_graph_json)?;
    let steps = super::ledger::list_circuit_run_steps_inner(db, target)?;
    let prior_limit = ["review_retry", "retry"]
        .iter()
        .find_map(|id| match graph.node(id).map(|node| &node.kind) {
            Some(CircuitNodeKind::RetryLimit { max_retries }) => Some(*max_retries),
            _ => None,
        })
        .ok_or_else(|| EligibilityError::Refused(Refusal::RoundLimitMissing))?;
    let last_attempt = steps
        .iter()
        .filter(|step| {
            matches!(
                step.node_id.as_str(),
                "reviewer" | "verdict" | "review_classifier" | "re_review"
            )
        })
        .map(|step| step.attempt)
        .max()
        .unwrap_or(0);
    let next_attempt = last_attempt
        .checked_add(1)
        .ok_or_else(|| EligibilityError::Refused(Refusal::RoundLimitReached))?;
    let context = super::context_store::StoredContext::strict_of_column(&run.context_json)
        .map_err(|_| Refusal::InvalidContext)?;
    let prior_limit = context
        .get("review.round_limit")
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|limit| *limit > 0)
        .unwrap_or(prior_limit);
    let next_limit = prior_limit
        .max(last_attempt)
        .checked_add(additional_rounds)
        .filter(|limit| *limit <= 10000)
        .ok_or_else(|| EligibilityError::Refused(Refusal::RoundLimitReached))?;
    let mut recovery = review_recovery_checked(db, target, next_limit)?;
    let other_live = super::ledger::find_live_run_for_source_inner(db, recovery.source_id)?;
    if let Some(id) = other_live {
        return Ok(ExtensionPlan::Existing(id));
    }
    let cleanup_in_progress: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_node_lifecycle_leases WHERE node_id=?1 AND cleanup_generation IS NOT NULL)",
        [recovery.source_id], |row| row.get(0))?;
    if cleanup_in_progress {
        return Err(Refusal::SourceStopping.into());
    }
    let retained_reviewer = retained_reviewer_inner(db, target)?;
    if retained_reviewer.is_some() {
        // The completed spawn supplies identity, not a fresh report. Route
        // through delivery before the join can schedule a verdict.
        recovery
            .graph
            .retarget_edge("reviewer", "review_round", "re_review");
        recovery.graph.validate()?;
    }

    // Old continuation runs are moved back under their original Circuit.
    // Their pinned review-only snapshot remains the executable graph.
    let root_run = lineage_root_inner(db, target)?;
    let circuit_name: String = db.query_row(
        "SELECT name FROM autopilot_circuits WHERE id=?1",
        [root_run.circuit_id],
        |row| row.get(0),
    )?;
    Ok(ExtensionPlan::Extend(Box::new(PreparedExtension {
        target,
        run,
        original_graph_json,
        next_attempt,
        next_limit,
        recovery,
        retained_reviewer,
        root_run,
        circuit_name,
    })))
}

fn lineage_root_inner(
    db: &Connection,
    run_id: i64,
) -> Result<crate::models::AutopilotCircuitRun, EligibilityError> {
    let mut root = run_id;
    let mut visited = std::collections::HashSet::new();
    loop {
        if !visited.insert(root) {
            return Err(Refusal::InvalidAncestry.into());
        }
        let current = super::ledger::get_circuit_run_inner(db, root)?
            .ok_or_else(|| EligibilityError::Refused(Refusal::MissingAncestor))?;
        let context = super::context_store::StoredContext::strict_of_column(&current.context_json)
            .map_err(|_| Refusal::InvalidContext)?;
        match RunLineage::recovery_ancestry(|key| context.get(key)) {
            RecoveryAncestry::FromRun { run_id } => root = run_id,
            RecoveryAncestry::None => break,
            RecoveryAncestry::Invalid => return Err(Refusal::InvalidAncestry.into()),
        }
    }
    super::ledger::get_circuit_run_inner(db, root)?
        .ok_or_else(|| EligibilityError::Refused(Refusal::MissingAncestor))
}

/// Read-only projection of the same preparation the mutation repeats under its
/// transaction. Operational DB failures stay errors, rather than false refusals.
pub(crate) fn review_continuation_inner(
    db: &Connection,
    run: &crate::models::AutopilotCircuitRun,
    steps: &[crate::models::AutopilotCircuitRunStep],
) -> Result<ReviewContinuation, DbError> {
    ReviewContinuationReader::new(db).project(run, steps)
}

fn project_continuation(
    reader: &ReviewContinuationReader<'_>,
    run: &crate::models::AutopilotCircuitRun,
    steps: &[crate::models::AutopilotCircuitRunStep],
) -> Result<ReviewContinuation, DbError> {
    let db = reader.db;
    let stored = super::context_store::StoredContext::of_column(&run.context_json);
    let graph_json = super::evidence::run_graph_json(db, run.id)?;
    let graph = CircuitGraph::from_json(&graph_json);
    let lineage = RunLineage::read(
        |key| stored.get(key),
        graph
            .as_ref()
            .unwrap_or(&CircuitGraph::walking_skeleton("")),
    );
    let mut projection = ReviewContinuation {
        lineage,
        supported: false,
        available: false,
        refusal_reason: None,
        refusal_message: None,
        source_agent_node_id: run.source_agent_node_id,
        reviewer_agent_node_id: steps
            .iter()
            .find(|step| step.node_id == "reviewer")
            .and_then(|step| step.agent_node_id),
        target_run_id: None,
        review_nodes: None,
    };
    if !matches!(lineage.recovery, RecoveryAncestry::None) {
        projection.lineage.review_origin = crate::circuit::lineage::ReviewOrigin::Unknown;
        match lineage_root_inner(db, run.id) {
            Ok(root) => {
                let root_context =
                    super::context_store::StoredContext::of_column(&root.context_json);
                let root_graph = super::evidence::run_graph_json(db, root.id)?;
                // An unreadable historical graph cannot invalidate the current
                // executable snapshot. Its known codec origin can still survive.
                projection.lineage.review_origin = match CircuitGraph::from_json(&root_graph) {
                    Ok(graph) => {
                        RunLineage::read(|key| root_context.get(key), &graph).review_origin
                    }
                    Err(_) => RunLineage::decode(|key| root_context.get(key)).review_origin,
                };
            }
            Err(EligibilityError::Refused(_)) => {}
            Err(EligibilityError::Database(error)) => return Err(error),
        }
    }
    let refusal = match graph {
        Err(_) => Some(Refusal::InvalidGraph),
        Ok(graph) => {
            projection.supported = review_continuation_supported(run, &graph);
            projection.review_nodes = review_nodes(&graph);
            if projection.source_agent_node_id.is_none() && graph.is_issue_driven_autopilot_review()
            {
                projection.source_agent_node_id = steps
                    .iter()
                    .find(|step| step.node_id == "implementer")
                    .and_then(|step| step.agent_node_id);
            }
            if run.state != "failed" {
                Some(if projection.supported {
                    Refusal::RunNotFailed
                } else {
                    Refusal::UnsupportedGraph
                })
            } else {
                match extension_plan_inner(reader, run.id, 1) {
                    Ok(ExtensionPlan::Existing(id)) => {
                        let target = super::ledger::get_circuit_run_inner(db, id)?
                            .ok_or_else(|| DbError::not_found(Refusal::RunMissing.message()))?;
                        let target_steps = super::ledger::list_circuit_run_steps_inner(db, id)?;
                        projection.source_agent_node_id =
                            target.source_agent_node_id.or_else(|| {
                                target_steps
                                    .iter()
                                    .find(|step| step.node_id == "implementer")
                                    .and_then(|step| step.agent_node_id)
                            });
                        projection.reviewer_agent_node_id = target_steps
                            .iter()
                            .find(|step| step.node_id == "reviewer")
                            .and_then(|step| step.agent_node_id);
                        projection.target_run_id = Some(id);
                        projection.available = true;
                        None
                    }
                    Ok(ExtensionPlan::Extend(plan)) => {
                        projection.target_run_id = Some(plan.target);
                        projection.source_agent_node_id = Some(plan.recovery.source_id);
                        projection.reviewer_agent_node_id = plan.retained_reviewer;
                        projection.available = true;
                        None
                    }
                    Err(EligibilityError::Refused(reason)) => Some(reason),
                    Err(EligibilityError::Database(error)) => return Err(error),
                }
            }
        }
    };
    projection.refusal_reason = refusal;
    projection.refusal_message = refusal.map(|reason| reason.message().to_string());
    Ok(projection)
}

fn review_nodes(graph: &CircuitGraph) -> Option<ReviewRunNodes> {
    let verdict = graph
        .nodes
        .iter()
        .find(|node| matches!(node.kind, CircuitNodeKind::ReviewVerdict { .. }))?;
    let present = |id: &str| graph.node(id).map(|_| id.to_string());
    Some(ReviewRunNodes {
        verdict_node_id: verdict.id.clone(),
        retry_node_ids: graph
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, CircuitNodeKind::RetryLimit { .. }))
            .map(|node| node.id.clone())
            .collect(),
        merge_node_id: graph
            .node("merge")
            .filter(|node| matches!(node.kind, CircuitNodeKind::InjectPty { .. }))
            .map(|node| node.id.clone()),
        merge_confirmed_node_id: present("close_implementer"),
        merge_unconfirmed_node_ids: ["merge_unconfirmed", "merge_blocked"]
            .iter()
            .filter_map(|id| present(id))
            .collect(),
    })
}

pub(crate) fn recovery_circuit_inner(
    db: &Connection,
    mesh_id: i64,
    recovery: &ReviewRecovery,
) -> Result<(i64, String), DbError> {
    let graph = recovery.graph.to_json()?;
    // Recovery circuits are disabled and reusable; they cannot acquire an
    // issue trigger or replace the stock title-bar preset.
    let existing = db.query_row(
        "SELECT id, name FROM autopilot_circuits WHERE mesh_id=?1 AND enabled=0 AND is_preset=0 AND description=?2 AND graph_json=?3 LIMIT 1",
        params![mesh_id, "Continue a failed review on its retained worktree", graph],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    if let Some(existing) = existing {
        return Ok(existing);
    }
    db.execute("INSERT INTO autopilot_circuits (mesh_id,name,description,enabled,graph_json,is_preset) VALUES (?1,?2,?3,0,?4,0)",
        params![mesh_id, recovery.name, "Continue a failed review on its retained worktree", graph])?;
    Ok((db.last_insert_rowid(), recovery.name.clone()))
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
