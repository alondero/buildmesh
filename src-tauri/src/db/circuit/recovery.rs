//! Review extensions borrow the retained implementation agent and reuse its
//! Circuit Run. Earlier findings and replaced graph snapshots remain archived.

use rusqlite::{Connection, OptionalExtension, params};
use crate::autopilot::circuit::{context::CircuitContext, model::{CircuitGraph, CircuitNodeKind}};

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

fn is_review_extension(state: &str, context_json: &str) -> bool {
    matches!(state, "pending" | "running" | "paused" | "completed")
        && CircuitContext::from_json(context_json).ok()
            .is_some_and(|context| context.get("review.extended") == Some("1"))
}

pub fn existing_review_target(run_id: i64) -> Result<Option<i64>, String> {
    let db = crate::db::read_conn();
    let run = super::ledger::get_circuit_run_inner(&db, run_id).map_err(|e| e.to_string())?
        .ok_or("This run no longer exists.")?;
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
pub(crate) fn continuation_target_inner(db: &Connection, run_id: i64) -> Result<ContinuationTarget, String> {
    let original = super::ledger::get_circuit_run_inner(db, run_id).map_err(|e| e.to_string())?
        .ok_or("This run no longer exists.")?;
    if original.state != "failed" {
        return Err("Only failed reviews can be continued. Cancelled reviews require a fresh review.".into());
    }
    let mut current = run_id;
    let mut visited = std::collections::HashSet::new();
    loop {
        if !visited.insert(current) { return Err("Review lineage is inconsistent. Start a fresh review.".into()); }
        let next: Option<(i64,String)> = db.query_row(
            "SELECT id,state FROM autopilot_circuit_runs
             WHERE json_extract(CASE WHEN json_valid(context_json) THEN context_json ELSE '{}' END, '$.\"recovery.from_run_id\"')=?1
             ORDER BY id LIMIT 1", [current.to_string()], |row| Ok((row.get(0)?,row.get(1)?)))
            .optional().map_err(|e| e.to_string())?;
        match next {
            None => return Ok(ContinuationTarget::Failed(current)),
            Some((id,state)) if state == "failed" => current = id,
            Some((id,state)) if matches!(state.as_str(), "pending" | "running" | "paused" | "completed") =>
                return Ok(ContinuationTarget::Existing(id)),
            Some(_) => return Err("The continued review was cancelled. Start a fresh review.".into()),
        }
    }
}

pub(crate) fn review_recovery_inner(db: &Connection, run_id: i64, rounds: i32) -> Result<ReviewRecovery, String> {
    if !(1..=10000).contains(&rounds) {
        return Err("Review round limit reached.".into());
    }
    let run = super::ledger::get_circuit_run_inner(db, run_id).map_err(|e| e.to_string())?
        .ok_or("This run no longer exists.")?;
    if run.state != "failed" {
        return Err("Only failed reviews can be continued. Open the active run to resume or cancel it.".into());
    }
    let context = CircuitContext::from_json(&run.context_json)?;
    let original = super::evidence::run_graph(db, run_id)?;
    let steps = super::ledger::list_circuit_run_steps_inner(db, run_id).map_err(|e| e.to_string())?;
    let (source_id, local) = review_source_id(&run, &original, &steps)?;
    let source = crate::db::agent_node::get_agent_node_by_id_inner(db, source_id)
        .map_err(|_| "The implementation agent is no longer retained. Recover from the PR branch and start a new review.".to_string())?;
    if source.mesh_id != run.mesh_id {
        return Err("The implementation agent no longer belongs to this Mesh.".into());
    }
    let mut reviewer = original.node("reviewer").ok_or("The reviewer definition is unavailable.")?.kind.clone();
    let CircuitNodeKind::SpawnAgentNode { prompt, provider, model, effort, .. } = &mut reviewer else {
        return Err("The reviewer definition is unavailable.".into());
    };
    // Freeze the original review scope, including PR comment instructions and
    // custom reviewer prompts. Do not replay implementation or publish steps.
    let mut prompt_context = context.clone();
    for key in ["source.output", "retry.attempt", "retry.max_retries", "node.reviewer.output", "circuit.run_id"] {
        prompt_context.set(key, format!("{{{{{key}}}}}"));
    }
    *prompt = prompt_context.resolve(prompt);
    if context.get("source.review_preset") == Some("1") {
        *model = None;
        *effort = None;
        *provider = None;
    }
    *provider = provider.clone().filter(|p| !p.trim().is_empty())
        .or_else(|| context.get("review.provider").filter(|p| !p.trim().is_empty()).map(str::to_string))
        .or_else(|| Some(source.launch_configuration.as_ref().map_or_else(|| source.provider.clone(), |c| c.id.clone())));
    let mut graph = CircuitGraph::agent_review(None, None, rounds);
    graph.nodes.iter_mut().find(|n| n.id == "reviewer").unwrap().kind = reviewer;
    let feedback_id = if local { "feedback" } else { "follow_feedback" };
    if let Some(CircuitNodeKind::InjectPty { prompt, .. }) = original.node(feedback_id).map(|n| &n.kind) {
        if let CircuitNodeKind::InjectPty { prompt: next_prompt, .. } = &mut graph.nodes.iter_mut().find(|n| n.id == "feedback").unwrap().kind {
            *next_prompt = prompt_context.resolve(prompt);
        }
    } else {
        return Err("The review feedback step has changed. Open the implementation agent to recover this custom circuit.".into());
    }
    graph.validate()?;
    let frozen_launches = context.get("review.launch.reviewer").map(|value| vec![("review.launch.reviewer".into(), value.into())]).unwrap_or_default();
    Ok(ReviewRecovery { run_id, source_id, graph, name: "Continued review".into(), frozen_launches })
}

fn review_source_id(
    run: &crate::models::AutopilotCircuitRun,
    original: &CircuitGraph,
    steps: &[crate::models::AutopilotCircuitRunStep],
) -> Result<(i64, bool), String> {
    if run.state != "failed" {
        return Err("Only failed reviews can be continued. Open the active run to resume or cancel it.".into());
    }
    let local = run.source_agent_node_id.is_some() && original.has_local_review_contract();
    if !local && !original.is_issue_driven_autopilot_review() {
        return Err("This custom circuit needs manual recovery. Open its implementation agent and inspect the failed step.".into());
    }
    let source_id = if local {
        run.source_agent_node_id
    } else {
        steps.iter().find(|step| step.node_id == "implementer").and_then(|step| step.agent_node_id)
    }.ok_or("The implementation agent is no longer retained. Recover from the PR branch and start a new review.")?;
    if !steps.iter().any(|step| original.node(&step.node_id).is_some_and(|node|
        matches!(node.kind, CircuitNodeKind::ReviewVerdict { .. }))) {
        return Err("This run stopped before review. Open the implementation agent and resolve its failure first.".into());
    }
    if !matches!(original.node("reviewer").map(|node| &node.kind), Some(CircuitNodeKind::SpawnAgentNode { .. })) {
        return Err("The reviewer definition is unavailable.".into());
    }
    let feedback_id = if local { "feedback" } else { "follow_feedback" };
    if !matches!(original.node(feedback_id).map(|node| &node.kind), Some(CircuitNodeKind::InjectPty { .. })) {
        return Err("The review feedback step has changed. Open the implementation agent to recover this custom circuit.".into());
    }
    Ok((source_id, local))
}

pub fn review_recovery_source(run_id: i64) -> Result<i64, String> {
    let db = crate::db::read_conn();
    if let Some(run) = super::ledger::get_circuit_run_inner(&db, run_id).map_err(|e| e.to_string())? {
        if is_review_extension(&run.state, &run.context_json) {
            return run.source_agent_node_id.ok_or("The implementation agent is no longer retained.".into());
        }
    }
    let parent = match continuation_target_inner(&db, run_id)? {
        ContinuationTarget::Failed(id) => id,
        ContinuationTarget::Existing(_) => run_id,
    };
    let run = super::ledger::get_circuit_run_inner(&db, parent).map_err(|e| e.to_string())?
        .ok_or("This run no longer exists.")?;
    let graph = super::evidence::run_graph(&db, parent)?;
    let steps = super::ledger::list_circuit_run_steps_inner(&db, parent).map_err(|e| e.to_string())?;
    let (source_id, _) = review_source_id(&run, &graph, &steps)?;
    let source = crate::db::agent_node::get_agent_node_by_id_inner(&db, source_id)
        .map_err(|_| "The implementation agent is no longer retained. Recover from the PR branch and start a new review.".to_string())?;
    if source.mesh_id != run.mesh_id {
        return Err("The implementation agent no longer belongs to this Mesh.".into());
    }
    Ok(source_id)
}

pub fn continue_failed_review(run_id: i64, rounds: i32) -> Result<i64, String> {
    let mut db = crate::db::write_conn();
    let recovery = review_recovery_inner(&db, run_id, rounds)?;
    super::ledger::create_node_circuit_run_recovery_locked(&mut db, recovery, rounds)
}

/// Extend the allowance of one failed review and queue its next reviewer
/// attempt on the same run. The append-only history keeps the earlier rounds;
/// the step rows are only the engine's current projection.
pub fn extend_failed_review(run_id: i64, additional_rounds: i32) -> Result<i64, String> {
    let mut db = crate::db::write_conn();
    extend_failed_review_locked(&mut db, run_id, additional_rounds)
}

pub(crate) fn extend_failed_review_locked(db: &mut Connection, run_id: i64, additional_rounds: i32) -> Result<i64, String> {
    if !(1..=10).contains(&additional_rounds) {
        return Err("Additional review rounds must be between 1 and 10.".into());
    }
    let tx = db.transaction().map_err(|e| e.to_string())?;
    if let Some(run) = super::ledger::get_circuit_run_inner(&tx, run_id).map_err(|e| e.to_string())? {
        if is_review_extension(&run.state, &run.context_json) {
            return Ok(run_id);
        }
    }
    let target = match continuation_target_inner(&tx, run_id)? {
        ContinuationTarget::Existing(id) => return Ok(id),
        ContinuationTarget::Failed(id) => id,
    };
    let run = super::ledger::get_circuit_run_inner(&tx, target).map_err(|e| e.to_string())?
        .ok_or("This run no longer exists.")?;
    let original_graph_json = super::evidence::run_graph_json(&tx, target)?;
    let graph = CircuitGraph::from_json(&original_graph_json)?;
    let steps = super::ledger::list_circuit_run_steps_inner(&tx, target).map_err(|e| e.to_string())?;
    let prior_limit = ["review_retry", "retry"].iter().find_map(|id| match graph.node(id).map(|node| &node.kind) {
        Some(CircuitNodeKind::RetryLimit { max_retries }) => Some(*max_retries),
        _ => None,
    }).ok_or("The review round limit is unavailable.")?;
    let last_attempt = steps.iter().filter(|step| matches!(step.node_id.as_str(), "reviewer" | "verdict" | "review_classifier"))
        .map(|step| step.attempt).max().unwrap_or(0);
    let next_attempt = last_attempt.checked_add(1).ok_or("Review round limit reached.")?;
    let next_limit = prior_limit.max(last_attempt).checked_add(additional_rounds).filter(|limit| *limit <= 10000)
        .ok_or("Review round limit reached.")?;
    let recovery = review_recovery_inner(&tx, target, next_limit)?;
    let other_live = super::ledger::find_live_run_for_source_inner(&tx, recovery.source_id).map_err(|e| e.to_string())?;
    if let Some(id) = other_live { return Ok(id); }
    let cleanup_in_progress: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_node_lifecycle_leases WHERE node_id=?1 AND cleanup_generation IS NOT NULL)",
        [recovery.source_id], |row| row.get(0)).map_err(|e| e.to_string())?;
    if cleanup_in_progress {
        return Err("The implementation agent is still being stopped. Try Review again in a moment.".into());
    }
    let owned_reviewer_live: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM autopilot_circuit_run_steps s JOIN agent_nodes a ON a.id=s.agent_node_id
         WHERE s.run_id=?1 AND s.agent_node_id IS NOT NULL AND s.agent_node_id != ?2 AND a.status != 'archived')",
        params![target, recovery.source_id], |row| row.get(0)).map_err(|e| e.to_string())?;
    if owned_reviewer_live {
        return Err("The previous reviewer is still being cleaned up. Try Review again in a moment.".into());
    }

    // Old continuation runs are moved back under their original Circuit.
    // Their pinned review-only snapshot remains the executable graph.
    let mut root = target;
    let mut visited = std::collections::HashSet::new();
    loop {
        if !visited.insert(root) { return Err("Review lineage is inconsistent.".into()); }
        let current = super::ledger::get_circuit_run_inner(&tx, root).map_err(|e| e.to_string())?
            .ok_or("Review lineage is incomplete.")?;
        let context = CircuitContext::from_json(&current.context_json)?;
        match context.get("recovery.from_run_id").and_then(|id| id.parse::<i64>().ok()) {
            Some(parent) => root = parent,
            None => break,
        }
    }
    let root_run = super::ledger::get_circuit_run_inner(&tx, root).map_err(|e| e.to_string())?
        .ok_or("Review lineage is incomplete.")?;
    let circuit_name: String = tx.query_row("SELECT name FROM autopilot_circuits WHERE id=?1", [root_run.circuit_id], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    let mut context: std::collections::BTreeMap<String, String> = serde_json::from_str(&run.context_json).map_err(|e| e.to_string())?;
    context.retain(|key, _| !key.starts_with("node.") && !key.starts_with("evidence.") && key != "cleanup.pending");
    context.insert("retry.attempt".into(), next_attempt.to_string());
    context.insert("retry.max_retries".into(), next_limit.to_string());
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
    tx.execute("INSERT OR IGNORE INTO circuit_run_snapshot_history (run_id,attempt,graph_json)
        VALUES (?1,?2,?3)", params![target, next_attempt, original_graph_json])
        .map_err(|e| e.to_string())?;
    tx.execute("INSERT INTO circuit_run_snapshots (run_id,graph_json,behavior_revision) VALUES (?1,?2,1)
        ON CONFLICT(run_id) DO UPDATE SET graph_json=excluded.graph_json,
            behavior_revision=excluded.behavior_revision", params![target, graph_json])
        .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM autopilot_circuit_run_steps WHERE run_id=?1", [target]).map_err(|e| e.to_string())?;
    for node in ["trigger", "await_source", "source_ready"] {
        tx.execute("INSERT INTO autopilot_circuit_run_steps (run_id,node_id,status,attempt,outcome,started_at,completed_at)
            VALUES (?1,?2,'completed',?3,'completed',datetime('now'),datetime('now'))",
            params![target, node, next_attempt]).map_err(|e| e.to_string())?;
    }
    tx.execute("INSERT INTO autopilot_circuit_run_steps (run_id,node_id,status,attempt) VALUES (?1,'reviewer','pending_slot',?2)",
        params![target, next_attempt]).map_err(|e| e.to_string())?;
    tx.execute("UPDATE autopilot_circuit_runs SET circuit_id=?2, source_agent_node_id=?3, state='pending',
        context_json=?4, queue_position=(SELECT COALESCE(MAX(queue_position),0)+1 FROM autopilot_circuit_runs WHERE mesh_id=?5),
        updated_at=datetime('now') WHERE id=?1 AND state='failed'",
        params![target, root_run.circuit_id, recovery.source_id, serde_json::to_string(&context).map_err(|e| e.to_string())?, run.mesh_id])
        .map_err(|e| e.to_string())?;
    super::evidence::append_history(&tx, target, Some("reviewer"), Some(next_attempt), "review_extension",
        &serde_json::json!({"additional_rounds":additional_rounds,"round_limit":next_limit,"attempt":next_attempt,"graph_sha256":graph_sha256}).to_string(),
        Some(super::evidence::SOURCE_OPERATOR), Some(super::evidence::DISPOSITION_APPLIED)).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(target)
}

pub(crate) fn recovery_circuit_inner(db: &Connection, mesh_id: i64, recovery: &ReviewRecovery) -> Result<(i64, String), String> {
    let graph = recovery.graph.to_json()?;
    // Recovery circuits are disabled and reusable; they cannot acquire an
    // issue trigger or replace the stock title-bar preset.
    let existing = db.query_row(
        "SELECT id, name FROM autopilot_circuits WHERE mesh_id=?1 AND enabled=0 AND is_preset=0 AND description=?2 AND graph_json=?3 LIMIT 1",
        params![mesh_id, "Continue a failed review on its retained worktree", graph],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(|e| e.to_string())?;
    if let Some(existing) = existing { return Ok(existing); }
    db.execute("INSERT INTO autopilot_circuits (mesh_id,name,description,enabled,concurrency_limit,graph_json,is_preset) VALUES (?1,?2,?3,0,2,?4,0)",
        params![mesh_id, recovery.name, "Continue a failed review on its retained worktree", graph]).map_err(|e| e.to_string())?;
    Ok((db.last_insert_rowid(), recovery.name.clone()))
}
