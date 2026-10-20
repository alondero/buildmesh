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
    let mut context = CircuitContext::from_json(&run.context_json)?;
    context.set("review.round_limit", limit.to_string());
    context.set(
        "retry.max_retries",
        if limit == 0 {
            "unlimited".into()
        } else {
            limit.to_string()
        },
    );
    tx.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?2, updated_at=datetime('now') WHERE id=?1",
        params![run_id, context.to_json()?],
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
mod tests {
    use super::*;
    use crate::circuit::{
        model::CircuitGraph,
        stepper::{RunState, TransitionFence},
    };

    fn seed(state: &str, graph: CircuitGraph) {
        let db = crate::db::write_conn();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,status,attempt) VALUES(1,'reviewer','running',4);").unwrap();
        db.execute(
            "UPDATE autopilot_circuit_runs SET state=?1 WHERE id=1",
            [state],
        )
        .unwrap();
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1 WHERE id=1",
            [graph.to_json().unwrap()],
        )
        .unwrap();
    }

    #[test]
    fn editing_allowance_preserves_in_flight_work_and_blueprint_and_fences_stale_worker() {
        let _db = crate::db::test_support::isolated();
        let graph = CircuitGraph::agent_review(None, None, 3);
        seed("running", graph.clone());
        let before = crate::db::list_circuit_run_steps(1).unwrap();
        assert_eq!(set_review_round_limit(1, 1).unwrap(), "running");
        assert_eq!(
            crate::db::list_circuit_run_steps(1).unwrap()[0].status,
            "running"
        );
        assert_eq!(
            crate::db::list_circuit_run_steps(1).unwrap()[0].attempt,
            before[0].attempt
        );
        let run = crate::db::get_circuit_run(1).unwrap().unwrap();
        assert_eq!(run.state, "running");
        assert_eq!(
            CircuitContext::from_json(&run.context_json)
                .unwrap()
                .get("review.round_limit"),
            Some("1")
        );
        assert_eq!(
            crate::db::get_autopilot_circuit(1)
                .unwrap()
                .unwrap()
                .graph_json,
            graph.to_json().unwrap()
        );
        let mut db = crate::db::write_conn();
        let stale = TransitionFence {
            state: RunState::Running,
            steps: vec![crate::circuit::stepper::StepView {
                node_id: "reviewer".into(),
                status: crate::circuit::stepper::StepStatus::Running,
                attempt: 4,
                outcome: None,
                error: None,
                agent_node_id: None,
            }],
            revision: Some(0),
        };
        assert!(super::super::evidence::commit_transition_locked(
            &mut db,
            1,
            Some("failed"),
            "{}",
            &[],
            super::super::evidence::EvidenceWrite {
                expected: Some(&stale),
                ..Default::default()
            }
        )
        .is_err());
        let retained = super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap();
        assert_eq!(retained.context_json, run.context_json);
        assert_eq!(retained.state, "running");
    }

    #[test]
    fn queued_and_paused_runs_can_raise_the_limit_or_select_unlimited() {
        for state in ["pending", "paused"] {
            let _db = crate::db::test_support::isolated();
            seed(state, CircuitGraph::issue_driven_autopilot_review("run"));
            for (input, expected) in [(9, "9"), (10, "0"), (100, "0"), (0, "0"), (2, "2")] {
                assert_eq!(set_review_round_limit(1, input).unwrap(), state);
                let run = crate::db::get_circuit_run(1).unwrap().unwrap();
                assert_eq!(
                    CircuitContext::from_json(&run.context_json)
                        .unwrap()
                        .get("review.round_limit"),
                    Some(expected)
                );
            }
        }
    }

    #[test]
    fn refuses_negative_missing_terminal_and_non_review_runs() {
        let _db = crate::db::test_support::isolated();
        seed("running", CircuitGraph::walking_skeleton("work"));
        assert!(set_review_round_limit(1, -1)
            .unwrap_err()
            .to_string()
            .contains("positive"));
        assert!(set_review_round_limit(99, 3)
            .unwrap_err()
            .to_string()
            .contains("no longer exists"));
        assert!(set_review_round_limit(1, 3)
            .unwrap_err()
            .to_string()
            .contains("no review retry loop"));
        for state in ["completed", "failed", "cancelled"] {
            crate::db::write_conn()
                .execute(
                    "UPDATE autopilot_circuit_runs SET state=?1 WHERE id=1",
                    [state],
                )
                .unwrap();
            assert!(set_review_round_limit(1, 3)
                .unwrap_err()
                .to_string()
                .contains("active run"));
        }
    }

    #[test]
    fn probe_and_overview_use_pinned_allowance_even_when_blueprint_loses_its_review_loop() {
        let _db = crate::db::test_support::isolated();
        let pinned = CircuitGraph::agent_review(None, None, 3);
        seed("running", pinned.clone());
        {
            let db = crate::db::write_conn();
            db.execute("INSERT INTO circuit_run_snapshots(run_id,graph_json,behavior_revision) VALUES(1,?1,1)", [pinned.to_json().unwrap()]).unwrap();
            db.execute(
                "UPDATE autopilot_circuits SET graph_json=?1 WHERE id=1",
                [CircuitGraph::walking_skeleton("edited").to_json().unwrap()],
            )
            .unwrap();
        }
        let (rows, _) = super::super::list_circuit_probe(1, 10).unwrap();
        assert_eq!(
            rows.iter().find(|(c, _)| c.id == 1).unwrap().1[0].review_round_limit,
            Some(3)
        );
        assert_eq!(
            super::super::overview::overview(1)
                .unwrap()
                .review_round_limit,
            Some(3)
        );
        set_review_round_limit(1, 9).unwrap();
        assert_eq!(
            super::super::overview::overview(1)
                .unwrap()
                .review_round_limit,
            Some(9)
        );
    }

    #[test]
    fn authored_zero_budget_is_not_displayed_as_unlimited() {
        let _db = crate::db::test_support::isolated();
        seed("running", CircuitGraph::agent_review(None, None, 0));
        assert_eq!(
            super::super::overview::overview(1)
                .unwrap()
                .review_round_limit,
            Some(1)
        );
        set_review_round_limit(1, 0).unwrap();
        assert_eq!(
            super::super::overview::overview(1)
                .unwrap()
                .review_round_limit,
            Some(0)
        );
    }

    #[test]
    fn review_again_adds_one_round_after_lowering_the_live_allowance() {
        let _db = crate::db::test_support::isolated();
        seed("running", CircuitGraph::agent_review(None, None, 3));
        {
            let db = crate::db::write_conn();
            db.execute_batch("INSERT INTO agent_nodes(id,mesh_id,name,path,status,provider) VALUES(9,1,'source','/repo','ready','codex');
                UPDATE autopilot_circuit_runs SET source_agent_node_id=9, context_json='{\"source.review_preset\":\"1\",\"source.agent_id\":\"9\"}' WHERE id=1;
                DELETE FROM autopilot_circuit_run_steps WHERE run_id=1;
                INSERT INTO autopilot_circuit_run_steps(run_id,node_id,status,attempt,outcome) VALUES(1,'verdict','completed',1,'working');").unwrap();
        }
        set_review_round_limit(1, 1).unwrap();
        crate::db::write_conn()
            .execute(
                "UPDATE autopilot_circuit_runs SET state='failed' WHERE id=1",
                [],
            )
            .unwrap();
        super::super::recovery::extend_failed_review(1, 1).unwrap();
        let run = crate::db::get_circuit_run(1).unwrap().unwrap();
        let context = CircuitContext::from_json(&run.context_json).unwrap();
        assert_eq!(context.get("retry.max_retries"), Some("2"));
        assert_eq!(context.get("retry.attempt"), Some("2"));
        assert_eq!(context.get("review.round_limit"), None);
    }
}
