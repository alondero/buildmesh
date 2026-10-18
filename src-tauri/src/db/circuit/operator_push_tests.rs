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
fn duplicate_observation_history_does_not_block_recording_the_step() {
    let mut db = fixture(
        &graph(await_turn()),
        "unverified",
        &CircuitContext::default(),
    );
    let seen = revision(&db);
    for _ in 0..3 {
        super::evidence::append_history(
            &db,
            super::evidence::HistoryEvent {
                run_id: 1,
                node_id: Some("await"),
                attempt: Some(1),
                kind: "observation",
                detail: "{}",
                source: Some("codex_rollout"),
                disposition: Some("duplicate"),
            },
        )
        .unwrap();
    }

    let attest = CheckpointRequest {
        run_id: 1,
        node_id: "await".into(),
        attempt: 1,
        expected_revision: seen,
        action: CheckpointAction::Completed,
        reason: String::new(),
    };
    super::evidence::record_outcome_locked(&mut db, &attest)
        .expect("duplicate observations are not a new fact");
    let step = super::ledger::list_circuit_run_steps_inner(&db, 1)
        .unwrap()
        .into_iter()
        .find(|step| step.node_id == "await")
        .unwrap();
    assert_eq!(step.status, "completed");
    assert_eq!(step.outcome.as_deref(), Some("completed"));

    let mut pushed = fixture(&graph(await_turn()), "running", &CircuitContext::default());
    let seen = revision(&pushed);
    super::evidence::append_history(
        &pushed,
        super::evidence::HistoryEvent {
            run_id: 1,
            node_id: Some("await"),
            attempt: Some(1),
            kind: "observation",
            detail: "{}",
            source: Some("agent_status_projection"),
            disposition: Some("duplicate"),
        },
    )
    .unwrap();
    let mut move_on = request(&pushed, StepPushOutcome::Completed);
    move_on.expected_revision = seen;
    push_step_locked(&mut pushed, &move_on, NOW_MS)
        .expect("a push survives duplicate observations");

    let mut blocked = fixture(
        &graph(await_turn()),
        "unverified",
        &CircuitContext::default(),
    );
    let seen = revision(&blocked);
    super::evidence::append_history(
        &blocked,
        super::evidence::HistoryEvent {
            run_id: 1,
            node_id: Some("await"),
            attempt: Some(1),
            kind: "observation",
            detail: "{}",
            source: Some("codex_rollout"),
            disposition: Some("accepted"),
        },
    )
    .unwrap();
    let attest = CheckpointRequest {
        run_id: 1,
        node_id: "await".into(),
        attempt: 1,
        expected_revision: seen,
        action: CheckpointAction::Completed,
        reason: String::new(),
    };
    let error = super::evidence::record_outcome_locked(&mut blocked, &attest).unwrap_err();
    assert!(
        error.contains("The run changed. Refresh its evidence before acting."),
        "{error}"
    );
    let mut move_on = request(&blocked, StepPushOutcome::Completed);
    move_on.expected_revision = seen;
    let error = push_step_locked(&mut blocked, &move_on, NOW_MS).unwrap_err();
    assert!(
        error.contains("The run changed. Refresh it before acting."),
        "{error}"
    );
}

#[test]
fn completing_the_fix_wait_offers_the_next_review() {
    let review = CircuitGraph::agent_review(None, None, 5);
    let mut context = CircuitContext::default();
    context.set("source.agent_id", "9");
    let db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
        INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(9,1,'implementer','/repo','idle');
        INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
        INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status)
            VALUES(1,'await_fixes',2,'unverified');",
    )
    .unwrap();
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [review.to_json().unwrap()],
    )
    .unwrap();
    db.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?1",
        [context.to_json().unwrap()],
    )
    .unwrap();

    let attention = attention_inner(&db, 1).unwrap();
    let checkpoint = attention
        .checkpoints
        .iter()
        .find(|checkpoint| checkpoint.node_id == "await_fixes")
        .expect("the fix wait is an unverified checkpoint");
    assert!(
        checkpoint
            .actions
            .iter()
            .any(|action| matches!(action, CheckpointAction::Completed)),
        "the person can record that the implementation finished: {:?}",
        checkpoint.actions
    );
    assert!(
        checkpoint.starts_next_review,
        "recording completion re-enters the review loop"
    );
    let push = attention
        .pushes
        .iter()
        .find(|push| push.node_id == "await_fixes")
        .expect("the fix wait can be moved on");
    assert!(push.outcomes.contains(&StepPushOutcome::Completed));
    assert!(push.starts_next_review);
    assert!(!checkpoint.ends_review);
    assert!(!checkpoint.review_again);
    assert!(!push.ends_review);
    assert!(!push.review_again);

    let ordinary = fixture(
        &graph(await_turn()),
        "unverified",
        &CircuitContext::default(),
    );
    let ordinary = attention_inner(&ordinary, 1).unwrap();
    assert!(!ordinary.checkpoints[0].starts_next_review);
    assert!(!ordinary.checkpoints[0].ends_review);
    assert!(!ordinary.pushes[0].starts_next_review);
    assert!(!ordinary.pushes[0].ends_review);
}

#[test]
fn the_last_allowed_review_round_offers_review_again() {
    let review = CircuitGraph::agent_review(None, None, 5);
    let mut context = CircuitContext::default();
    context.set("source.agent_id", "9");
    let db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
        INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(9,1,'implementer','/repo','idle');
        INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
        INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES
            (1,'await_fixes',2,'unverified'),
            (1,'re_review',5,'completed');",
    )
    .unwrap();
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [review.to_json().unwrap()],
    )
    .unwrap();
    db.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?1",
        [context.to_json().unwrap()],
    )
    .unwrap();

    let attention = attention_inner(&db, 1).unwrap();
    let checkpoint = attention
        .checkpoints
        .iter()
        .find(|checkpoint| checkpoint.node_id == "await_fixes")
        .expect("the fix wait is an unverified checkpoint");
    assert!(
        !checkpoint.starts_next_review,
        "the last allowed attempt does not start another review"
    );
    assert!(
        checkpoint.ends_review,
        "the last allowed attempt stops the review"
    );
    assert!(
        !checkpoint.review_again,
        "this fixture has no source agent, so recovery cannot continue it"
    );
    let push = attention
        .pushes
        .iter()
        .find(|push| push.node_id == "await_fixes")
        .expect("the fix wait can be moved on");
    assert!(push.outcomes.contains(&StepPushOutcome::Completed));
    assert!(!push.starts_next_review);
    assert!(push.ends_review);
    assert!(!push.review_again);

    let mut raised = context.clone();
    raised.set("source.review_preset", "1");
    raised.set("retry.max_retries", "6");
    db.execute(
        "UPDATE autopilot_circuit_run_steps SET attempt=5 WHERE node_id='re_review'",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?1",
        [raised.to_json().unwrap()],
    )
    .unwrap();
    let raised_attention = attention_inner(&db, 1).unwrap();
    let raised_checkpoint = raised_attention
        .checkpoints
        .iter()
        .find(|checkpoint| checkpoint.node_id == "await_fixes")
        .unwrap();
    assert!(
        raised_checkpoint.starts_next_review,
        "a review preset follows the run's raised round limit, not only the graph"
    );
    assert!(!raised_checkpoint.ends_review);
    assert!(!raised_checkpoint.review_again);
    assert!(raised_attention.pushes[0].starts_next_review);
    assert!(!raised_attention.pushes[0].ends_review);
    assert!(!raised_attention.pushes[0].review_again);
}

/// A bounded review cycle that is not a node review and not the issue-review
/// blueprint. Recovery rejects it; the last-round guidance must not promise
/// Review again.
fn custom_review_loop() -> CircuitGraph {
    CircuitGraph {
        version: CIRCUIT_GRAPH_VERSION,
        blueprint: None,
        nodes: vec![
            node("trigger", CircuitNodeKind::Manual),
            node(
                "implementer",
                CircuitNodeKind::SpawnAgentNode {
                    prompt: "do the work".into(),
                    name: None,
                    provider: None,
                    model: None,
                    effort: None,
                    extra_args: None,
                    timeout_seconds: None,
                },
            ),
            node(
                "wait",
                CircuitNodeKind::AwaitAgentTurn {
                    target_node_id: Some("implementer".into()),
                },
            ),
            node("gate", CircuitNodeKind::RetryLimit { max_retries: 1 }),
            node(
                "again",
                CircuitNodeKind::InjectPty {
                    prompt: "look again".into(),
                    target_node_id: Some("reviewer".into()),
                },
            ),
            node(
                "verdict",
                CircuitNodeKind::ReviewVerdict {
                    target_node_id: Some("reviewer".into()),
                },
            ),
            node(
                "reviewer",
                CircuitNodeKind::SpawnAgentNode {
                    prompt: "review the change".into(),
                    name: None,
                    provider: None,
                    model: None,
                    effort: None,
                    extra_args: None,
                    timeout_seconds: None,
                },
            ),
            node(
                "exhausted",
                CircuitNodeKind::Notify {
                    message: "stopped".into(),
                },
            ),
        ],
        edges: vec![
            edge("trigger", "implementer", EdgeCondition::Always),
            edge("trigger", "reviewer", EdgeCondition::Always),
            edge("implementer", "wait", EdgeCondition::Always),
            edge(
                "wait",
                "gate",
                EdgeCondition::OnOutcome(StepOutcome::Completed),
            ),
            edge(
                "gate",
                "again",
                EdgeCondition::OnOutcome(StepOutcome::Completed),
            ),
            edge(
                "gate",
                "exhausted",
                EdgeCondition::OnOutcome(StepOutcome::Failed),
            ),
            edge("again", "verdict", EdgeCondition::Always),
            edge(
                "verdict",
                "wait",
                EdgeCondition::OnOutcome(StepOutcome::Working),
            ),
        ],
    }
}

fn review_attention_db(
    graph: &CircuitGraph,
    source_agent_node_id: Option<i64>,
    context: &CircuitContext,
    steps_sql: &str,
) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
        INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(9,1,'implementer','/repo','idle');
        INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
        INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');",
    )
    .unwrap();
    if let Some(source) = source_agent_node_id {
        db.execute(
            "UPDATE autopilot_circuit_runs SET source_agent_node_id=?1 WHERE id=1",
            [source],
        )
        .unwrap();
    }
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
    db.execute_batch(steps_sql).unwrap();
    db
}

#[test]
fn a_custom_review_loop_on_its_last_round_does_not_promise_review_again() {
    let custom = custom_review_loop();
    custom
        .validate()
        .expect("a bounded custom review loop is a valid graph");
    assert!(!custom.has_local_review_contract());
    assert!(!custom.is_issue_driven_autopilot_review());
    let mut context = CircuitContext::default();
    context.set("source.agent_id", "9");
    let db = review_attention_db(
        &custom,
        None,
        &context,
        "INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
            VALUES(1,'implementer',1,'completed',9);
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status)
            VALUES(1,'wait',1,'unverified');
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,outcome)
            VALUES(1,'again',1,'completed','completed');
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,outcome)
            VALUES(1,'verdict',1,'completed','working');",
    );
    let attention = attention_inner(&db, 1).unwrap();
    let checkpoint = attention
        .checkpoints
        .iter()
        .find(|checkpoint| checkpoint.node_id == "wait")
        .expect("the wait is an unverified checkpoint");
    assert!(
        checkpoint.ends_review,
        "the spent custom loop still stops the review"
    );
    assert!(!checkpoint.starts_next_review);
    assert!(
        !checkpoint.review_again,
        "recovery cannot continue this graph, so the checkpoint must not promise Review again"
    );
    let push = attention
        .pushes
        .iter()
        .find(|push| push.node_id == "wait")
        .expect("the wait can be moved on");
    assert!(push.ends_review);
    assert!(!push.review_again);

    db.execute(
        "UPDATE autopilot_circuit_runs SET state='failed' WHERE id=1",
        [],
    )
    .unwrap();
    let refused = super::recovery::review_recovery_inner(&db, 1, 1).unwrap_err();
    assert!(
        refused.contains("This custom circuit needs manual recovery. Open its implementation agent and inspect the failed step."),
        "{refused}"
    );

    let review = CircuitGraph::agent_review(None, None, 5);
    let supported = review_attention_db(
        &review,
        Some(9),
        &context,
        "INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status)
            VALUES(1,'await_fixes',2,'unverified');
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,outcome)
            VALUES(1,'re_review',5,'completed','completed');
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,outcome)
            VALUES(1,'verdict',5,'completed','working');",
    );
    let supported_attention = attention_inner(&supported, 1).unwrap();
    let supported_checkpoint = supported_attention
        .checkpoints
        .iter()
        .find(|checkpoint| checkpoint.node_id == "await_fixes")
        .expect("the fix wait is an unverified checkpoint");
    assert!(supported_checkpoint.ends_review);
    assert!(
        supported_checkpoint.review_again,
        "a node review with its source agent can be continued"
    );
    assert!(supported_attention.pushes[0].review_again);
    supported
        .execute(
            "UPDATE autopilot_circuit_runs SET state='failed' WHERE id=1",
            [],
        )
        .unwrap();
    let plan = super::recovery::review_recovery_inner(&supported, 1, 1).unwrap();
    assert_eq!(plan.source_id, 9);

    let issue = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
    issue.validate().unwrap();
    let issue_db = review_attention_db(
        &issue,
        None,
        &CircuitContext::default(),
        "INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
            VALUES(1,'implementer',1,'completed',9);
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status)
            VALUES(1,'feedback_classifier',1,'unverified');
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,outcome)
            VALUES(1,'re_review',3,'completed','completed');
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,outcome)
            VALUES(1,'review_classifier',3,'completed','working');",
    );
    let issue_attention = attention_inner(&issue_db, 1).unwrap();
    let issue_checkpoint = issue_attention
        .checkpoints
        .iter()
        .find(|checkpoint| checkpoint.node_id == "feedback_classifier")
        .expect("the feedback classifier is an unverified checkpoint");
    assert!(issue_checkpoint.ends_review);
    assert!(
        issue_checkpoint.review_again,
        "the issue-review blueprint can be continued"
    );
    issue_db
        .execute(
            "UPDATE autopilot_circuit_runs SET state='failed' WHERE id=1",
            [],
        )
        .unwrap();
    let issue_plan = super::recovery::review_recovery_inner(&issue_db, 1, 1).unwrap();
    assert_eq!(issue_plan.source_id, 9);
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
