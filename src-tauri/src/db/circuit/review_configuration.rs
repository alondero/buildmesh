//! Operator changes to reviewer launches that have not started yet.
use crate::{circuit::model::CircuitNodeKind, db::DbError};

pub fn reconfigure_reviewers(node_ids: &[i64], selection: &str) -> Result<(), DbError> {
    match crate::preferences::default_store() {
        Some(store) => reconfigure_reviewers_from(&store, node_ids, selection),
        // A build with **no** store installed (a test that never installed one)
        // reads the code-defined defaults, which is what the per-test helpers
        // around this one have always done (issue #1482).
        None => reconfigure_reviewers_with_preferences(
            node_ids,
            selection,
            &crate::preferences::AppPreferences::default(),
        ),
    }
}

/// [`reconfigure_reviewers`] against an explicit store.
///
/// The caller owns the store, so the launch configuration this reconfiguration
/// resolves is the one the caller set up (issue #1482).
pub fn reconfigure_reviewers_from(
    store: &crate::preferences::PreferencesStore,
    node_ids: &[i64],
    selection: &str,
) -> Result<(), DbError> {
    crate::circuit::compatibility::validate_reviewer_provider_id(selection)?;
    let prefs = store.load()?;
    reconfigure_reviewers_with_preferences(node_ids, selection, &prefs)
}

/// The reconfiguration itself, with the preferences already resolved.
fn reconfigure_reviewers_with_preferences(
    node_ids: &[i64],
    selection: &str,
    prefs: &crate::preferences::AppPreferences,
) -> Result<(), DbError> {
    crate::circuit::compatibility::validate_reviewer_provider_id(selection)?;
    let plan =
        crate::preferences::launch_configurations::resolve(prefs, selection, &Default::default())?;
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
        // The run must still be there; its context is read whole below.
        super::ledger::get_circuit_run_inner(&tx, run_id)?
            .ok_or_else(|| DbError::not_found("Review run disappeared"))?;
        let graph = super::evidence::run_graph(&tx, run_id)?;
        let steps = super::ledger::list_circuit_run_steps_inner(&tx, run_id)?;
        // The launch plan and the provider are the only values that change.
        // The rest of the run's context is left alone, so an operator's
        // edit never rewrites what the worker holds.
        let mut mutation = super::context_store::ContextMutation::new();
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
            mutation.put(format!("review.launch.{target}"), snapshot.to_owned());
            changed = true;
        }
        if !changed {
            return Err(DbError::invalid(format!(
                "Node {node_id}'s circuit has no reviewer"
            )));
        }
        mutation.put("review.provider", selection);
        let applied = super::context_store::apply_mutation(&tx, run_id, &mutation)?;
        tx.execute("UPDATE autopilot_circuit_runs SET context_json=?2, updated_at=datetime('now') WHERE id=?1", rusqlite::params![run_id, applied.column_json()])?;
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
