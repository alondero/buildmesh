use super::*;
use crate::circuit::context::CircuitContext;
use crate::circuit::model::{
    CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, GithubActionKind,
    CIRCUIT_GRAPH_VERSION,
};
use crate::circuit::stepper::StepStatus;
use crate::db::circuit::evidence::{EffectIntent, EffectKind, EvidenceWrite};
use crate::db::CircuitStepOp;
use crate::models::{EnvType, SessionStatus};
use rusqlite::OptionalExtension;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(1);

fn seq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

struct Scratch {
    db: crate::db::test_support::IsolatedDbGuard,
    dir: tempfile::TempDir,
    mesh_id: i64,
    path: String,
}

struct World {
    _db: crate::db::test_support::IsolatedDbGuard,
    _dir: tempfile::TempDir,
    active: db::ActiveCircuitRun,
    view: RunView,
    run_id: i64,
}

impl Scratch {
    fn new() -> Self {
        let db = crate::db::test_support::isolated();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap().to_string();
        let mesh = db::create_mesh(
            &format!("effect-dispatch-{}-{}", std::process::id(), seq()),
            &path,
        )
        .unwrap();
        Self {
            db,
            dir,
            mesh_id: mesh.id,
            path,
        }
    }

    fn agent(&self) -> crate::models::AgentNode {
        db::create_agent_node(db::NewAgentNode {
            mesh_id: self.mesh_id,
            name: &format!("dispatch-agent-{}-{}", std::process::id(), seq()),
            path: &self.path,
            branch: "work",
            env: EnvType::Windows,
            provider: "grok",
            worktree_name: None,
            source_issue: None,
            source_pr: None,
            source_pr_pinned_sha: None,
            use_worktree: false,
            head_repo_owner: None,
            head_repo_clone_url: None,
            worktree_path: None,
            spawn_configuration: None,
        })
        .unwrap()
    }

    fn start(
        self,
        graph: CircuitGraph,
        steps: Vec<CircuitStepOp>,
        intents: &[EffectIntent],
        mut context: CircuitContext,
        db_state_after: Option<&str>,
    ) -> World {
        let json = graph.to_json().unwrap();
        let circuit = db::create_autopilot_circuit(
            self.mesh_id,
            &format!("dispatch-{}-{}", std::process::id(), seq()),
            "",
            &json,
        )
        .unwrap();
        let run_id = db::create_circuit_run(
            circuit.id,
            self.mesh_id,
            &format!("manual:dispatch-{}-{}", std::process::id(), seq()),
            "{}",
        )
        .unwrap();
        db::set_circuit_run_state(run_id, "running").unwrap();
        context.with_run(run_id);
        db::circuit::evidence::commit_transition(
            run_id,
            Some("running"),
            &context.to_json().unwrap(),
            &steps,
            EvidenceWrite {
                intents,
                ..Default::default()
            },
        )
        .unwrap();
        let active = db::list_active_circuit_runs()
            .unwrap()
            .into_iter()
            .find(|active| active.run.id == run_id)
            .expect("running run is active");
        if let Some(state) = db_state_after {
            db::set_circuit_run_state(run_id, state).unwrap();
        }
        let view = RunView {
            run_id,
            graph,
            state: RunState::Running,
            context,
            steps: steps
                .into_iter()
                .map(|step| crate::circuit::stepper::StepView {
                    node_id: step.node_id,
                    attempt: step.attempt,
                    status: StepStatus::Running,
                    outcome: None,
                    error: None,
                    agent_node_id: step.agent_node_id,
                })
                .collect(),
        };
        World {
            _db: self.db,
            _dir: self.dir,
            active,
            view,
            run_id,
        }
    }
}

fn spawn_kind() -> CircuitNodeKind {
    CircuitNodeKind::SpawnAgentNode {
        prompt: "work".into(),
        name: None,
        provider: None,
        model: None,
        effort: None,
        extra_args: None,
        timeout_seconds: None,
    }
}

fn one_node(id: &str, kind: CircuitNodeKind) -> CircuitGraph {
    CircuitGraph {
        version: CIRCUIT_GRAPH_VERSION,
        blueprint: None,
        nodes: vec![CircuitNode {
            id: id.into(),
            kind,
        }],
        edges: vec![],
    }
}

fn running_step(node_id: &str, agent_node_id: Option<i64>) -> CircuitStepOp {
    CircuitStepOp {
        node_id: node_id.into(),
        status: "running".into(),
        outcome: None,
        error: None,
        agent_node_id,
        attempt: 1,
        fresh_attempt: false,
    }
}

fn intent(node_id: &str, kind: EffectKind) -> EffectIntent {
    EffectIntent {
        node_id: node_id.into(),
        attempt: 1,
        kind,
    }
}

fn dispatch(world: &mut World, host: &FakeHost, effects: &[Effect]) -> DispatchedEffects {
    dispatch_effects(host, &world.active, &mut world.view, effects, None).expect("dispatch runs")
}

fn effect_state(run_id: i64, node_id: &str, kind: &str) -> Option<String> {
    db::read_conn()
        .unwrap()
        .query_row(
            "SELECT state FROM circuit_effects WHERE run_id=?1 AND node_id=?2 AND kind=?3",
            rusqlite::params![run_id, node_id, kind],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

fn effect_rows(run_id: i64) -> i64 {
    db::read_conn()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM circuit_effects WHERE run_id=?1",
            [run_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn history_kinds(run_id: i64) -> Vec<String> {
    let conn = db::read_conn().unwrap();
    let mut stmt = conn
        .prepare("SELECT kind FROM circuit_run_history WHERE run_id=?1 ORDER BY id")
        .unwrap();
    stmt.query_map(rusqlite::params![run_id], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn step_agent(run_id: i64, node_id: &str) -> Option<i64> {
    db::read_conn()
        .unwrap()
        .query_row(
            "SELECT agent_node_id FROM autopilot_circuit_run_steps WHERE run_id=?1 AND node_id=?2",
            rusqlite::params![run_id, node_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn pin_ready_turn(agent_id: i64) -> String {
    db::update_agent_node_status(agent_id, SessionStatus::Ready).unwrap();
    db::write_conn()
        .execute(
            "UPDATE agent_nodes SET session_started_at=42 WHERE id=?1",
            [agent_id],
        )
        .unwrap();
    db::agent_turn_stamp(agent_id)
        .unwrap()
        .expect("session start and status stamp")
}

fn probe_state(
    host: &FakeHost,
    run_id: i64,
    node_id: &'static str,
    kind: &'static str,
) -> Rc<RefCell<String>> {
    let seen = Rc::new(RefCell::new(String::new()));
    let probe = Rc::clone(&seen);
    *host.at_external.borrow_mut() = Some(Box::new(move || {
        *probe.borrow_mut() = effect_state(run_id, node_id, kind).unwrap_or_default();
    }));
    seen
}

#[test]
fn spawn_effect_claims_then_dispatches_once_and_a_closed_gate_does_neither() {
    let closed = Scratch::new().start(
        one_node("spawn", spawn_kind()),
        vec![running_step("spawn", None)],
        &[intent("spawn", EffectKind::Spawn)],
        CircuitContext::new(),
        Some("cancelled"),
    );
    let mut closed = closed;
    let closed_host = FakeHost::default();
    let blocked = dispatch(
        &mut closed,
        &closed_host,
        &[Effect::SpawnAgentNode {
            node_id: "spawn".into(),
        }],
    );
    assert!(blocked.outcomes.is_empty());
    assert!(closed_host.spawns.borrow().is_empty());
    assert_eq!(
        effect_state(closed.run_id, "spawn", "spawn").as_deref(),
        Some("intent")
    );

    let mut open = Scratch::new().start(
        one_node("spawn", spawn_kind()),
        vec![running_step("spawn", None)],
        &[intent("spawn", EffectKind::Spawn)],
        CircuitContext::new(),
        None,
    );
    let host = FakeHost::default();
    *host.spawn_error.borrow_mut() = Some("boom".into());
    let seen = probe_state(&host, open.run_id, "spawn", "spawn");
    let effect = Effect::SpawnAgentNode {
        node_id: "spawn".into(),
    };
    let pass = dispatch(&mut open, &host, std::slice::from_ref(&effect));
    assert_eq!(seen.borrow().as_str(), "possible_dispatch");
    assert_eq!(host.spawns.borrow().len(), 1);
    assert!(matches!(
        &pass.outcomes[..],
        [CircuitEvent::EffectUncertain { node_id, attempt: 1, reason }]
            if node_id == "spawn" && reason.contains("boom") && reason.contains("unverified")
    ));
    assert_eq!(
        effect_state(open.run_id, "spawn", "spawn").as_deref(),
        Some("possible_dispatch")
    );
    let again = dispatch(&mut open, &host, &[effect]);
    assert!(again.outcomes.is_empty());
    assert_eq!(host.spawns.borrow().len(), 1);
}

#[test]
fn inject_pty_effect_claims_then_writes_the_prompt() {
    let graph = one_node(
        "inject",
        CircuitNodeKind::InjectPty {
            prompt: "follow up".into(),
            target_node_id: Some("$source".into()),
        },
    );
    let closed_scratch = Scratch::new();
    let closed_agent = closed_scratch.agent();
    let mut closed_context = CircuitContext::new();
    closed_context.set("source.agent_id", closed_agent.id.to_string());
    let mut closed = closed_scratch.start(
        graph.clone(),
        vec![running_step("inject", None)],
        &[intent("inject", EffectKind::Prompt)],
        closed_context,
        Some("cancelled"),
    );
    let closed_host = FakeHost::default();
    *closed_host.input_stamp_value.borrow_mut() = Some("stamp-1".into());
    let blocked = dispatch(
        &mut closed,
        &closed_host,
        &[Effect::InjectPty {
            node_id: "inject".into(),
            prompt: "follow up".into(),
            target_node_id: Some("$source".into()),
        }],
    );
    assert!(blocked.outcomes.is_empty());
    assert!(closed_host.prompts.borrow().is_empty());
    assert_eq!(
        effect_state(closed.run_id, "inject", "prompt").as_deref(),
        Some("intent")
    );
    assert_ne!(
        db::get_agent_node_by_id(closed_agent.id).unwrap().status,
        SessionStatus::Running
    );

    let scratch = Scratch::new();
    let agent = scratch.agent();
    db::update_agent_node_status(agent.id, SessionStatus::Ready).unwrap();
    let mut context = CircuitContext::new();
    context.set("source.agent_id", agent.id.to_string());
    let mut open = scratch.start(
        graph,
        vec![running_step("inject", None)],
        &[intent("inject", EffectKind::Prompt)],
        context,
        None,
    );
    let host = FakeHost::default();
    *host.input_stamp_value.borrow_mut() = Some("stamp-1".into());
    let seen = probe_state(&host, open.run_id, "inject", "prompt");
    let pass = dispatch(
        &mut open,
        &host,
        &[Effect::InjectPty {
            node_id: "inject".into(),
            prompt: "follow up".into(),
            target_node_id: Some("$source".into()),
        }],
    );
    assert_eq!(seen.borrow().as_str(), "possible_dispatch");
    assert_eq!(host.prompts.borrow().len(), 1);
    assert_eq!(host.prompts.borrow()[0].agent_id, agent.id);
    assert!(host.prompts.borrow()[0].text.contains("follow up"));
    assert_eq!(
        host.prompts.borrow()[0].expected_input.as_deref(),
        Some("stamp-1")
    );
    assert!(matches!(
        &pass.outcomes[..],
        [CircuitEvent::PromptDelivered { node_id, attempt: 1 }] if node_id == "inject"
    ));
    assert_eq!(
        effect_state(open.run_id, "inject", "prompt").as_deref(),
        Some("acknowledged")
    );
    assert_eq!(
        db::get_agent_node_by_id(agent.id).unwrap().status,
        SessionStatus::Running
    );
    let kinds = history_kinds(open.run_id);
    let claimed = kinds
        .iter()
        .position(|kind| kind == "effect_possible_dispatch")
        .unwrap();
    let submitted = kinds
        .iter()
        .position(|kind| kind == "prompt_submitted")
        .unwrap();
    let acked = kinds
        .iter()
        .position(|kind| kind == "effect_result")
        .unwrap();
    assert!(claimed < submitted && submitted < acked);
}

fn claimed_delivery(agent_id: i64, prefix: &str, task_id: Option<&str>) -> CircuitContext {
    let stamp = pin_ready_turn(agent_id);
    let mut context = CircuitContext::new();
    context.set(&format!("node.spawn.{prefix}.delivery"), "claimed");
    context.set(&format!("node.spawn.{prefix}.stamp"), stamp);
    context.set(&format!("node.spawn.{prefix}.revision"), "rev-1");
    context.set(&format!("node.spawn.{prefix}.input"), "owned-input");
    if let Some(task_id) = task_id {
        context.set("node.spawn.nudge.task_id", task_id);
    }
    context
}

fn ready_host() -> FakeHost {
    let host = FakeHost::default();
    host.alive.set(true);
    *host.report_revision.borrow_mut() = Some("rev-1".into());
    host
}

#[test]
fn continue_effect_requires_a_claimed_delivery_before_the_write() {
    let graph = one_node("spawn", spawn_kind());
    let unclaimed_scratch = Scratch::new();
    let unclaimed_agent = unclaimed_scratch.agent();
    pin_ready_turn(unclaimed_agent.id);
    let mut unclaimed = unclaimed_scratch.start(
        graph.clone(),
        vec![running_step("spawn", Some(unclaimed_agent.id))],
        &[],
        CircuitContext::new(),
        None,
    );
    let unclaimed_host = ready_host();
    let skipped = dispatch(
        &mut unclaimed,
        &unclaimed_host,
        &[Effect::ContinueAgentTurn {
            node_id: "spawn".into(),
            target_agent_id: unclaimed_agent.id,
            prompt: "go on".into(),
        }],
    );
    assert!(skipped.outcomes.is_empty());
    assert!(unclaimed_host.prompts.borrow().is_empty());
    assert_eq!(
        db::get_agent_node_by_id(unclaimed_agent.id).unwrap().status,
        SessionStatus::Ready
    );

    let cancelled_scratch = Scratch::new();
    let cancelled_agent = cancelled_scratch.agent();
    let mut cancelled = cancelled_scratch.start(
        graph.clone(),
        vec![running_step("spawn", Some(cancelled_agent.id))],
        &[],
        claimed_delivery(cancelled_agent.id, "continuation", None),
        Some("cancelled"),
    );
    let cancelled_host = ready_host();
    let blocked = dispatch(
        &mut cancelled,
        &cancelled_host,
        &[Effect::ContinueAgentTurn {
            node_id: "spawn".into(),
            target_agent_id: cancelled_agent.id,
            prompt: "go on".into(),
        }],
    );
    assert!(blocked.outcomes.is_empty());
    assert!(cancelled_host.prompts.borrow().is_empty());
    assert_eq!(
        db::get_agent_node_by_id(cancelled_agent.id).unwrap().status,
        SessionStatus::Ready
    );

    let scratch = Scratch::new();
    let agent = scratch.agent();
    let mut open = scratch.start(
        graph,
        vec![running_step("spawn", Some(agent.id))],
        &[],
        claimed_delivery(agent.id, "continuation", None),
        None,
    );
    let host = ready_host();
    let pass = dispatch(
        &mut open,
        &host,
        &[Effect::ContinueAgentTurn {
            node_id: "spawn".into(),
            target_agent_id: agent.id,
            prompt: "go on".into(),
        }],
    );
    assert!(matches!(
        &pass.outcomes[..],
        [CircuitEvent::ContinuationDelivered { node_id, attempt: 1 }] if node_id == "spawn"
    ));
    assert_eq!(host.prompts.borrow().len(), 1);
    assert_eq!(
        host.prompts.borrow()[0].expected_input.as_deref(),
        Some("owned-input")
    );
    assert!(host.prompts.borrow()[0].text.contains("go on"));
    assert_eq!(
        db::get_agent_node_by_id(agent.id).unwrap().status,
        SessionStatus::Running
    );
    assert_eq!(effect_rows(open.run_id), 0);
}

/// Issue #1533: a busy reader pool **defers** the batch.
///
/// It does not fail the run — a transient shortage is not the run's fault —
/// and it does not leave the stepper's claim behind: a claim at `claimed`
/// is one the observation ignores, so the effect would be stranded rather
/// than retried. The deferred outcome releases the claim, and the batch
/// dispatches on the next pass.
#[test]
fn a_busy_pool_defers_the_batch_and_releases_the_claim() {
    let graph = one_node("spawn", spawn_kind());
    let scratch = Scratch::new();
    let agent = scratch.agent();
    let mut world = scratch.start(
        graph,
        vec![running_step("spawn", Some(agent.id))],
        &[],
        claimed_delivery(agent.id, "continuation", None),
        None,
    );
    let host = ready_host();

    // Hold the pool dry: every read now fails with the typed busy error.
    let held = crate::db::test_support::saturate_readers();

    let deferred = dispatch(
        &mut world,
        &host,
        &[Effect::ContinueAgentTurn {
            node_id: "spawn".into(),
            target_agent_id: agent.id,
            prompt: "go on".into(),
        }],
    );
    assert!(
        host.prompts.borrow().is_empty(),
        "a busy pool must not write a prompt"
    );
    assert!(
        matches!(
            &deferred.outcomes[..],
            [CircuitEvent::ContinuationDeferred { node_id, attempt: 1 }] if node_id == "spawn"
        ),
        "the batch is deferred and the claim released: {:?}",
        deferred.outcomes
    );

    // With the pool serving again, the same batch dispatches.
    drop(held);
    let pass = dispatch(
        &mut world,
        &host,
        &[Effect::ContinueAgentTurn {
            node_id: "spawn".into(),
            target_agent_id: agent.id,
            prompt: "go on".into(),
        }],
    );
    assert!(
        matches!(
            &pass.outcomes[..],
            [CircuitEvent::ContinuationDelivered { node_id, attempt: 1 }] if node_id == "spawn"
        ),
        "the deferred batch dispatches once the pool serves again: {:?}",
        pass.outcomes
    );
    assert_eq!(host.prompts.borrow().len(), 1);
}

#[test]
fn nudge_effect_requires_a_claimed_delivery_before_the_write() {
    let graph = one_node("spawn", spawn_kind());
    let unclaimed_scratch = Scratch::new();
    let unclaimed_agent = unclaimed_scratch.agent();
    pin_ready_turn(unclaimed_agent.id);
    let mut unclaimed = unclaimed_scratch.start(
        graph.clone(),
        vec![running_step("spawn", Some(unclaimed_agent.id))],
        &[],
        CircuitContext::new(),
        None,
    );
    let unclaimed_host = ready_host();
    *unclaimed_host.stalled_task.borrow_mut() = Some("task-7".into());
    let skipped = dispatch(
        &mut unclaimed,
        &unclaimed_host,
        &[Effect::NudgeIdleAgent {
            node_id: "spawn".into(),
            target_agent_id: unclaimed_agent.id,
            prompt: "wake".into(),
        }],
    );
    assert!(skipped.outcomes.is_empty());
    assert!(unclaimed_host.prompts.borrow().is_empty());

    let cancelled_scratch = Scratch::new();
    let cancelled_agent = cancelled_scratch.agent();
    let mut cancelled = cancelled_scratch.start(
        graph.clone(),
        vec![running_step("spawn", Some(cancelled_agent.id))],
        &[],
        claimed_delivery(cancelled_agent.id, "nudge", Some("task-7")),
        Some("cancelled"),
    );
    let cancelled_host = ready_host();
    *cancelled_host.stalled_task.borrow_mut() = Some("task-7".into());
    let blocked = dispatch(
        &mut cancelled,
        &cancelled_host,
        &[Effect::NudgeIdleAgent {
            node_id: "spawn".into(),
            target_agent_id: cancelled_agent.id,
            prompt: "wake".into(),
        }],
    );
    assert!(blocked.outcomes.is_empty());
    assert!(cancelled_host.prompts.borrow().is_empty());
    assert_eq!(
        db::get_agent_node_by_id(cancelled_agent.id).unwrap().status,
        SessionStatus::Ready
    );

    let scratch = Scratch::new();
    let agent = scratch.agent();
    let mut open = scratch.start(
        graph,
        vec![running_step("spawn", Some(agent.id))],
        &[],
        claimed_delivery(agent.id, "nudge", Some("task-7")),
        None,
    );
    let host = ready_host();
    *host.stalled_task.borrow_mut() = Some("task-7".into());
    let pass = dispatch(
        &mut open,
        &host,
        &[Effect::NudgeIdleAgent {
            node_id: "spawn".into(),
            target_agent_id: agent.id,
            prompt: "wake".into(),
        }],
    );
    assert!(matches!(
        &pass.outcomes[..],
        [CircuitEvent::NudgeDelivered { node_id, attempt: 1 }] if node_id == "spawn"
    ));
    assert_eq!(host.prompts.borrow().len(), 1);
    assert_eq!(
        host.prompts.borrow()[0].expected_input.as_deref(),
        Some("owned-input")
    );
    assert!(history_kinds(open.run_id)
        .iter()
        .any(|kind| kind == "prompt_submitted"));
    assert_eq!(effect_rows(open.run_id), 0);
    assert_eq!(
        db::get_agent_node_by_id(agent.id).unwrap().status,
        SessionStatus::Running
    );
}

#[test]
fn set_node_status_effect_does_not_dispatch_and_the_gate_stops_the_batch() {
    let effects = [
        Effect::SetNodeStatus {
            node_id: "status".into(),
            status: "completed".into(),
            target_node_id: None,
        },
        Effect::Notify {
            message: "shipped".into(),
        },
    ];
    let graph = one_node(
        "status",
        CircuitNodeKind::SetNodeStatus {
            status: crate::circuit::model::SessionStatusKind::Completed,
            target_node_id: None,
        },
    );
    let closed_scratch = Scratch::new();
    let closed_agent = closed_scratch.agent();
    db::update_agent_node_status(closed_agent.id, SessionStatus::Ready).unwrap();
    let mut closed = closed_scratch.start(
        graph.clone(),
        vec![running_step("status", None)],
        &[],
        CircuitContext::new(),
        Some("cancelled"),
    );
    let closed_host = FakeHost::default();
    let blocked = dispatch(&mut closed, &closed_host, &effects);
    assert!(blocked.outcomes.is_empty());
    assert!(closed_host.notifications.borrow().is_empty());
    assert_eq!(effect_rows(closed.run_id), 0);
    assert_eq!(
        db::get_agent_node_by_id(closed_agent.id).unwrap().status,
        SessionStatus::Ready
    );

    let scratch = Scratch::new();
    let agent = scratch.agent();
    db::update_agent_node_status(agent.id, SessionStatus::Ready).unwrap();
    let mut open = scratch.start(
        graph,
        vec![running_step("status", None)],
        &[],
        CircuitContext::new(),
        Some("completed"),
    );
    open.view.state = RunState::Completed;
    let host = FakeHost::default();
    let pass = dispatch(&mut open, &host, &effects);
    assert!(pass.outcomes.is_empty());
    assert_eq!(host.notifications.borrow().len(), 1);
    assert_eq!(host.notifications.borrow()[0].1, "shipped");
    assert_eq!(host.notifications.borrow()[0].2, "info");
    assert_eq!(effect_rows(open.run_id), 0);
    assert_eq!(
        db::get_agent_node_by_id(agent.id).unwrap().status,
        SessionStatus::Ready
    );
}

#[test]
fn close_agent_effect_deletes_the_agent_only_while_the_gate_is_open() {
    let graph = CircuitGraph {
        version: CIRCUIT_GRAPH_VERSION,
        blueprint: None,
        nodes: vec![
            CircuitNode {
                id: "spawn".into(),
                kind: spawn_kind(),
            },
            CircuitNode {
                id: "close".into(),
                kind: CircuitNodeKind::CloseAgentNode {
                    target_node_id: Some("spawn".into()),
                },
            },
        ],
        edges: vec![CircuitEdge {
            from: "spawn".into(),
            to: "close".into(),
            condition: Default::default(),
        }],
    };
    let effect = Effect::CloseAgentNode {
        node_id: "close".into(),
        target_node_id: Some("spawn".into()),
    };
    let closed_scratch = Scratch::new();
    let closed_agent = closed_scratch.agent();
    let mut closed = closed_scratch.start(
        graph.clone(),
        vec![
            running_step("spawn", Some(closed_agent.id)),
            running_step("close", None),
        ],
        &[],
        CircuitContext::new(),
        Some("cancelled"),
    );
    let closed_host = FakeHost::default();
    let blocked = dispatch(&mut closed, &closed_host, std::slice::from_ref(&effect));
    assert!(blocked.outcomes.is_empty());
    assert!(closed_host.notifications.borrow().is_empty());
    assert!(db::get_agent_node_by_id(closed_agent.id).is_ok());
    assert_eq!(step_agent(closed.run_id, "spawn"), Some(closed_agent.id));

    let scratch = Scratch::new();
    let agent = scratch.agent();
    let mut open = scratch.start(
        graph,
        vec![
            running_step("spawn", Some(agent.id)),
            running_step("close", None),
        ],
        &[],
        CircuitContext::new(),
        None,
    );
    let host = FakeHost::default();
    let pass = dispatch(&mut open, &host, &[effect]);
    assert!(pass.outcomes.is_empty());
    assert!(
        host.notifications.borrow().is_empty(),
        "a clean close does not warn: {:?}",
        host.notifications.borrow()
    );
    assert!(db::get_agent_node_by_id(agent.id).is_err());
    assert_eq!(step_agent(open.run_id, "spawn"), None);
    assert!(open.view.step("spawn").unwrap().agent_node_id.is_none());
}

#[test]
fn notify_effect_emits_only_while_the_gate_is_open() {
    let graph = one_node(
        "note",
        CircuitNodeKind::Notify {
            message: "shipped".into(),
        },
    );
    let effect = Effect::Notify {
        message: "shipped".into(),
    };
    let mut closed = Scratch::new().start(
        graph.clone(),
        vec![running_step("note", None)],
        &[],
        CircuitContext::new(),
        Some("cancelled"),
    );
    let closed_host = FakeHost::default();
    let blocked = dispatch(&mut closed, &closed_host, std::slice::from_ref(&effect));
    assert!(blocked.outcomes.is_empty());
    assert!(closed_host.notifications.borrow().is_empty());

    let mut open = Scratch::new().start(
        graph,
        vec![running_step("note", None)],
        &[],
        CircuitContext::new(),
        None,
    );
    let host = FakeHost::default();
    let pass = dispatch(&mut open, &host, &[effect]);
    assert!(pass.outcomes.is_empty());
    assert_eq!(
        host.notifications.borrow().as_slice(),
        &[(open.run_id, "shipped".into(), "info".into(),)]
    );
    assert_eq!(effect_rows(open.run_id), 0);
}

#[test]
fn github_effect_claims_then_records_an_uncertain_dispatch() {
    let graph = one_node(
        "label",
        CircuitNodeKind::GithubAction {
            action: GithubActionKind::AddLabel,
            open_pr_policy: None,
            label: Some("triaged".into()),
            comment: None,
        },
    );
    let effect = Effect::CallGithub {
        node_id: "label".into(),
        action: GithubActionKind::AddLabel,
        label: Some("triaged".into()),
        comment: None,
    };
    let mut closed = Scratch::new().start(
        graph.clone(),
        vec![running_step("label", None)],
        &[intent("label", EffectKind::Github)],
        CircuitContext::new(),
        Some("cancelled"),
    );
    let closed_host = FakeHost::default();
    let blocked = dispatch(&mut closed, &closed_host, std::slice::from_ref(&effect));
    assert!(blocked.outcomes.is_empty());
    assert!(closed_host.pr_ready.borrow().is_empty());
    assert_eq!(
        effect_state(closed.run_id, "label", "github").as_deref(),
        Some("intent")
    );

    let mut open = Scratch::new().start(
        graph,
        vec![running_step("label", None)],
        &[intent("label", EffectKind::Github)],
        CircuitContext::new(),
        None,
    );
    let host = FakeHost::default();
    let pass = dispatch(&mut open, &host, &[effect]);
    assert!(host.pr_ready.borrow().is_empty());
    assert_eq!(
        effect_state(open.run_id, "label", "github").as_deref(),
        Some("possible_dispatch")
    );
    let [CircuitEvent::EffectUncertain {
        node_id,
        attempt: 1,
        reason,
    }] = &pass.outcomes[..]
    else {
        panic!("expected one uncertain outcome, got {:?}", pass.outcomes);
    };
    assert_eq!(node_id, "label");
    assert!(
        reason.contains("git error")
            || reason.contains("no `origin` remote")
            || reason.contains("isn't a GitHub URL"),
        "missing origin fails before a GitHub client is built: {reason}"
    );
}
