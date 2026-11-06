//! One Circuit Run as an Agent Node card's Overview tab reads it.
//!
//! A single snapshot holds everything the tab draws: the run and its step
//! ledger, the graph the run executes (its pinned snapshot when it has one,
//! not the blueprint as later edited), and the attention view that decides
//! which actions are offered. Reading them in one transaction means the step
//! rail and the action buttons can never describe two different moments.

use rusqlite::Connection;
use serde::Serialize;

use super::evidence::{attention_inner, run_graph, CircuitRunAttention};
use crate::circuit::model::CircuitGraph;
use crate::db::DbError;
use crate::models::{AutopilotCircuitRun, AutopilotCircuitRunStep};

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitRunOverview.ts")]
pub struct CircuitRunOverview {
    pub run: AutopilotCircuitRun,
    pub steps: Vec<AutopilotCircuitRunStep>,
    pub circuit_name: String,
    /// The graph this run executes.
    pub graph: CircuitGraph,
    pub attention: CircuitRunAttention,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_round_limit: Option<i32>,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_continuation: Option<crate::circuit::review_continuation::ReviewContinuation>,
}

pub fn overview(run_id: i64) -> Result<CircuitRunOverview, DbError> {
    let db = crate::db::read_conn()?;
    let tx = db.unchecked_transaction()?;
    let overview = overview_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(overview)
}

pub(crate) fn overview_inner(db: &Connection, run_id: i64) -> Result<CircuitRunOverview, DbError> {
    let mut run = super::ledger::get_circuit_run_inner(db, run_id)?
        .ok_or_else(|| DbError::not_found("Run no longer exists."))?;
    // The Overview tab shows the reviewer's report, which is part of the
    // run's context, so the projection serves the whole context.
    run.context_json = super::context_store::load_served_json(db, run.id)?;
    let circuit_name: String = db.query_row(
        "SELECT name FROM autopilot_circuits WHERE id=?1",
        [run.circuit_id],
        |row| row.get(0),
    )?;
    let steps = super::ledger::list_circuit_run_steps_inner(db, run_id)?;
    Ok(CircuitRunOverview {
        review_continuation: Some(super::recovery::review_continuation_inner(
            db, &run, &steps,
        )?),
        review_round_limit: super::review_rounds::current_limit(db, &run)?,
        steps,
        graph: run_graph(db, run_id)?,
        attention: attention_inner(db, run_id)?,
        circuit_name,
        run,
    })
}

#[cfg(test)]
#[path = "overview_tests.rs"]
mod tests;
