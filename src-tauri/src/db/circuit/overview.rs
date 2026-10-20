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
}

pub fn overview(run_id: i64) -> Result<CircuitRunOverview, DbError> {
    let db = crate::db::read_conn()?;
    let tx = db.unchecked_transaction()?;
    let overview = overview_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(overview)
}

pub(crate) fn overview_inner(db: &Connection, run_id: i64) -> Result<CircuitRunOverview, DbError> {
    let run = super::ledger::get_circuit_run_inner(db, run_id)?
        .ok_or_else(|| DbError::not_found("Run no longer exists."))?;
    let circuit_name: String = db.query_row(
        "SELECT name FROM autopilot_circuits WHERE id=?1",
        [run.circuit_id],
        |row| row.get(0),
    )?;
    Ok(CircuitRunOverview {
        review_round_limit: super::review_rounds::current_limit(db, &run)?,
        steps: super::ledger::list_circuit_run_steps_inner(db, run_id)?,
        graph: run_graph(db, run_id)?,
        attention: attention_inner(db, run_id)?,
        circuit_name,
        run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_reads_the_runs_pinned_graph_and_its_attention_together() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        let blueprint = CircuitGraph::walking_skeleton("as authored");
        let mut pinned = blueprint.clone();
        pinned.nodes.retain(|node| node.id != "notify");
        pinned.edges.retain(|edge| edge.to != "notify");
        db.execute_batch(
            "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'Review loop');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status)
                VALUES(1,'trigger',1,'completed');",
        )
        .unwrap();
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [blueprint.to_json().unwrap()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO circuit_run_snapshots(run_id,graph_json,behavior_revision) VALUES(1,?1,1)",
            [pinned.to_json().unwrap()],
        )
        .unwrap();

        let overview = overview_inner(&db, 1).unwrap();
        assert_eq!(overview.circuit_name, "Review loop");
        assert_eq!(overview.run.state, "running");
        assert_eq!(overview.steps.len(), 1);
        assert_eq!(
            overview.graph, pinned,
            "a pinned run never shows later edits"
        );
        assert!(overview.attention.pushes.is_empty());
        assert!(serde_json::to_value(&overview)
            .unwrap()
            .get("review_round_limit")
            .is_none());
        let detail = crate::commands::circuit::CircuitRunDetail {
            run: overview.run.clone(),
            steps: overview.steps.clone(),
            review_round_limit: None,
        };
        assert!(serde_json::to_value(&detail)
            .unwrap()
            .get("review_round_limit")
            .is_none());
        let unlimited = crate::commands::circuit::CircuitRunDetail {
            review_round_limit: Some(0),
            ..detail
        };
        assert_eq!(
            serde_json::to_value(&unlimited).unwrap()["review_round_limit"],
            0
        );
        let unlimited = CircuitRunOverview {
            review_round_limit: Some(0),
            ..overview
        };
        assert_eq!(
            serde_json::to_value(&unlimited).unwrap()["review_round_limit"],
            0
        );
        assert!(overview_inner(&db, 2).is_err());
    }
}
