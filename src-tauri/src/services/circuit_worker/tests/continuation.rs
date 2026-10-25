// Circuit worker continuation tests. Included in the existing tests scope.
#[test]
fn circuit_continuation_rejects_user_input_regeneration_and_new_report() {
    let mut view = report_gate_view();
    view.context
        .set("node.finish_classifier.continuation.stamp", "100:yield");
    view.context
        .set("node.finish_classifier.continuation.revision", "report-1");
    let valid = |status, stamp, revision| {
        continuation_is_current(
            &view,
            "finish_classifier",
            900,
            status,
            true,
            Some(stamp),
            Some(revision),
        )
    };
    assert!(valid(SessionStatus::Ready, "100:yield", "report-1"));
    // A finished turn that owes a result file is reminded as Completed, so the
    // reminder must still be accepted when its delivery is confirmed current.
    assert!(valid(SessionStatus::Completed, "100:yield", "report-1"));
    assert!(!valid(SessionStatus::Running, "100:yield", "report-1"));
    assert!(!valid(SessionStatus::Ready, "200:yield", "report-1"));
    assert!(!valid(SessionStatus::Ready, "100:new-input", "report-1"));
    assert!(!valid(SessionStatus::Ready, "100:yield", "report-2"));
}

#[test]
fn a_wake_up_is_fenced_against_its_own_claim_not_the_continuation_s() {
    // Issue #2105: the nudge keeps its own `nudge.*` delivery bookkeeping. If
    // the shared fence read the continuation's keys, every wake-up would be
    // judged obsolete the moment the two lifecycles diverged, and the stall
    // would never actually be broken.
    let mut view = report_gate_view();
    let node_id = "finish_classifier";
    view.context
        .set(&format!("node.{node_id}.nudge.stamp"), "100:yield");
    view.context
        .set(&format!("node.{node_id}.nudge.revision"), "report-1");
    let target = 900;
    let fence = |stamp: &str, revision: &str| {
        continuation_is_current_for(
            &view,
            node_id,
            target,
            ContinuationTurn {
                status: SessionStatus::Ready,
                alive: true,
                stamp: Some(stamp),
                revision: Some(revision),
            },
            "nudge",
        )
    };
    assert!(
        fence("100:yield", "report-1"),
        "the nudge's own claim must satisfy the fence"
    );
    assert!(!fence("200:yield", "report-1"), "a new turn invalidates it");
    assert!(
        !fence("100:yield", "report-2"),
        "a new report invalidates it"
    );
    // The continuation's keys are a different lifecycle and must not satisfy a
    // wake-up's fence, nor vice versa.
    assert!(
        !continuation_is_current(
            &view,
            node_id,
            target,
            SessionStatus::Ready,
            true,
            Some("100:yield"),
            Some("report-1")
        ),
        "a continuation must not read the nudge's claim as its own"
    );
    view.context
        .set(&format!("node.{node_id}.continuation.stamp"), "100:yield");
    view.context
        .set(&format!("node.{node_id}.continuation.revision"), "report-1");
    assert!(
        continuation_is_current(
            &view,
            node_id,
            target,
            SessionStatus::Ready,
            true,
            Some("100:yield"),
            Some("report-1")
        ),
        "the continuation keeps its own fence"
    );
}

#[test]
fn circuit_continuation_accepts_only_the_agent_a_spawn_step_owns() {
    let spawn_node = CircuitNode {
        id: "implement".into(),
        kind: crate::circuit::model::CircuitNodeKind::SpawnAgentNode {
            prompt: "implement".into(),
            name: None,
            provider: None,
            model: None,
            effort: None,
            extra_args: None,
            timeout_seconds: None,
        },
    };
    let mut view = RunView {
        run_id: 27,
        graph: crate::circuit::model::CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![spawn_node],
            edges: vec![],
        },
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![StepView {
            node_id: "implement".into(),
            status: StepStatus::Running,
            agent_node_id: Some(900),
            attempt: 1,
            outcome: None,
            error: None,
        }],
    };
    view.context
        .set("node.implement.continuation.stamp", "100:yield");
    view.context
        .set("node.implement.continuation.revision", "report-1");
    let valid = |target| {
        continuation_is_current(
            &view,
            "implement",
            target,
            SessionStatus::Ready,
            true,
            Some("100:yield"),
            Some("report-1"),
        )
    };
    assert!(valid(900));
    assert!(!valid(901));
}

#[test]
fn circuit_report_dedupe_reconsiders_identical_text_from_a_new_native_turn() {
    let mut view = report_gate_view();
    let report = "Still working";
    advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "finish_classifier".into(),
            classification: Some(crate::circuit::evaluator::Classification::Working),
            output: Some(report.into()),
        },
    );
    assert!(!should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        report,
        None
    ));
    let old_revision = view
        .context
        .get("node.finish_classifier.classified_report_revision")
        .unwrap()
        .to_string();
    let binding = crate::circuit::test_support::record_report_evidence_for_turn(
        &mut view,
        "finish_classifier",
        report,
        "new-native-turn",
    );
    assert_eq!(
        binding.report_revision, old_revision,
        "same text digest, different turn identity"
    );
    assert!(should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        report,
        None
    ));
    assert!(has_unconsumed_classifier_evidence(
        &view,
        "finish_classifier"
    ));
    advance(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: Some(binding),
            node_id: "finish_classifier".into(),
            classification: Some(crate::circuit::evaluator::Classification::Working),
            output: Some(report.into()),
        },
    );
    assert!(!has_unconsumed_classifier_evidence(
        &view,
        "finish_classifier"
    ));
}

#[test]
fn circuit_report_dedupe_is_durable_and_scoped_to_gate_attempt() {
    use crate::circuit::evaluator::Classification;
    let mut view = report_gate_view();
    let report = "Still working; waiting for the requested credentials.";
    // The implementation gate consumed the same agent's output. That
    // global clock must not suppress this finish gate's first evaluation.
    assert!(should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        report,
        Some(0)
    ));
    advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "finish_classifier".into(),
            classification: Some(Classification::Working),
            output: Some(report.into()),
        },
    );
    view.context = CircuitContext::from_json(&view.context.to_json().unwrap()).unwrap();
    assert!(
        !should_classify_report(
            &view,
            "finish_classifier",
            SessionStatus::AwaitingInput,
            report,
            None
        ),
        "restart must not reclassify a consumed report"
    );
    assert!(
        should_classify_report(
            &view,
            "finish_classifier",
            SessionStatus::AwaitingInput,
            "Wrap-up complete",
            None
        ),
        "recover a new transcript without PTY clocks"
    );
    view.steps[0].attempt += 1;
    assert!(
        should_classify_report(
            &view,
            "finish_classifier",
            SessionStatus::AwaitingInput,
            report,
            Some(0)
        ),
        "a new round may produce an identical report"
    );
    view.state = RunState::Paused;
    assert!(!should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        report,
        None
    ));
}

#[test]
fn circuit_classifier_failure_retries_silent_turn_and_persists_reason() {
    let mut view = report_gate_view();
    let report = "Wrap-up complete: all tests passed and the PR is open.";
    let result = advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "finish_classifier".into(),
            classification: None,
            output: Some(report.into()),
        },
    );
    assert!(result.step_writes[0]
        .error
        .as_ref()
        .and_then(|error| error.as_ref())
        .is_some_and(|error| error.contains("retrying")));
    assert!(!should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        report,
        Some(59_999)
    ));
    assert!(should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        report,
        Some(60_000)
    ));
    assert!(
        should_classify_report(
            &view,
            "finish_classifier",
            SessionStatus::AwaitingInput,
            report,
            None
        ),
        "restart permits recovery"
    );
    advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "finish_classifier".into(),
            classification: Some(crate::circuit::evaluator::Classification::Completed),
            output: Some(report.into()),
        },
    );
    assert_eq!(
        view.step("finish_classifier").unwrap().status,
        StepStatus::Completed
    );
    assert!(view.step("finish_classifier").unwrap().error.is_none());
    assert_eq!(
        view.step("open_pr").map(|step| step.status),
        Some(StepStatus::Running),
        "{:?}",
        view.steps
    );
    assert!(!should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        report,
        None
    ));
}
