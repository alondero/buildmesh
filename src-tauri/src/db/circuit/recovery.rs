//! A follow-up review borrows the retained implementation agent. The failed
//! ledger stays immutable, including its report and cleanup ownership.

use rusqlite::{Connection, OptionalExtension, params};
use crate::autopilot::circuit::{context::CircuitContext, model::{CircuitGraph, CircuitNodeKind}};

#[derive(Debug)]
pub(crate) struct ReviewRecovery {
    pub run_id: i64,
    pub source_id: i64,
    pub graph: CircuitGraph,
    pub name: String,
}

pub(crate) fn review_recovery_inner(db: &Connection, run_id: i64, rounds: i32) -> Result<ReviewRecovery, String> {
    if !(1..=10).contains(&rounds) {
        return Err("Review rounds must be between 1 and 10.".into());
    }
    let run = super::ledger::get_circuit_run_inner(db, run_id).map_err(|e| e.to_string())?
        .ok_or("This run no longer exists.")?;
    if run.state != "failed" {
        return Err("Only failed reviews can be continued. Open the active run to resume or cancel it.".into());
    }
    let circuit = super::ledger::get_autopilot_circuit_inner(db, run.circuit_id).map_err(|e| e.to_string())?
        .ok_or("This circuit no longer exists.")?;
    let context = CircuitContext::from_json(&run.context_json)?;
    let original = CircuitGraph::from_json(&circuit.graph_json)?;
    let steps = super::ledger::list_circuit_run_steps_inner(db, run_id).map_err(|e| e.to_string())?;
    let local = context.get("source.review_preset") == Some("1") || context.get("recovery.from_run_id").is_some();
    if !local && !original.is_issue_driven_autopilot_review() {
        return Err("This custom circuit needs manual recovery. Open its implementation agent and inspect the failed step.".into());
    }
    let source_id = if local {
        run.source_agent_node_id
    } else {
        steps.iter().find(|s| s.node_id == "implementer").and_then(|s| s.agent_node_id)
    }.ok_or("The implementation agent is no longer retained. Recover from the PR branch and start a new review." )?;
    if !steps.iter().any(|s| original.node(&s.node_id).is_some_and(|n|
        matches!(n.kind, CircuitNodeKind::ReviewVerdict { .. }))) {
        return Err("This run stopped before review. Open the implementation agent and resolve its failure first.".into());
    }
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
        .or_else(|| Some(source.provider.clone()));
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
    Ok(ReviewRecovery { run_id, source_id, graph, name: "Continued review".into() })
}

pub fn review_recovery_source(run_id: i64, rounds: i32) -> Result<i64, String> {
    let db = crate::db::read_conn();
    Ok(review_recovery_inner(&db, run_id, rounds)?.source_id)
}

pub fn continue_failed_review(run_id: i64, rounds: i32) -> Result<i64, String> {
    let mut db = crate::db::write_conn();
    let recovery = review_recovery_inner(&db, run_id, rounds)?;
    super::ledger::create_node_circuit_run_recovery_locked(&mut db, recovery, rounds)
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
