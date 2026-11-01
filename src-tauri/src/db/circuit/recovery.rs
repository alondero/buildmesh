//! Review extensions borrow the retained implementation agent and reuse its
//! Circuit Run. Earlier findings and replaced graph snapshots remain archived.

use crate::circuit::stepper::{resolve_target_agent, StepStatus, StepView};
use crate::circuit::{
    context::CircuitContext,
    model::{CircuitGraph, CircuitNodeKind},
};
use crate::db::DbError;
use rusqlite::{params, Connection, OptionalExtension};

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
    retained_reviewer_inner(&db, target)
}

fn retained_reviewer_inner(db: &Connection, run_id: i64) -> Result<Option<i64>, DbError> {
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
        Some((_, true)) => Err(DbError::conflict(
            "The previous reviewer is still being cleaned up. Try Review again in a moment.",
        )),
        Some((id, false)) => {
            let piloted_elsewhere: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM autopilot_circuit_runs r
                 WHERE r.id != ?1 AND r.state IN ('pending','running','paused')
                   AND (r.source_agent_node_id=?2 OR EXISTS(SELECT 1 FROM autopilot_circuit_run_steps s
                       WHERE s.run_id=r.id AND s.agent_node_id=?2)))",
                params![run_id, id], |row| row.get(0))?;
            if piloted_elsewhere {
                return Err(DbError::conflict("The reviewer is controlled by another active Circuit Run. Finish or cancel that run before continuing this review."));
            }
            Ok(Some(id))
        }
        None => Ok(None),
    }
}

fn is_review_extension(state: &str, context_json: &str) -> bool {
    matches!(state, "pending" | "running" | "paused" | "completed")
        && CircuitContext::from_json(context_json)
            .ok()
            .is_some_and(|context| context.get("review.extended") == Some("1"))
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

/// Follow recorded generations under the caller's connection. Repeated
/// requests for an older failure must never create a sibling review.
pub(crate) fn continuation_target_inner(
    db: &Connection,
    run_id: i64,
) -> Result<ContinuationTarget, DbError> {
    let original = super::ledger::get_circuit_run_inner(db, run_id)?
        .ok_or_else(|| DbError::not_found("This run no longer exists."))?;
    if original.state != "failed" {
        return Err(DbError::invalid(
            "Only failed reviews can be continued. Cancelled reviews require a fresh review.",
        ));
    }
    let mut current = run_id;
    let mut visited = std::collections::HashSet::new();
    loop {
        if !visited.insert(current) {
            return Err(DbError::conflict(
                "Review lineage is inconsistent. Start a fresh review.",
            ));
        }
        let next: Option<(i64,String)> = db.query_row(
            "SELECT id,state FROM autopilot_circuit_runs
             WHERE json_extract(CASE WHEN json_valid(context_json) THEN context_json ELSE '{}' END, '$.\"recovery.from_run_id\"')=?1
             ORDER BY id LIMIT 1", [current.to_string()], |row| Ok((row.get(0)?,row.get(1)?)))
            .optional()?;
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
            Some(_) => {
                return Err(DbError::conflict(
                    "The continued review was cancelled. Start a fresh review.",
                ))
            }
        }
    }
}

pub(crate) fn review_recovery_inner(
    db: &Connection,
    run_id: i64,
    rounds: i32,
) -> Result<ReviewRecovery, DbError> {
    if !(1..=10000).contains(&rounds) {
        return Err(DbError::conflict("Review round limit reached."));
    }
    let run = super::ledger::get_circuit_run_inner(db, run_id)?
        .ok_or_else(|| DbError::not_found("This run no longer exists."))?;
    if run.state != "failed" {
        return Err(DbError::invalid(
            "Only failed reviews can be continued. Open the active run to resume or cancel it.",
        ));
    }
    let context = super::artifacts::load_context(db, run_id)?;
    let original = super::evidence::run_graph(db, run_id)?;
    let steps = super::ledger::list_circuit_run_steps_inner(db, run_id)?;
    let (source_id, local) = review_source_id(&run, &original, &steps)?;
    let source = crate::db::agent_node::get_agent_node_by_id_inner(db, source_id).map_err(|_| {
        DbError::not_found(
            "The implementation agent is no longer retained. Recover from the PR branch and start a new review.",
        )
    })?;
    if source.mesh_id != run.mesh_id {
        return Err(DbError::conflict(
            "The implementation agent no longer belongs to this Mesh.",
        ));
    }
    let mut reviewer = original
        .node("reviewer")
        .ok_or_else(|| DbError::invalid("The reviewer definition is unavailable."))?
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
        return Err(DbError::invalid("The reviewer definition is unavailable."));
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
    if context.get("source.review_preset") == Some("1") {
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
        return Err(DbError::conflict(
            "The review feedback step has changed. Open the implementation agent to recover this custom circuit.",
        ));
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
/// A node review still needs its source agent. The issue-review blueprint
/// does not. The failed-state check and the later recovery checks stay in
/// [`review_source_id`]. Last-round guidance is shown while the run is still
/// going, so it uses this contract.
pub(crate) fn review_continuation_supported(
    run: &crate::models::AutopilotCircuitRun,
    graph: &CircuitGraph,
) -> bool {
    (run.source_agent_node_id.is_some() && graph.has_local_review_contract())
        || graph.is_issue_driven_autopilot_review()
}

fn review_source_id(
    run: &crate::models::AutopilotCircuitRun,
    original: &CircuitGraph,
    steps: &[crate::models::AutopilotCircuitRunStep],
) -> Result<(i64, bool), DbError> {
    if run.state != "failed" {
        return Err(DbError::invalid(
            "Only failed reviews can be continued. Open the active run to resume or cancel it.",
        ));
    }
    if !review_continuation_supported(run, original) {
        return Err(DbError::invalid(
            "This custom circuit needs manual recovery. Open its implementation agent and inspect the failed step.",
        ));
    }
    let local = run.source_agent_node_id.is_some() && original.has_local_review_contract();
    let source_id = if local {
        run.source_agent_node_id
    } else {
        steps.iter().find(|step| step.node_id == "implementer").and_then(|step| step.agent_node_id)
    }.ok_or_else(|| DbError::not_found(
        "The implementation agent is no longer retained. Recover from the PR branch and start a new review.",
    ))?;
    if !steps.iter().any(|step| {
        original
            .node(&step.node_id)
            .is_some_and(|node| matches!(node.kind, CircuitNodeKind::ReviewVerdict { .. }))
    }) {
        return Err(DbError::invalid(
            "This run stopped before review. Open the implementation agent and resolve its failure first.",
        ));
    }
    if !matches!(
        original.node("reviewer").map(|node| &node.kind),
        Some(CircuitNodeKind::SpawnAgentNode { .. })
    ) {
        return Err(DbError::invalid("The reviewer definition is unavailable."));
    }
    let feedback_id = if local { "feedback" } else { "follow_feedback" };
    if !matches!(
        original.node(feedback_id).map(|node| &node.kind),
        Some(CircuitNodeKind::InjectPty { .. })
    ) {
        return Err(DbError::conflict(
            "The review feedback step has changed. Open the implementation agent to recover this custom circuit.",
        ));
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
    if let Some(run) = super::ledger::get_circuit_run_inner(&tx, run_id)? {
        if is_review_extension(&run.state, &run.context_json) {
            return Ok(run_id);
        }
    }
    let target = match continuation_target_inner(&tx, run_id)? {
        ContinuationTarget::Existing(id) => return Ok(id),
        ContinuationTarget::Failed(id) => id,
    };
    let run = super::ledger::get_circuit_run_inner(&tx, target)?
        .ok_or_else(|| DbError::not_found("This run no longer exists."))?;
    let original_graph_json = super::evidence::run_graph_json(&tx, target)?;
    let graph = CircuitGraph::from_json(&original_graph_json)?;
    let steps = super::ledger::list_circuit_run_steps_inner(&tx, target)?;
    let prior_limit = ["review_retry", "retry"]
        .iter()
        .find_map(|id| match graph.node(id).map(|node| &node.kind) {
            Some(CircuitNodeKind::RetryLimit { max_retries }) => Some(*max_retries),
            _ => None,
        })
        .ok_or_else(|| DbError::invalid("The review round limit is unavailable."))?;
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
        .ok_or_else(|| DbError::conflict("Review round limit reached."))?;
    let context = CircuitContext::from_json(&run.context_json)?;
    let prior_limit = context
        .get("review.round_limit")
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|limit| *limit > 0)
        .unwrap_or(prior_limit);
    let next_limit = prior_limit
        .max(last_attempt)
        .checked_add(additional_rounds)
        .filter(|limit| *limit <= 10000)
        .ok_or_else(|| DbError::conflict("Review round limit reached."))?;
    let mut recovery = review_recovery_inner(&tx, target, next_limit)?;
    let other_live = super::ledger::find_live_run_for_source_inner(&tx, recovery.source_id)?;
    if let Some(id) = other_live {
        return Ok(id);
    }
    let cleanup_in_progress: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_node_lifecycle_leases WHERE node_id=?1 AND cleanup_generation IS NOT NULL)",
        [recovery.source_id], |row| row.get(0))?;
    if cleanup_in_progress {
        return Err(DbError::conflict(
            "The implementation agent is still being stopped. Try Review again in a moment.",
        ));
    }
    let retained_reviewer = retained_reviewer_inner(&tx, target)?;
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
    let mut root = target;
    let mut visited = std::collections::HashSet::new();
    loop {
        if !visited.insert(root) {
            return Err(DbError::conflict("Review lineage is inconsistent."));
        }
        let current = super::ledger::get_circuit_run_inner(&tx, root)?
            .ok_or_else(|| DbError::conflict("Review lineage is incomplete."))?;
        let context = CircuitContext::from_json(&current.context_json)?;
        match context
            .get("recovery.from_run_id")
            .and_then(|id| id.parse::<i64>().ok())
        {
            Some(parent) => root = parent,
            None => break,
        }
    }
    let root_run = super::ledger::get_circuit_run_inner(&tx, root)?
        .ok_or_else(|| DbError::conflict("Review lineage is incomplete."))?;
    let circuit_name: String = tx.query_row(
        "SELECT name FROM autopilot_circuits WHERE id=?1",
        [root_run.circuit_id],
        |row| row.get(0),
    )?;
    let mut context = super::artifacts::load_context_map(&tx, target)?;
    context.retain(|key, _| {
        !key.starts_with("node.") && !key.starts_with("evidence.") && key != "cleanup.pending"
    });
    context.insert("retry.attempt".into(), next_attempt.to_string());
    context.insert("retry.max_retries".into(), next_limit.to_string());
    context.insert("review.round_limit".into(), next_limit.to_string());
    context.insert("review.extended".into(), "1".into());
    if run.source_agent_node_id.is_none() {
        context.insert("review.source_was_circuit_owned".into(), "1".into());
    }
    context.insert("circuit.id".into(), root_run.circuit_id.to_string());
    context.insert("circuit.name".into(), circuit_name);
    context.insert("source.agent_id".into(), recovery.source_id.to_string());
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
    // The extension keeps the run's trigger context but starts the reviewer's
    // working state afresh, so the node-scoped bulky values go with it
    // (issue #2601). The values the extension does keep — the source's report,
    // the trigger body — stay in the side table and in the column.
    super::artifacts::delete_values_with_prefix(&tx, target, "node")?;
    super::artifacts::delete_values_with_prefix(&tx, target, "evidence")?;
    let context_json =
        super::artifacts::write_context(&tx, target, &serde_json::to_string(&context)?)?;
    tx.execute("UPDATE autopilot_circuit_runs SET circuit_id=?2, source_agent_node_id=?3, state='pending',
        context_json=?4, queue_position=(SELECT COALESCE(MAX(queue_position),0)+1 FROM autopilot_circuit_runs WHERE mesh_id=?5),
        updated_at=datetime('now') WHERE id=?1 AND state='failed'",
        params![target, root_run.circuit_id, recovery.source_id, context_json, run.mesh_id])
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
