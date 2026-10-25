// Circuit worker classifier retry tests. Included in the existing tests scope.
/// A transient classifier outage on a *finished* reviewer's report must be
/// retried, not parked. The reviewer has finished, so it produces no further
/// output: a park that only recorded the observation would leave
/// `should_classify_report` refusing that report forever and the gate would
/// stall until its wait deadline. Reporting the outage through the existing
/// channel records `classification = "unavailable"`, which is what admits
/// the cooldown retry.
#[test]
fn reviewer_classifier_outage_is_retried_after_the_cooldown() {
    let mut view = RunView {
        run_id: 27,
        graph: CircuitGraph::agent_review(None, None, 3),
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![StepView {
            node_id: "verdict".into(),
            status: StepStatus::Running,
            agent_node_id: Some(1),
            attempt: 1,
            outcome: None,
            error: None,
        }],
    };
    let report = "Findings: none. Verdict: approved.";
    // The decision that makes the retry possible: an outage is reported
    // through the classification channel, not parked.
    assert!(
        ReviewerReadiness::Working.parks(),
        "a working reviewer parks"
    );
    assert!(
        !ReviewerReadiness::Unavailable.parks(),
        "an outage must be reported, not parked: a finished reviewer produces no new output, \
         so a parked report would never be re-observed and its 60-second retry would never fire"
    );
    assert!(!ReviewerReadiness::Reportable.parks());
    // What `classify_step_turn` publishes for `ReviewerReadiness::Unavailable`.
    advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "verdict".into(),
            classification: None,
            output: Some(report.into()),
        },
    );
    assert_eq!(
        view.context.get("node.verdict.classification"),
        Some("unavailable"),
        "an outage must be recorded, or nothing ever retries it"
    );
    assert!(
        !should_classify_report(
            &view,
            "verdict",
            SessionStatus::AwaitingInput,
            report,
            Some(59_000)
        ),
        "the cooldown still holds inside 60 seconds"
    );
    assert!(
        should_classify_report(
            &view,
            "verdict",
            SessionStatus::AwaitingInput,
            report,
            Some(60_001)
        ),
        "the unfinished verdict must be retried once the cooldown expires, even though the \
         finished reviewer produces no new output"
    );
    // A deliberate park is the opposite case: nothing failed, so no outage
    // is recorded and the unchanged report stays unobserved.
    let mut parked = RunView {
        run_id: 27,
        graph: CircuitGraph::agent_review(None, None, 3),
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![StepView {
            node_id: "verdict".into(),
            status: StepStatus::Running,
            agent_node_id: Some(1),
            attempt: 1,
            outcome: None,
            error: None,
        }],
    };
    advance(
        &mut parked,
        &CircuitEvent::TurnParked {
            report_revision: None,
            node_id: "verdict".into(),
            output: report.into(),
        },
    );
    assert_eq!(parked.context.get("node.verdict.classification"), None);
    assert!(
        !should_classify_report(
            &parked,
            "verdict",
            SessionStatus::AwaitingInput,
            report,
            Some(600_000)
        ),
        "a working reviewer is not re-observed on the cooldown"
    );
}

/// The park's recorded observation is what bounds the readiness question:
/// an unchanged report is not observed again, so a reviewer that sits
/// mid-turn is not re-classified on every tick. The next report is.
#[test]
fn unverified_classifier_outage_keeps_its_retry_cooldown() {
    let mut view = report_gate_view();
    let step = view
        .steps
        .iter_mut()
        .find(|step| step.node_id == "finish_classifier")
        .unwrap();
    step.status = StepStatus::Unverified;
    view.context.set(
        "node.finish_classifier.evaluated_attempt",
        step.attempt.to_string(),
    );
    view.context
        .set("node.finish_classifier.evaluated_output", "Finished.");
    view.context
        .set("node.finish_classifier.classification", "unavailable");
    assert!(!should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        "Finished.",
        Some(10_000)
    ));
    assert!(should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        "Finished.",
        Some(60_001)
    ));
}

/// A gate whose unchanged report already failed `failures` classifier calls,
/// the last with `error`, as `record_classifier_failure` leaves its context.
fn classifier_outage_view(failures: u32, error: &str) -> RunView {
    let mut view = report_gate_view();
    view.step_mut("finish_classifier").unwrap().status = StepStatus::Unverified;
    for (key, value) in [
        ("evaluated_attempt", "1".to_string()),
        ("evaluated_output", "Finished.".to_string()),
        ("classification", "unavailable".to_string()),
        ("classifier_failures.1", failures.to_string()),
        ("classifier_error.1", error.to_string()),
    ] {
        view.context
            .set(&format!("node.finish_classifier.{key}"), value);
    }
    view
}

fn mcode_timeout() -> String {
    format!("mcode: {}", evaluator::CLASSIFIER_TIMEOUT_MESSAGE)
}

fn report_retry_is_due(view: &RunView, since_evaluation_ms: u128) -> bool {
    should_classify_report(
        view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        "Finished.",
        Some(since_evaluation_ms),
    )
}

/// Run 399: five minute-spaced timeouts parked the gate during a 28-minute
/// provider stall. Each further timeout now waits longer before the next call.
#[test]
fn repeated_classifier_timeouts_widen_the_report_retry_cooldown() {
    for (failures, wait_ms) in [(1, 60_000), (2, 180_000), (3, 600_000), (4, 1_200_000)] {
        let view = classifier_outage_view(failures, &mcode_timeout());
        assert!(
            !report_retry_is_due(&view, wait_ms - 1),
            "after {failures} timeouts the retry is not due {} ms in",
            wait_ms - 1
        );
        assert!(
            report_retry_is_due(&view, wait_ms),
            "after {failures} timeouts the retry is due at {wait_ms} ms"
        );
    }
}

#[test]
fn a_classifier_failure_that_is_not_a_timeout_keeps_the_fast_retry_cadence() {
    let view = classifier_outage_view(
        4,
        "mcode: classifier exited with exit code: 4: not available",
    );
    assert!(!report_retry_is_due(&view, 59_999));
    assert!(report_retry_is_due(&view, 60_000));
}

#[test]
fn the_quiet_turn_watchdog_honours_the_same_timeout_back_off() {
    let fresh = report_gate_view();
    assert!(quiet_evaluation_is_due(&fresh, "finish_classifier", None));
    assert!(!quiet_evaluation_is_due(
        &fresh,
        "finish_classifier",
        Some(59_999)
    ));
    assert!(quiet_evaluation_is_due(
        &fresh,
        "finish_classifier",
        Some(60_000)
    ));

    let timed_out = classifier_outage_view(2, &mcode_timeout());
    assert!(!quiet_evaluation_is_due(
        &timed_out,
        "finish_classifier",
        Some(179_999)
    ));
    assert!(quiet_evaluation_is_due(
        &timed_out,
        "finish_classifier",
        Some(180_000)
    ));
    assert!(quiet_evaluation_is_due(
        &timed_out,
        "finish_classifier",
        None
    ));

    let failed = classifier_outage_view(2, "mcode: authentication failed");
    assert!(quiet_evaluation_is_due(
        &failed,
        "finish_classifier",
        Some(60_000)
    ));
}

/// Review of PR 2178: after a restart the in-memory evaluation clock is gone
/// and `None` meant "retry now", so a restart inside a 10-minute cooldown
/// spent another failure from the budget during the same outage.
#[test]
fn a_restart_does_not_forget_a_timeout_cooldown() {
    let now_ms: i64 = 10_000_000;
    let mut view = classifier_outage_view(3, &mcode_timeout());
    view.context.set(
        "node.finish_classifier.classifier_failed_at_ms.1",
        (now_ms - 300_000).to_string(),
    );
    let elapsed = classifier_retry_elapsed_ms(&view, "finish_classifier", None, now_ms);
    assert_eq!(elapsed, Some(300_000));
    assert!(!should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        "Finished.",
        elapsed
    ));
    assert!(!quiet_evaluation_is_due(
        &view,
        "finish_classifier",
        elapsed
    ));

    let later = classifier_retry_elapsed_ms(&view, "finish_classifier", None, now_ms + 300_000);
    assert!(should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        "Finished.",
        later
    ));
    assert!(quiet_evaluation_is_due(&view, "finish_classifier", later));

    // A live in-memory clock is more recent than the persisted one.
    assert_eq!(
        classifier_retry_elapsed_ms(&view, "finish_classifier", Some(42), now_ms),
        Some(42)
    );
    // A clock set back never turns a cooldown into an instant retry.
    assert_eq!(
        classifier_retry_elapsed_ms(&view, "finish_classifier", None, now_ms - 400_000),
        Some(0)
    );
    // Contexts saved before the failure time existed keep today's behaviour.
    let legacy = classifier_outage_view(3, &mcode_timeout());
    assert_eq!(
        classifier_retry_elapsed_ms(&legacy, "finish_classifier", None, now_ms),
        None
    );
}

#[test]
fn circuit_classifier_exhaustion_survives_restart_and_new_reports() {
    let mut view = report_gate_view();
    for _ in 0..5 {
        advance(
            &mut view,
            &CircuitEvent::TurnClassified {
                binding: None,
                node_id: "finish_classifier".into(),
                classification: None,
                output: Some("Finished.".into()),
            },
        );
    }
    assert_eq!(
        view.step("finish_classifier").unwrap().status,
        StepStatus::Unverified
    );
    view.context = CircuitContext::from_json(&view.context.to_json().unwrap()).unwrap();
    for status in [SessionStatus::Ready, SessionStatus::AwaitingInput] {
        for output in ["Finished.", "New finished report."] {
            assert!(
                !should_classify_report(&view, "finish_classifier", status, output, None),
                "an exhausted classifier must wait for an explicit recheck"
            );
        }
    }
    view.context
        .set("node.finish_classifier.classifier_failures.1", "0");
    view.context
        .set("node.finish_classifier.evaluated_attempt", "");
    assert!(should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::Ready,
        "Finished.",
        None
    ));
}

#[test]
fn native_report_outage_keeps_cooldown_without_consuming_a_verdict() {
    let mut view = report_gate_view();
    view.step_mut("finish_classifier").unwrap().status = StepStatus::Unverified;
    let report = "Finished.";
    let binding = crate::circuit::test_support::record_report_evidence_for_turn(
        &mut view,
        "finish_classifier",
        report,
        "finished-native-turn",
    );
    let transition = advance(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: Some(binding.clone()),
            node_id: "finish_classifier".into(),
            classification: None,
            output: Some(report.into()),
        },
    );
    assert!(transition.classifications[0].lifecycle_verified);
    assert_eq!(
        view.context.get("node.finish_classifier.classification"),
        Some("unavailable")
    );
    assert_eq!(
        view.context
            .get("node.finish_classifier.evaluated_report_revision"),
        Some(binding.report_revision.as_str())
    );
    assert!(view
        .context
        .get("node.finish_classifier.classified_evidence_owner")
        .is_none());
    assert!(has_unconsumed_classifier_evidence(
        &view,
        "finish_classifier"
    ));
    assert!(!should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::Ready,
        report,
        Some(10_000)
    ));
    assert!(should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::Ready,
        report,
        Some(60_000)
    ));
    view.context = CircuitContext::from_json(&view.context.to_json().unwrap()).unwrap();
    assert!(should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::Ready,
        report,
        None
    ));
    assert!(should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::Ready,
        "A different report",
        Some(10_000)
    ));
}

#[test]
fn parked_observation_is_not_reclassified_until_the_report_changes() {
    let mut view = RunView {
        run_id: 27,
        graph: CircuitGraph::agent_review(None, None, 3),
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![StepView {
            node_id: "verdict".into(),
            status: StepStatus::Running,
            agent_node_id: Some(1),
            attempt: 1,
            outcome: None,
            error: None,
        }],
    };
    let progress = "PowerShell NativeCommandError. Let me retry with the standard `.cmd` shim.";
    advance(
        &mut view,
        &CircuitEvent::TurnParked {
            report_revision: None,
            node_id: "verdict".into(),
            output: progress.into(),
        },
    );
    assert!(
        !should_classify_report(
            &view,
            "verdict",
            SessionStatus::AwaitingInput,
            progress,
            None
        ),
        "an unchanged parked report must not be observed again"
    );
    assert!(
        should_classify_report(
            &view,
            "verdict",
            SessionStatus::AwaitingInput,
            "Findings: none. Verdict: approved.",
            None
        ),
        "the reviewer's next report must be observed"
    );
}
