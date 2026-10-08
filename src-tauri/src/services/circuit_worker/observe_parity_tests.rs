//! Literal event snapshot captured before the observer modules were extracted.
//! The scripted source runs the production fan-out, rather than a copy of it.
use super::observation::{
    observe_agent_projection_with, observe_with, Observations, WaitObservation, ACTIVE_WAIT_MS,
    YIELDED_WAIT_MS,
};
use super::turn_classify::{ClassifiedTurn, MissingResult};
use super::*;
use crate::circuit::evaluator::Classification;
use crate::circuit::model::{
    CircuitEdge, CircuitNode, EdgeCondition, GithubActionKind, CIRCUIT_GRAPH_VERSION,
};

#[derive(Default)]
pub(super) struct Script {
    native: Vec<CircuitEvent>,
    clock_reads: usize,
    status: Option<SessionStatus>,
    lookup_error: bool,
    ack: Option<bool>,
    alive: bool,
    pub(super) turn: Option<ClassifiedTurn>,
    observed: bool,
    approvals: Vec<String>,
    calls: Vec<&'static str>,
}

impl Observations for Script {
    fn native_events(&mut self, _: &RunView) -> Vec<CircuitEvent> {
        self.calls.push("native");
        std::mem::take(&mut self.native)
    }
    fn agent(&mut self, id: i64) -> db::SqlResult<crate::models::AgentNode> {
        self.calls.push("agent");
        if self.lookup_error {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.status
            .map(|status| crate::models::AgentNode {
                id,
                status,
                ..Default::default()
            })
            .ok_or(rusqlite::Error::QueryReturnedNoRows)
    }
    fn project_agent(
        &mut self,
        view: &RunView,
        step: &StepView,
        agent: &crate::models::AgentNode,
        events: &mut Vec<CircuitEvent>,
    ) {
        self.calls.push("projection");
        observe_agent_projection_with(
            view,
            step,
            agent,
            db::agent_node::AgentStatusObservation {
                status: agent.status,
                session_incarnation: None,
                session_id: None,
                observed_at_ms: 100,
                source_revision: "status:1".into(),
            },
            || self.alive,
            events,
        );
    }
    fn codex(
        &mut self,
        _: &RunView,
        _: &StepView,
        _: &crate::models::AgentNode,
    ) -> Option<CircuitEvent> {
        None
    }
    fn error_tail(&mut self, _: i64) -> String {
        "process failed".into()
    }
    fn has_github_target(&mut self, _: &RunView, _: &StepView) -> bool {
        true
    }
    fn prompt_acknowledged(&mut self, _: &RunView, _: &StepView) -> db::SqlResult<bool> {
        self.calls.push("ack");
        self.ack.ok_or(rusqlite::Error::InvalidQuery)
    }
    fn alive(&mut self, _: i64) -> bool {
        self.alive
    }
    fn classify(&mut self, _: &RunView, _: &StepView) -> Option<ClassifiedTurn> {
        self.calls.push("classify");
        self.turn.take()
    }
    fn verify(&mut self, _: &RunView, _: &StepView, _: &str) -> Option<bool> {
        self.calls.push("verify");
        Some(true)
    }
    fn blocked(&mut self, _: i64, _: i64) {
        self.calls.push("blocked");
    }
    fn approvals(&mut self, _: i64) -> Vec<String> {
        std::mem::take(&mut self.approvals)
    }
    fn capacity(&mut self) -> CircuitEvent {
        self.calls.push("capacity");
        CircuitEvent::Tick(crate::circuit::stepper::Capacity {
            agent_free_slots: 2,
        })
    }
    fn now_ms(&mut self) -> i64 {
        self.clock_reads += 1;
        500
    }
    fn wait(&mut self, _: &RunView, _: &StepView, _: i64) -> Option<WaitObservation> {
        self.calls.push("wait");
        Some(WaitObservation {
            progress: self.observed.then(|| "report:1".into()),
            observed: self.observed,
            yielded: true,
            yielded_budget_ms: YIELDED_WAIT_MS,
            active_budget_ms: ACTIVE_WAIT_MS,
        })
    }
}

pub(super) fn view(
    kind: CircuitNodeKind,
    state: RunState,
    status: StepStatus,
    attached: Option<i64>,
) -> RunView {
    RunView {
        run_id: 1876,
        state,
        context: CircuitContext::default(),
        graph: CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![CircuitNode {
                id: "step".into(),
                kind,
            }],
            edges: vec![],
        },
        steps: vec![StepView {
            node_id: "step".into(),
            status,
            attempt: 1,
            agent_node_id: attached,
            outcome: None,
            error: None,
        }],
    }
}

pub(super) fn spawn() -> CircuitNodeKind {
    CircuitNodeKind::SpawnAgentNode {
        prompt: "do work".into(),
        name: None,
        provider: None,
        model: None,
        effort: None,
        extra_args: None,
        timeout_seconds: Some(7),
    }
}

fn turn(classification: Option<Classification>, parked: bool) -> ClassifiedTurn {
    ClassifiedTurn {
        classifier_error: classification
            .is_none()
            .then(|| "classifier offline".into()),
        observation_blocker: None,
        binding: None,
        agent_node_id: 42,
        classification,
        output: "review report".into(),
        continuation: None,
        waiting_for_a_finished_turn: parked,
        missing_result: None,
    }
}

#[test]
fn scripted_observe_event_parity() {
    let mut scenarios = Vec::new();
    scenarios.push((
        "pending",
        view(spawn(), RunState::Pending, StepStatus::Queued, None),
        Script::default(),
    ));
    scenarios.push((
        "uncertain spawn",
        view(spawn(), RunState::Running, StepStatus::Running, None),
        Script::default(),
    ));
    for (name, status, lookup_error) in [
        ("missing agent", None, false),
        ("lookup failure", None, true),
        ("lost agent", Some(SessionStatus::Lost), false),
        ("failed agent", Some(SessionStatus::Error), false),
        ("working agent", Some(SessionStatus::Running), false),
    ] {
        scenarios.push((
            name,
            view(spawn(), RunState::Running, StepStatus::Running, Some(42)),
            Script {
                status,
                lookup_error,
                ..Default::default()
            },
        ));
    }
    for (name, ack) in [
        ("acknowledged prompt", Some(true)),
        ("uncertain prompt", Some(false)),
        ("ack lookup failure", None),
    ] {
        let mut run = view(
            CircuitNodeKind::InjectPty {
                target_node_id: Some("$source".into()),
                prompt: "feedback".into(),
            },
            RunState::Running,
            StepStatus::Running,
            None,
        );
        run.context.set("source.agent_id", "42");
        run.context.set("node.step.prompt_delivery.1", "intent");
        scenarios.push((
            name,
            run,
            Script {
                ack,
                status: Some(SessionStatus::Ready),
                alive: true,
                observed: true,
                ..Default::default()
            },
        ));
    }
    for (name, verdict, parked) in [
        ("mid-work reviewer", Some(Classification::Working), true),
        ("needs input", Some(Classification::Blocked), false),
        ("classifier outage", None, false),
    ] {
        scenarios.push((
            name,
            view(
                CircuitNodeKind::ReviewVerdict {
                    target_node_id: Some("$source".into()),
                },
                RunState::Running,
                StepStatus::Running,
                Some(42),
            ),
            Script {
                status: Some(SessionStatus::AwaitingInput),
                observed: true,
                turn: Some(turn(verdict, parked)),
                ..Default::default()
            },
        ));
    }
    let mut continuation = view(spawn(), RunState::Running, StepStatus::Running, Some(42));
    continuation
        .context
        .set("node.step.continuation.attempt", "1");
    continuation
        .context
        .set("node.step.continuation.delivery", "claimed");
    scenarios.push((
        "uncertain continuation",
        continuation.clone(),
        Script::default(),
    ));
    continuation
        .context
        .set("node.step.continuation.delivery", "pending");
    scenarios.push(("retry continuation", continuation, Script::default()));
    let mut approval = view(
        CircuitNodeKind::CollaboratorCheck {
            require_approval: true,
        },
        RunState::Running,
        StepStatus::Blocked,
        None,
    );
    approval.context.set("autopilot.collaborator_gate", "auto");
    scenarios.push((
        "approvals",
        approval,
        Script {
            approvals: vec!["queued-approval".into()],
            ..Default::default()
        },
    ));
    scenarios.push((
        "verification",
        view(
            CircuitNodeKind::DeterministicVerification {
                command: "check".into(),
            },
            RunState::Running,
            StepStatus::Running,
            None,
        ),
        Script::default(),
    ));
    scenarios.push((
        "remote recheck",
        view(
            CircuitNodeKind::GithubAction {
                action: GithubActionKind::OpenPr,
                label: None,
                comment: None,
                open_pr_policy: None,
            },
            RunState::Running,
            StepStatus::Unverified,
            None,
        ),
        Script::default(),
    ));
    let mut cleanup = view(
        CircuitNodeKind::CloseAgentNode {
            target_node_id: Some("spawn".into()),
        },
        RunState::Running,
        StepStatus::Completed,
        None,
    );
    cleanup.graph.nodes.push(CircuitNode {
        id: "spawn".into(),
        kind: spawn(),
    });
    cleanup.graph.edges.push(CircuitEdge {
        from: "spawn".into(),
        to: "step".into(),
        condition: EdgeCondition::Always,
    });
    cleanup.steps.push(StepView {
        node_id: "spawn".into(),
        status: StepStatus::Completed,
        attempt: 1,
        agent_node_id: Some(42),
        outcome: None,
        error: None,
    });
    scenarios.push(("cleanup failure retry", cleanup.clone(), Script::default()));
    cleanup.steps[1].agent_node_id = None;
    scenarios.push(("cleanup acknowledged", cleanup, Script::default()));
    scenarios.push((
        "paused",
        view(spawn(), RunState::Paused, StepStatus::Running, Some(42)),
        Script {
            status: Some(SessionStatus::Running),
            ..Default::default()
        },
    ));
    scenarios.push((
        "cancelled",
        view(
            spawn(),
            RunState::Cancelled,
            StepStatus::Cancelled,
            Some(42),
        ),
        Script::default(),
    ));

    let mut snapshot = String::new();
    for (name, run, mut script) in scenarios {
        let events = observe_with(&run, &mut script);
        snapshot.push_str(&format!(
            "{name}\n{events:#?}\ncalls: {:?}\n\n",
            script.calls
        ));
    }
    assert_eq!(snapshot.trim(), include_str!("observe_parity.snap").trim());
}

#[test]
fn wait_clock_is_read_only_for_running_runs() {
    for state in [
        RunState::Pending,
        RunState::Paused,
        RunState::Cancelled,
        RunState::Running,
    ] {
        let run = view(spawn(), state, StepStatus::Queued, None);
        let mut script = Script::default();
        observe_with(&run, &mut script);
        assert_eq!(script.clock_reads, usize::from(state == RunState::Running));
    }
}

#[test]
fn native_receipts_precede_trigger_and_capacity_events() {
    let run = view(spawn(), RunState::Pending, StepStatus::Queued, None);
    let mut script = Script {
        native: vec![CircuitEvent::Paused],
        ..Default::default()
    };
    let events = observe_with(&run, &mut script);
    assert_eq!(events.len(), 3);
    assert!(matches!(
        &events[..],
        [
            CircuitEvent::Paused,
            CircuitEvent::Triggered,
            CircuitEvent::Tick(_)
        ]
    ));
}

#[test]
fn a_finished_turn_missing_its_result_file_is_reminded_and_never_classified() {
    let mut view = view(
        CircuitNodeKind::LlmTurnClassifier {
            target_node_id: None,
        },
        RunState::Running,
        StepStatus::Running,
        Some(42),
    );
    // The classifier's step is reminded through the spawn that owns the agent.
    view.graph.nodes.push(CircuitNode {
        id: "work".into(),
        kind: spawn(),
    });
    view.graph.edges.push(CircuitEdge {
        from: "work".into(),
        to: "step".into(),
        condition: EdgeCondition::default(),
    });
    view.steps.push(StepView {
        node_id: "work".into(),
        status: StepStatus::Completed,
        agent_node_id: Some(42),
        attempt: 1,
        outcome: None,
        error: None,
    });
    let owed = |turn_missing: Option<MissingResult>| ClassifiedTurn {
        classifier_error: None,
        observation_blocker: None,
        binding: None,
        agent_node_id: 42,
        classification: None,
        output: "transcript report".into(),
        continuation: None,
        waiting_for_a_finished_turn: false,
        missing_result: turn_missing,
    };
    let missing = || MissingResult {
        path_for_agent: "C:/runs/result.md".into(),
        stamp: "100:yield".into(),
        revision: "report-1".into(),
        input_stamp: "1:0".into(),
    };

    let mut script = Script {
        turn: Some(owed(Some(missing()))),
        ..Default::default()
    };
    let mut events = Vec::new();
    super::turn_classify::observe_gates_with(&view, &mut events, &mut script);
    assert_eq!(events.len(), 1, "only the reminder is observed");
    assert!(matches!(
        &events[0],
        CircuitEvent::ResultFileMissing {
            node_id,
            attempt: 1,
            result_path,
            stamp,
            revision,
            input_stamp,
        } if node_id == "step"
            && result_path == "C:/runs/result.md"
            && stamp == "100:yield"
            && revision == "report-1"
            && input_stamp == "1:0"
    ));
    assert!(
        !script.calls.contains(&"blocked"),
        "a first reminder raises no attention"
    );

    // Two reminders are spent for this attempt. The report revision is the
    // same as the last reminder, but this is a new turn (new lifecycle stamp)
    // that still lacks the file: the step is exhausted, so the agent is marked
    // for attention exactly once.
    view.context.set("node.step.result_reminders.1", "2");
    view.context
        .set("node.step.result_reminder_revision", "report-1");
    view.context
        .set("node.step.result_reminder_stamp", "100:earlier-turn");
    let mut script = Script {
        turn: Some(owed(Some(missing()))),
        ..Default::default()
    };
    let mut events = Vec::new();
    super::turn_classify::observe_gates_with(&view, &mut events, &mut script);
    assert_eq!(script.calls.iter().filter(|c| **c == "blocked").count(), 1);
    assert!(matches!(
        events.as_slice(),
        [CircuitEvent::ResultFileMissing { .. }]
    ));

    // The same observation (revision and stamp) already answered is not
    // raised again.
    view.context
        .set("node.step.result_reminder_stamp", "100:yield");
    let mut script = Script {
        turn: Some(owed(Some(missing()))),
        ..Default::default()
    };
    let mut events = Vec::new();
    super::turn_classify::observe_gates_with(&view, &mut events, &mut script);
    assert!(!script.calls.contains(&"blocked"));
}
