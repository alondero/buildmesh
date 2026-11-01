//! Live review allowance edits preserve every current step and the pinned graph.

use crate::circuit::{
    context::CircuitContext,
    model::{CircuitGraph, CircuitNodeKind},
    stepper::review_retry_gate,
};
use crate::db::DbError;
use rusqlite::params;

/// The selector reads the same pinned graph and override as the worker.
pub(crate) fn current_limit(
    db: &rusqlite::Connection,
    run: &crate::models::AutopilotCircuitRun,
) -> crate::db::SqlResult<Option<i32>> {
    if !matches!(run.state.as_str(), "pending" | "running" | "paused") {
        return Ok(None);
    }
    let json = super::evidence::run_graph_json(db, run.id)?;
    // A malformed graph remains inspectable in Probe, but cannot be configured.
    let Ok(graph) = CircuitGraph::from_json(&json) else {
        return Ok(None);
    };
    let Some(authored) = graph.nodes.iter().find_map(|node| match node.kind {
        CircuitNodeKind::RetryLimit { max_retries } if review_retry_gate(&graph, &node.id) => {
            Some(max_retries)
        }
        _ => None,
    }) else {
        return Ok(None);
    };
    let context = CircuitContext::from_json(&run.context_json)
        .map_err(rusqlite::Error::InvalidParameterName)?;
    if let Some(limit) = context
        .get("review.round_limit")
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|limit| *limit >= 0)
    {
        return Ok(Some(limit));
    }
    let preset = (context.get("source.review_preset") == Some("1"))
        .then(|| {
            context
                .get("retry.max_retries")
                .and_then(|value| value.parse::<i32>().ok())
                .filter(|limit| *limit > 0)
        })
        .flatten();
    // An authored zero budget stops after the current pass; only an explicit
    // run override of zero means unlimited.
    Ok(Some(preset.unwrap_or(authored).max(1)))
}

/// Zero or a value above nine selects unlimited review rounds.
pub fn set_review_round_limit(run_id: i64, max_rounds: i32) -> Result<String, DbError> {
    if max_rounds < 0 {
        return Err(DbError::invalid(
            "Review rounds must be zero (unlimited) or a positive number.",
        ));
    }
    let limit = if max_rounds > 9 { 0 } else { max_rounds };
    let mut db = crate::db::write_conn();
    let tx = db.transaction()?;
    let run = super::ledger::get_circuit_run_inner(&tx, run_id)?
        .ok_or_else(|| DbError::not_found("This run no longer exists."))?;
    if !matches!(run.state.as_str(), "pending" | "running" | "paused") {
        return Err(DbError::conflict(
            "Review rounds can only be changed on an active run.",
        ));
    }
    let graph = super::evidence::run_graph(&tx, run_id)?;
    if !graph
        .nodes
        .iter()
        .any(|node| review_retry_gate(&graph, &node.id))
    {
        return Err(DbError::invalid("This run has no review retry loop."));
    }
    let mut context = super::artifacts::load_context(&tx, run_id)?;
    context.set("review.round_limit", limit.to_string());
    context.set(
        "retry.max_retries",
        if limit == 0 {
            "unlimited".into()
        } else {
            limit.to_string()
        },
    );
    let context_json = super::artifacts::write_context(&tx, run_id, &context.to_json()?)?;
    tx.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?2, updated_at=datetime('now') WHERE id=?1",
        params![run_id, context_json],
    )?;
    // Advancing history invalidates any worker decision loaded before this edit.
    super::evidence::append_history(
        &tx,
        super::evidence::HistoryEvent {
            run_id,
            node_id: None,
            attempt: None,
            kind: "review_round_limit",
            detail: &serde_json::json!({ "max_rounds": limit }).to_string(),
            source: Some(super::evidence::SOURCE_OPERATOR),
            disposition: Some(super::evidence::DISPOSITION_APPLIED),
        },
    )?;
    tx.commit()?;
    Ok(run.state)
}

#[cfg(test)]
#[path = "review_rounds_tests.rs"]
mod tests;
