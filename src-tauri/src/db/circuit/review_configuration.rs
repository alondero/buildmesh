//! Operator changes to reviewer launches that have not started yet.
use crate::{
    circuit::{context::CircuitContext, model::CircuitNodeKind},
    db::DbError,
};

pub fn reconfigure_reviewers(node_ids: &[i64], selection: &str) -> Result<(), DbError> {
    crate::circuit::compatibility::validate_reviewer_provider_id(selection)?;
    let prefs = crate::preferences::load()?;
    let plan =
        crate::preferences::launch_configurations::resolve(&prefs, selection, &Default::default())?;
    let snapshot =
        serde_json::to_string(&crate::preferences::launch_configurations::snapshot(plan))?;
    let mut db = crate::db::write_conn();
    reconfigure_locked(&mut db, node_ids, selection, &snapshot)
}

fn reconfigure_locked(
    db: &mut rusqlite::Connection,
    node_ids: &[i64],
    selection: &str,
    snapshot: &str,
) -> Result<(), DbError> {
    if node_ids.is_empty() || node_ids.len() > 10 {
        return Err(DbError::invalid("Choose 1-10 implementation nodes"));
    }
    let tx = db.transaction()?;
    for node_id in node_ids {
        let run_id = super::ledger::find_live_run_for_source_inner(&tx, *node_id)?
            .ok_or_else(|| DbError::invalid(format!("Node {node_id} has no active review run")))?;
        let run = super::ledger::get_circuit_run_inner(&tx, run_id)?
            .ok_or_else(|| DbError::not_found("Review run disappeared"))?;
        let graph = super::evidence::run_graph(&tx, run_id)?;
        let steps = super::ledger::list_circuit_run_steps_inner(&tx, run_id)?;
        let mut context = CircuitContext::from_json(&run.context_json)?;
        let mut changed = false;
        for gate in &graph.nodes {
            let CircuitNodeKind::ReviewVerdict {
                target_node_id: Some(target),
            } = &gate.kind
            else {
                continue;
            };
            if steps.iter().any(|step| step.node_id == *target) {
                return Err(DbError::conflict(format!("Node {node_id}'s reviewer already started. Start a fresh review to change its Launch Configuration.")));
            }
            context.set(&format!("review.launch.{target}"), snapshot.to_owned());
            changed = true;
        }
        if !changed {
            return Err(DbError::invalid(format!(
                "Node {node_id}'s circuit has no reviewer"
            )));
        }
        context.set("review.provider", selection);
        tx.execute("UPDATE autopilot_circuit_runs SET context_json=?2, updated_at=datetime('now') WHERE id=?1", rusqlite::params![run_id, context.to_json()?])?;
        super::evidence::append_history(
            &tx,
            super::evidence::HistoryEvent {
                run_id,
                node_id: None,
                attempt: None,
                kind: "review_configuration",
                detail: &serde_json::json!({"selection":selection}).to_string(),
                source: Some(super::evidence::SOURCE_OPERATOR),
                disposition: Some(super::evidence::DISPOSITION_APPLIED),
            },
        )?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
#[path = "review_configuration_tests.rs"]
mod tests;
