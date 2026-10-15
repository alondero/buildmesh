//! Operator push against the production schema: what is offered, what the
//! write records, and that the next tick follows the outcome the person chose.

use rusqlite::Connection;

use super::evidence::{attention_inner, recovery_view, CheckpointAction, CheckpointRequest};
use super::operator_push::{push_step_locked, CircuitStepPushRequest, StepPushOutcome};
use crate::agent::session_lifecycle::{HookSignalDetail, LifecycleChangedPayload, LifecycleKind};
use crate::circuit::context::CircuitContext;
use crate::circuit::model::{
    CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition, StepOutcome,
    CIRCUIT_GRAPH_VERSION,
};
use crate::circuit::observation::WorkEvidence;
use crate::circuit::stepper::{advance, Capacity, CircuitEvent};
use crate::models::SessionStatus;

const NOW_MS: i64 = 1_760_000_000_000;

fn node(id: &str, kind: CircuitNodeKind) -> CircuitNode {
    CircuitNode {
        id: id.into(),
        kind,
    }
}

fn edge(from: &str, to: &str, condition: EdgeCondition) -> CircuitEdge {
    CircuitEdge {
        from: from.into(),
        to: to.into(),
        condition,
    }
}

/// trigger → spawn → await; await routes `completed` to `done` and `working`
/// to `nudge`. `blocked` is deliberately unrouted.
fn graph(await_kind: CircuitNodeKind) -> CircuitGraph {
    CircuitGraph {
        version: CIRCUIT_GRAPH_VERSION,
        blueprint: None,
        nodes: vec![
            node("trigger", CircuitNodeKind::Manual),
            node(
                "spawn",
                CircuitNodeKind::SpawnAgentNode {
                    prompt: "do the task".into(),
                    name: None,
                    provider: None,
                    model: None,
                    effort: None,
                    extra_args: None,
                    timeout_seconds: None,
                },
            ),
            node("await", await_kind),
            node(
                "done",
                CircuitNodeKind::Notify {
                    message: "done".into(),
                },
            ),
            node(
                "nudge",
                CircuitNodeKind::Notify {
                    message: "still working".into(),
                },
            ),
        ],
        edges: vec![
            edge("trigger", "spawn", EdgeCondition::Always),
            edge("spawn", "await", EdgeCondition::Always),
            edge(
                "await",
                "done",
                EdgeCondition::OnOutcome(StepOutcome::Completed),
            ),
            edge(
                "await",
                "nudge",
                EdgeCondition::OnOutcome(StepOutcome::Working),
            ),
        ],
    }
}

fn await_turn() -> CircuitNodeKind {
    CircuitNodeKind::AwaitAgentTurn {
        target_node_id: Some("spawn".into()),
    }
}

/// A running run whose `await` step is still waiting on agent 9, with the
/// given step status and run context.
fn fixture(graph: &CircuitGraph, await_status: &str, context: &CircuitContext) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
        INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(9,1,'agent','/repo','running');
        INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
        INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,outcome)
            VALUES(1,'trigger',1,'completed','completed');
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,outcome,agent_node_id)
            VALUES(1,'spawn',1,'completed','completed',9);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'await',1,?1)",
        [await_status],
    )
    .unwrap();
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
    db.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?1",
        [context.to_json().unwrap()],
    )
    .unwrap();
    db
}

fn stale_background_work() -> CircuitContext {
    let mut context = CircuitContext::default();
    let evidence = WorkEvidence {
        children: [("background-shell".to_string(), false)]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    context.set(
        "node.await.evidence.1",
        serde_json::to_string(&evidence).unwrap(),
    );
    context
}

fn set_lifecycle(db: &Connection, kind: LifecycleKind) {
    let payload = LifecycleChangedPayload::new(
        9,
        kind,
        SessionStatus::Running,
        &HookSignalDetail::default(),
        "harness report",
    );
    db.execute(
        "UPDATE agent_nodes SET lifecycle_snapshot=?1 WHERE id=9",
        [serde_json::to_string(&payload).unwrap()],
    )
    .unwrap();
}

fn revision(db: &Connection) -> i64 {
    attention_inner(db, 1).unwrap().revision
}

fn request(db: &Connection, outcome: StepPushOutcome) -> CircuitStepPushRequest {
    CircuitStepPushRequest {
        run_id: 1,
        node_id: "await".into(),
        attempt: 1,
        expected_revision: revision(db),
        outcome,
        reason: String::new(),
    }
}

fn push(db: &mut Connection, outcome: StepPushOutcome) -> Result<(), String> {
    let request = request(db, outcome);
    push_step_locked(db, &request, NOW_MS)
}

/// Run one worker tick over the durable state and return which successor
/// circuit nodes it started.
fn successors_after_tick(db: &Connection) -> Vec<String> {
    let run = super::ledger::get_circuit_run_inner(db, 1)
        .unwrap()
        .unwrap();
    let mut view = recovery_view(db, &run).unwrap();
    advance(
        &mut view,
        &CircuitEvent::Tick(Capacity {
            agent_free_slots: 4,
        }),
    );
    view.steps
        .iter()
        .filter(|step| step.node_id == "done" || step.node_id == "nudge")
        .map(|step| step.node_id.clone())
        .collect()
}

#[test]
fn a_running_wait_held_by_stale_background_work_can_be_pushed_on() {
    let mut db = fixture(&graph(await_turn()), "running", &stale_background_work());
    set_lifecycle(&db, LifecycleKind::BackgroundRunning);

    // The existing completion attestation only resolves Unverified steps, and
    // refuses known outstanding work: this is the case it cannot reach.
    let attest = CheckpointRequest {
        run_id: 1,
        node_id: "await".into(),
        attempt: 1,
        expected_revision: revision(&db),
        action: CheckpointAction::Completed,
        reason: String::new(),
    };
    assert!(super::evidence::record_outcome_locked(&mut db, &attest).is_err());

    let attention = attention_inner(&db, 1).unwrap();
    assert_eq!(attention.pushes.len(), 1);
    let push = &attention.pushes[0];
    assert_eq!(push.node_id, "await");
    assert_eq!(
        push.outcomes,
        vec![StepPushOutcome::Completed, StepPushOutcome::Working],
        "blocked is not routed from this gate, so it is not offered"
    );
    assert_eq!(
        push.overrides.as_deref(),
        Some(
            "The harness reports unfinished child or background work. Waiting for its completion evidence."
        )
    );

    let request = request(&db, StepPushOutcome::Completed);
    push_step_locked(&mut db, &request, NOW_MS).unwrap();

    let step = super::ledger::list_circuit_run_steps_inner(&db, 1)
        .unwrap()
        .into_iter()
        .find(|step| step.node_id == "await")
        .unwrap();
    assert_eq!(step.status, "completed");
    assert_eq!(step.outcome.as_deref(), Some("completed"));
    assert_eq!(
        step.attempt, 1,
        "a push finishes the attempt, it does not retry it"
    );

    let run = super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let context = CircuitContext::from_json(&run.context_json).unwrap();
    let evidence: WorkEvidence =
        serde_json::from_str(context.get("node.await.evidence.1").unwrap()).unwrap();
    assert_eq!(evidence.children.get("background-shell"), Some(&true));
    assert_eq!(context.get("node.await.status"), Some("completed"));

    let snapshot: Option<String> = db
        .query_row(
            "SELECT lifecycle_snapshot FROM agent_nodes WHERE id=9",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        snapshot, None,
        "the stale background-work report is cleared"
    );

    let history = super::evidence::history_inner(&db, 1).unwrap();
    let last = history.last().unwrap();
    assert_eq!(last.kind, "operator_override");
    assert_eq!(last.source.as_deref(), Some("operator"));
    assert_eq!(last.disposition.as_deref(), Some("completed"));
    assert!(last
        .detail
        .contains("Overrode: The harness reports unfinished"));
    assert!(last.detail.contains("Cleared the agent's stale"));

    assert!(
        push_step_locked(&mut db, &request, NOW_MS).is_err(),
        "a push against an earlier revision must not replay"
    );
    assert!(attention_inner(&db, 1).unwrap().pushes.is_empty());
    assert_eq!(successors_after_tick(&db), vec!["done".to_string()]);
}

#[test]
fn a_pushed_classifier_follows_the_outcome_the_person_chose() {
    let classifier = CircuitNodeKind::LlmTurnClassifier {
        target_node_id: Some("spawn".into()),
    };
    let mut db = fixture(&graph(classifier), "unverified", &CircuitContext::default());
    let mut request = request(&db, StepPushOutcome::Working);
    request.reason = "  It stopped to ask about the schema; nudge it on.  ".into();
    push_step_locked(&mut db, &request, NOW_MS).unwrap();

    let history = super::evidence::history_inner(&db, 1).unwrap();
    let last = history.last().unwrap();
    assert_eq!(last.disposition.as_deref(), Some("working"));
    assert_eq!(
        last.detail,
        "Operator moved this step on as working. Note: It stopped to ask about the schema; nudge it on."
    );
    assert_eq!(successors_after_tick(&db), vec!["nudge".to_string()]);
}

#[test]
fn an_unrouted_outcome_is_refused() {
    let mut db = fixture(&graph(await_turn()), "running", &CircuitContext::default());
    let error = push(&mut db, StepPushOutcome::Blocked).unwrap_err();
    assert!(error.contains("can no longer be moved on"), "{error}");
}

#[test]
fn a_pending_input_wait_is_overridden_and_settled() {
    let mut context = CircuitContext::default();
    context.set("node.await.human_wait", "1");
    let mut db = fixture(&graph(await_turn()), "running", &context);
    set_lifecycle(&db, LifecycleKind::PermissionRequested);
    let offered = attention_inner(&db, 1).unwrap().pushes.remove(0);
    assert!(offered
        .overrides
        .unwrap()
        .starts_with("The harness has an unresolved question or permission request."));

    push(&mut db, StepPushOutcome::Completed).unwrap();
    let run = super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let context = CircuitContext::from_json(&run.context_json).unwrap();
    assert_eq!(context.get("node.await.human_wait"), Some(""));
    assert_eq!(successors_after_tick(&db), vec!["done".to_string()]);
}

#[test]
fn nothing_is_offered_where_a_push_would_be_dishonest() {
    // An identity conflict says the evidence may belong to another session.
    let mut conflicted = CircuitContext::default();
    conflicted.set(
        "node.await.evidence.1",
        serde_json::to_string(&WorkEvidence {
            conflicted: true,
            ..Default::default()
        })
        .unwrap(),
    );
    let db = fixture(&graph(await_turn()), "running", &conflicted);
    assert!(attention_inner(&db, 1).unwrap().pushes.is_empty());

    // Review verdicts keep their explicit decision path.
    let review = CircuitNodeKind::ReviewVerdict {
        target_node_id: Some("spawn".into()),
    };
    let mut db = fixture(&graph(review), "running", &CircuitContext::default());
    assert!(attention_inner(&db, 1).unwrap().pushes.is_empty());
    assert!(push(&mut db, StepPushOutcome::Completed).is_err());

    // A step that has not started has nothing to override.
    let db = fixture(
        &graph(await_turn()),
        "pending_slot",
        &CircuitContext::default(),
    );
    assert!(attention_inner(&db, 1).unwrap().pushes.is_empty());

    // A paused run is not moving; resume it first.
    let mut db = fixture(&graph(await_turn()), "running", &CircuitContext::default());
    db.execute("UPDATE autopilot_circuit_runs SET state='paused'", [])
        .unwrap();
    assert!(attention_inner(&db, 1).unwrap().pushes.is_empty());
    let error = push(&mut db, StepPushOutcome::Completed).unwrap_err();
    assert!(error.contains("Only a running run"), "{error}");
}

#[test]
fn a_running_spawn_with_its_agent_can_be_marked_finished() {
    let db = fixture(&graph(await_turn()), "running", &CircuitContext::default());
    db.execute_batch(
        "UPDATE autopilot_circuit_run_steps SET status='running', outcome=NULL WHERE node_id='spawn';
        DELETE FROM autopilot_circuit_run_steps WHERE node_id='await';",
    )
    .unwrap();
    let pushes = attention_inner(&db, 1).unwrap().pushes;
    assert_eq!(pushes.len(), 1);
    assert_eq!(pushes[0].node_id, "spawn");
    assert_eq!(pushes[0].outcomes, vec![StepPushOutcome::Completed]);
    assert_eq!(pushes[0].overrides, None);

    // Before the agent is attached there is no work to vouch for.
    db.execute(
        "UPDATE autopilot_circuit_run_steps SET agent_node_id=NULL WHERE node_id='spawn'",
        [],
    )
    .unwrap();
    assert!(attention_inner(&db, 1).unwrap().pushes.is_empty());
}

#[test]
fn a_conflict_stays_refused_behind_a_stale_background_report() {
    // The agent's lifecycle report is checked before the evidence's own
    // conflict flag, so the conflict must not hide behind it.
    let mut context = CircuitContext::default();
    context.set(
        "node.await.evidence.1",
        serde_json::to_string(&WorkEvidence {
            conflicted: true,
            ..Default::default()
        })
        .unwrap(),
    );
    let mut db = fixture(&graph(await_turn()), "running", &context);
    set_lifecycle(&db, LifecycleKind::BackgroundRunning);
    assert!(attention_inner(&db, 1).unwrap().pushes.is_empty());
    assert!(push(&mut db, StepPushOutcome::Completed).is_err());
}

#[test]
fn a_push_settles_stale_work_on_every_step_of_the_same_agent() {
    let mut context = stale_background_work();
    context.set(
        "node.spawn.evidence.1",
        serde_json::to_string(&WorkEvidence {
            children: [("spawned-helper".to_string(), false)]
                .into_iter()
                .collect(),
            ..Default::default()
        })
        .unwrap(),
    );
    let mut db = fixture(&graph(await_turn()), "running", &context);
    push(&mut db, StepPushOutcome::Completed).unwrap();

    let run = super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let context = CircuitContext::from_json(&run.context_json).unwrap();
    let spawn: WorkEvidence =
        serde_json::from_str(context.get("node.spawn.evidence.1").unwrap()).unwrap();
    assert_eq!(spawn.children.get("spawned-helper"), Some(&true));
}

#[test]
fn a_wait_with_no_way_onward_can_still_be_marked_finished() {
    let mut last = graph(await_turn());
    last.edges.retain(|edge| edge.from != "await");
    let mut db = fixture(&last, "running", &CircuitContext::default());
    let offered = attention_inner(&db, 1).unwrap().pushes;
    assert_eq!(offered.len(), 1);
    assert_eq!(offered[0].outcomes, vec![StepPushOutcome::Completed]);
    assert!(push(&mut db, StepPushOutcome::Working).is_err());
    push(&mut db, StepPushOutcome::Completed).unwrap();
}
