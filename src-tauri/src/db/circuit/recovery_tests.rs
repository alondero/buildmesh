use super::*;
use crate::circuit::stepper::{
    advance, Capacity, CircuitEvent, Effect, RunState, RunView, StepStatus, StepView,
};
use crate::circuit::{context::CircuitContext, model::CircuitGraph};
use crate::db::circuit::ledger::commit_circuit_advance_locked;
use rusqlite::Connection;

fn exhausted_review() -> Connection {
    let mut db = Connection::open_in_memory().unwrap();
    seed_exhausted_review(&mut db);
    db
}

fn seed_exhausted_review(db: &mut Connection) {
    crate::db::init_schema(db).unwrap();
    let graph = CircuitGraph::agent_review(None, None, 3).to_json().unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'review','/repo');
        INSERT INTO agent_nodes(id,mesh_id,name,path,status,provider) VALUES
            (9,1,'source','/repo','ready','codex'),(10,1,'reviewer','/review','ready','codex');
        INSERT INTO autopilot_circuits(id,mesh_id,name,graph_json) VALUES(1,1,'review','{}');
        INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,source_agent_node_id,trigger_identity,state,context_json)
            VALUES(1,1,1,9,'manual:review','running','{\"source.agent_id\":\"9\",\"source.review_preset\":\"1\"}');
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,status,attempt,outcome,agent_node_id,parent_agent_node_id)
            VALUES(1,'reviewer','completed',1,'completed',10,9), (1,'verdict','completed',3,'working',NULL,NULL);").unwrap();
    db.execute(
        "INSERT INTO circuit_run_snapshots(run_id,graph_json,behavior_revision) VALUES(1,?1,1)",
        [&graph],
    )
    .unwrap();
    commit_circuit_advance_locked(db, 1, Some("failed"), None, &[]).unwrap();
}

#[test]
fn exhausted_review_retains_and_reuses_reviewer_with_multiple_added_rounds() {
    let mut db = exhausted_review();
    assert!(
        super::super::leases::failed_circuit_agents_for_cleanup_inner(&db)
            .unwrap()
            .is_empty(),
        "exhausting review turns must leave the reviewer available to the user"
    );
    extend_failed_review_locked(&mut db, 1, 3).unwrap();
    let steps = super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap();
    let reviewer = steps
        .iter()
        .find(|step| step.node_id == "reviewer")
        .unwrap();
    assert_eq!(reviewer.agent_node_id, Some(10));
    assert_eq!(db.query_row("SELECT parent_agent_node_id FROM autopilot_circuit_run_steps WHERE run_id=1 AND node_id='reviewer'", [], |row| row.get::<_, i64>(0)).unwrap(), 9);
    assert_eq!(reviewer.status, "completed");
    assert_eq!(
        steps
            .iter()
            .find(|step| step.node_id == "re_review")
            .unwrap()
            .attempt,
        4
    );
    let run = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let context = CircuitContext::from_json(&run.context_json).unwrap();
    assert_eq!(context.get("review.round_limit"), Some("6"));
    // Repeated clicks do not add another allowance while the attempt is active.
    assert_eq!(extend_failed_review_locked(&mut db, 1, 3).unwrap(), 1);
    super::super::ledger::cancel_circuit_run_locked(&mut db, 1).unwrap();
    assert_eq!(
        super::super::leases::failed_circuit_agents_for_cleanup_inner(&db).unwrap(),
        vec![10]
    );
}

fn tick(view: &mut RunView) {
    let transition = advance(
        view,
        &CircuitEvent::Tick(Capacity {
            agent_free_slots: 0,
        }),
    );
    assert!(
        transition
            .effects
            .iter()
            .all(|effect| !matches!(effect, Effect::SpawnAgentNode { .. })),
        "continuation must never spawn a replacement reviewer"
    );
}

fn deliver(view: &mut RunView, step: &str) {
    let ready = advance(
        view,
        &CircuitEvent::AgentReady {
            node_id: step.into(),
        },
    );
    assert!(ready
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::InjectPty { node_id, .. } if node_id == step)));
    let attempt = view.step(step).unwrap().attempt;
    advance(
        view,
        &CircuitEvent::PromptDelivered {
            node_id: step.into(),
            attempt,
        },
    );
    tick(view);
}

fn classify(
    view: &mut RunView,
    step: &str,
    classification: crate::circuit::evaluator::Classification,
) -> crate::circuit::stepper::Transition {
    let output = format!("{step} round {}", view.step(step).unwrap().attempt);
    crate::circuit::test_support::advance_with_report_evidence(
        view,
        &CircuitEvent::TurnClassified {
            node_id: step.into(),
            classification: Some(classification),
            output: Some(output),
            binding: None,
        },
    )
}

#[test]
fn every_added_round_reuses_the_reviewer_and_only_approval_closes_it() {
    use crate::circuit::evaluator::Classification;
    let mut db = exhausted_review();
    extend_failed_review_locked(&mut db, 1, 3).unwrap();
    let run = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let mut view = RunView {
        run_id: 1,
        state: RunState::Pending,
        graph: super::super::evidence::run_graph(&db, 1).unwrap(),
        context: CircuitContext::from_json(&run.context_json).unwrap(),
        steps: super::super::ledger::list_circuit_run_steps_inner(&db, 1)
            .unwrap()
            .into_iter()
            .map(|step| StepView {
                node_id: step.node_id,
                status: StepStatus::from_db_str(&step.status),
                outcome: step
                    .outcome
                    .as_deref()
                    .and_then(crate::circuit::model::StepOutcome::from_db_str),
                error: step.error_message,
                agent_node_id: step.agent_node_id,
                attempt: step.attempt,
            })
            .collect(),
    };
    assert!(view.graph.has_local_review_contract());
    advance(&mut view, &CircuitEvent::Triggered);
    tick(&mut view);
    assert!(
        view.step("verdict").is_none(),
        "old review evidence cannot start a verdict before the next prompt"
    );
    for round in 4..=6 {
        assert_eq!(view.state, RunState::Running);
        assert_eq!(view.step("re_review").unwrap().attempt, round);
        assert_eq!(view.resolve_target_agent("re_review"), Some(10));
        deliver(&mut view, "re_review");
        assert_eq!(view.step("verdict").unwrap().attempt, round);
        if round < 6 {
            let findings = classify(&mut view, "verdict", Classification::Working);
            assert!(findings
                .effects
                .iter()
                .all(|effect| !matches!(effect, Effect::CloseAgentNode { .. })));
            tick(&mut view);
            deliver(&mut view, "feedback");
            classify(&mut view, "await_fixes", Classification::Completed);
            tick(&mut view);
        } else {
            let approval = classify(&mut view, "verdict", Classification::Completed);
            assert!(approval.effects.iter().any(|effect| matches!(effect, Effect::CloseAgentNode { target_node_id: Some(target), .. } if target == "reviewer")));
        }
    }
}

#[test]
fn retained_review_history_is_not_pruned_or_compacted_and_cleanup_blocks_continuation() {
    for trigger in ["manual:old-review", "issue:1:review"] {
        let mut db = exhausted_review();
        db.execute("UPDATE autopilot_circuit_runs SET trigger_identity=?1, updated_at='2020-01-01', created_at='2020-01-01' WHERE id=1", [trigger]).unwrap();
        db.execute_batch("INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,trigger_identity,state,created_at) VALUES(2,1,1,'manual:new','completed','2021-01-01');").unwrap();
        super::super::leases::prune_terminal_circuit_runs_before_inner(&db, "2022-01-01").unwrap();
        let retained = super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap();
        assert!(retained.context_json.contains("source.agent_id"));
        assert!(super::super::evidence::run_graph(&db, 1)
            .unwrap()
            .has_local_review_contract());
        db.execute_batch(
            "INSERT INTO agent_node_lifecycle_leases(node_id,cleanup_requested) VALUES(10,1);",
        )
        .unwrap();
        assert!(extend_failed_review_locked(&mut db, 1, 2)
            .unwrap_err()
            .to_string()
            .contains("still being cleaned up"));
    }
}

#[test]
fn implicit_review_target_retains_only_the_nearest_reviewer() {
    use crate::circuit::model::{CircuitEdge, CircuitNode, EdgeCondition};
    let mut db = exhausted_review();
    let mut graph = CircuitGraph::agent_review(None, None, 3);
    let helper_kind = graph.node("reviewer").unwrap().kind.clone();
    graph.nodes.push(CircuitNode {
        id: "helper".into(),
        kind: helper_kind,
    });
    graph.retarget_edge("publish_ready", "reviewer", "helper");
    graph.edges.push(CircuitEdge {
        from: "helper".into(),
        to: "reviewer".into(),
        condition: EdgeCondition::Always,
    });
    if let CircuitNodeKind::ReviewVerdict { target_node_id } = &mut graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "verdict")
        .unwrap()
        .kind
    {
        *target_node_id = None;
    }
    graph.validate().unwrap();
    db.execute(
        "UPDATE circuit_run_snapshots SET graph_json=?1 WHERE run_id=1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
    db.execute_batch("INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(11,1,'helper','/helper','ready');
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,status,agent_node_id,parent_agent_node_id) VALUES(1,'helper','completed',11,9);
        UPDATE autopilot_circuit_runs SET state='running' WHERE id=1;").unwrap();
    commit_circuit_advance_locked(&mut db, 1, Some("failed"), None, &[]).unwrap();
    assert_eq!(
        super::super::leases::failed_circuit_agents_for_cleanup_inner(&db).unwrap(),
        vec![11]
    );
}

#[test]
fn a_reviewer_borrowed_by_another_live_run_cannot_be_reused() {
    let mut db = exhausted_review();
    db.execute_batch("INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,source_agent_node_id,state) VALUES(2,1,1,10,'pending');").unwrap();
    assert!(extend_failed_review_locked(&mut db, 1, 2)
        .unwrap_err()
        .to_string()
        .contains("another active Circuit Run"));
    assert_eq!(
        super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap()
            .state,
        "failed"
    );
}

#[test]
fn issue_review_continuation_reuses_the_reviewer_without_replaying_implementation() {
    let mut db = exhausted_review();
    let graph = CircuitGraph::issue_driven_autopilot_review("review");
    db.execute(
        "UPDATE circuit_run_snapshots SET graph_json=?1 WHERE run_id=1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
    db.execute_batch("UPDATE autopilot_circuit_runs SET source_agent_node_id=NULL, context_json='{}',state='running' WHERE id=1;
        UPDATE autopilot_circuit_run_steps SET node_id='review_classifier' WHERE run_id=1 AND node_id='verdict';
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,status,agent_node_id) VALUES(1,'implementer','completed',9);").unwrap();
    commit_circuit_advance_locked(&mut db, 1, Some("failed"), None, &[]).unwrap();
    assert!(
        super::super::leases::failed_circuit_agents_for_cleanup_inner(&db)
            .unwrap()
            .is_empty()
    );
    extend_failed_review_locked(&mut db, 1, 3).unwrap();
    let steps = super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap();
    assert_eq!(
        steps
            .iter()
            .find(|step| step.node_id == "reviewer")
            .unwrap()
            .agent_node_id,
        Some(10)
    );
    assert_eq!(
        steps
            .iter()
            .find(|step| step.node_id == "re_review")
            .unwrap()
            .attempt,
        4
    );
    assert!(steps
        .iter()
        .all(|step| step.node_id != "implementer" && step.node_id != "open_pr"));
    assert_eq!(
        super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap()
            .source_agent_node_id,
        Some(9)
    );
}

#[test]
fn continuation_uses_a_new_prompt_attempt_after_failed_delivery() {
    use crate::db::circuit::evidence::{claim_effect, EffectIntent, EffectKind};
    let _isolated = crate::db::test_support::isolated();
    let next_attempt;
    {
        let mut db = crate::db::write_conn();
        seed_exhausted_review(&mut db);
        db.execute_batch("INSERT INTO autopilot_circuit_run_steps(run_id,node_id,status,attempt) VALUES(1,'re_review','failed',4);
            INSERT INTO circuit_effects(run_id,node_id,attempt,kind,state) VALUES(1,'re_review',4,'prompt','possible_dispatch');").unwrap();
        extend_failed_review_locked(&mut db, 1, 2).unwrap();
        next_attempt = super::super::ledger::list_circuit_run_steps_inner(&db, 1)
            .unwrap()
            .into_iter()
            .find(|step| step.node_id == "re_review")
            .unwrap()
            .attempt;
        assert_eq!(next_attempt, 5);
        db.execute_batch("UPDATE autopilot_circuit_runs SET state='running' WHERE id=1;
            UPDATE autopilot_circuit_run_steps SET status='running' WHERE run_id=1 AND node_id='re_review';").unwrap();
        db.execute("INSERT OR IGNORE INTO circuit_effects(run_id,node_id,attempt,kind,state) VALUES(1,'re_review',?1,'prompt','intent')", [next_attempt]).unwrap();
    }
    assert!(claim_effect(
        1,
        &EffectIntent {
            node_id: "re_review".into(),
            attempt: next_attempt,
            kind: EffectKind::Prompt
        }
    )
    .unwrap()
    .is_some());
}
