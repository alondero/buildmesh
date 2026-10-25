// Circuit worker result delivery tests. Included in the existing tests scope.
#[test]
fn a_wake_up_stranded_by_a_crash_is_settled_without_a_stall_being_observed() {
    // Round-2 finding: recovery used to hang off `NudgeDecision::Abandon`,
    // reachable only when a task stall was also observed — so a stranded claim
    // left the step Running until the wait budget expired. Recovery is now
    // driven by the claim alone, with no transcript fact required.
    let mut fixture = ResultReadFixture::new();
    let node = fixture.view.steps[1].node_id.clone();
    let attempt = fixture.view.steps[1].attempt;
    fixture
        .view
        .context
        .set(&format!("node.{node}.nudge.delivery"), "claimed");
    fixture
        .view
        .context
        .set(&format!("node.{node}.nudge.attempt"), attempt.to_string());

    let (events, _, _) = fixture.probe();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, CircuitEvent::NudgeUncertain { .. })),
        "a claim stranded at `claimed` must be settled by observation alone: {events:?}"
    );
    assert_eq!(
        fixture
            .view
            .context
            .get(&format!("node.{node}.nudge.delivery")),
        Some("uncertain"),
        "the claim is settled rather than left waiting forever"
    );
    assert!(
        fixture.view.steps[1]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("will not be replayed")),
        "the operator is told the wake-up may or may not have landed"
    );
}

#[test]
fn a_wake_up_in_flight_stops_the_turn_being_classified() {
    // Round-2 finding: continuations halt classification while their delivery
    // is claimed; the nudge did not, so a turn could be judged before its
    // wake-up had landed.
    let mut fixture = ResultReadFixture::new();
    let node = fixture.view.steps[1].node_id.clone();
    let attempt = fixture.view.steps[1].attempt;
    fixture
        .view
        .context
        .set(&format!("node.{node}.nudge.delivery"), "claimed");
    fixture
        .view
        .context
        .set(&format!("node.{node}.nudge.attempt"), attempt.to_string());

    let (events, _, _) = fixture.probe();
    assert!(
        !events.iter().any(|event| matches!(
            event,
            CircuitEvent::TurnClassified { .. }
                | CircuitEvent::ClassifierErrorObserved { .. }
                | CircuitEvent::ClassifierUnavailable { .. }
        )),
        "a turn must not be classified while a wake-up is in flight: {events:?}"
    );
}

#[test]
fn a_claimed_wake_up_for_another_attempt_does_not_block_classification() {
    // The guard is attempt-scoped: a claim left over from a previous attempt
    // must not wedge the current one.
    let mut fixture = ResultReadFixture::new();
    let node = fixture.view.steps[1].node_id.clone();
    fixture
        .view
        .context
        .set(&format!("node.{node}.nudge.delivery"), "claimed");
    fixture
        .view
        .context
        .set(&format!("node.{node}.nudge.attempt"), "99");

    let (events, _, _) = fixture.probe();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, CircuitEvent::NudgeUncertain { .. })),
        "a stale claim from another attempt is not this step's problem: {events:?}"
    );
}

#[test]
fn result_unstable_publication_retries_without_reminders_or_attention() {
    use crate::services::transcript_reader::report_snapshot::result_read_test_support;

    let mut fixture = ResultReadFixture::new();
    // Start with a spent reminder: instability must neither spend nor reset it.
    fixture
        .view
        .context
        .set("node.implementation_classifier.result_reminders.1", "1");
    let before = fixture.view.context.to_json().unwrap();
    for probe in 1..=6 {
        std::fs::write(&fixture.result, "Publishing result\n").unwrap();
        let expected_path = fixture.result.clone();
        let (events, effects, attention) = result_read_test_support::with_after_read(
            move |path| {
                assert_eq!(path, expected_path);
                std::fs::write(
                    path,
                    format!("Changed publication {probe}\nBUILDMESH_HANDOFF_V1: READY\n"),
                )
                .unwrap();
            },
            || fixture.probe(),
        );
        assert!(
            events.is_empty(),
            "unstable reads must publish no events: {events:?}"
        );
        assert!(
            effects.is_empty(),
            "unstable reads must deliver no reminder"
        );
        assert_eq!(attention, 0, "unstable reads must never raise needs-input");
        assert_eq!(fixture.view.steps[1].status, StepStatus::Running);
        assert_eq!(fixture.view.context.to_json().unwrap(), before);
        assert_eq!(
            evaluator::millis_since_last_evaluation(fixture.agent_id),
            None,
            "retry must return before classification or reminder evaluation"
        );
    }
    let report = "Stable completed result\nBUILDMESH_HANDOFF_V1: READY\n";
    std::fs::write(&fixture.result, report).unwrap();
    let (events, effects, attention) = fixture.probe();
    assert!(events
        .iter()
        .any(|event| matches!(event, CircuitEvent::TurnClassified {
        classification: Some(evaluator::Classification::Completed), output: Some(output), ..
    } if output == report)));
    assert!(effects.iter().all(|effect| !matches!(
        effect,
        crate::circuit::stepper::Effect::ContinueAgentTurn { .. }
    )));
    assert_eq!(attention, 0);
    assert_eq!(fixture.view.steps[1].status, StepStatus::Completed);
    assert_eq!(
        fixture
            .view
            .context
            .get("node.implementation_classifier.result_reminders.1"),
        Some("1")
    );
    assert_eq!(
        fixture
            .view
            .context
            .get("node.implementation_classifier.result_read_failures.1"),
        None
    );
}

#[test]
fn result_missing_and_blank_keep_bounded_reminders_through_production_classifier() {
    for content in [None, Some(" \n\t")] {
        let mut fixture = ResultReadFixture::new();
        if let Some(content) = content {
            std::fs::write(&fixture.result, content).unwrap();
        }
        for turn in 1..=3 {
            if turn > 1 {
                advance(
                    &mut fixture.view,
                    &CircuitEvent::ContinuationDelivered {
                        node_id: "implementation_classifier".into(),
                        attempt: 1,
                    },
                );
                db::write_conn()
                    .execute(
                        "UPDATE agent_nodes SET status_changed_at=?2 WHERE id=?1",
                        rusqlite::params![fixture.agent_id, format!("turn-{turn}")],
                    )
                    .unwrap();
            }
            let (events, effects, attention) = fixture.probe();
            assert!(matches!(
                events.as_slice(),
                [CircuitEvent::ResultFileMissing { .. }]
            ));
            assert_eq!(effects.len(), usize::from(turn <= 2));
            if turn <= 2 {
                assert!(matches!(
                    effects[0],
                    crate::circuit::stepper::Effect::ContinueAgentTurn { .. }
                ));
            }
            assert_eq!(attention, usize::from(turn == 3));
        }
        assert_eq!(
            fixture
                .view
                .context
                .get("node.implementation_classifier.result_reminders.1"),
            Some("2")
        );
        assert_eq!(fixture.view.steps[1].status, StepStatus::Unverified);
        let (_, effects, attention) = fixture.probe();
        assert!(effects.is_empty());
        assert_eq!(attention, 0);
    }
}

#[test]
fn result_without_required_decision_is_repaired_before_inference() {
    let mut fixture = ResultReadFixture::new();
    std::fs::write(
        &fixture.result,
        "Fixes committed and pushed. No blockers.\n",
    )
    .unwrap();
    let (events, effects, attention) = fixture.probe();
    assert!(matches!(
        events.as_slice(),
        [CircuitEvent::ResultFileMissing { .. }]
    ));
    assert!(matches!(
        effects.as_slice(),
        [crate::circuit::stepper::Effect::ContinueAgentTurn { .. }]
    ));
    assert_eq!(attention, 0);
    assert_eq!(
        fixture
            .view
            .context
            .get("node.implementation_classifier.classifier_failures.1"),
        None
    );
}

#[test]
fn unverified_owned_step_can_deliver_its_one_background_wake() {
    let mut fixture = ResultReadFixture::new();
    let gate = "implementation_classifier";
    fixture.view.step_mut(gate).unwrap().status = StepStatus::Unverified;
    let mut events = Vec::new();
    observe_waits_with(&fixture.view, &mut events, 1_000, |_, _| {
        Some(WaitObservation {
            progress: Some("report:1".into()),
            observed: true,
            yielded: true,
            yielded_budget_ms: 30_000,
            active_budget_ms: ACTIVE_WAIT_MS,
            stalled_task_id: Some("bg-finished".into()),
            fence: Some(TurnFence {
                stamp: "1:".into(),
                revision: "report:1".into(),
            }),
        })
    });
    assert!(events
        .iter()
        .any(|event| matches!(event, CircuitEvent::NudgeIdleSession { .. })));
    assert!(!events
        .iter()
        .any(|event| matches!(event, CircuitEvent::WaitObserved { .. })));
    let mut effects = Vec::new();
    for event in events {
        effects.extend(advance(&mut fixture.view, &event).effects);
    }
    assert!(matches!(
        effects.as_slice(),
        [crate::circuit::stepper::Effect::NudgeIdleAgent { .. }]
    ));
    assert_eq!(fixture.view.step(gate).unwrap().status, StepStatus::Running);
    advance(
        &mut fixture.view,
        &CircuitEvent::NudgeDelivered {
            node_id: gate.into(),
            attempt: 1,
        },
    );
    assert_eq!(
        fixture
            .view
            .context
            .get("node.implementation_classifier.nudge.delivery"),
        Some("delivered")
    );
    assert_eq!(
        fixture
            .view
            .nudge_decision(gate, 1, "new-report", "new-turn"),
        crate::circuit::stepper::NudgeDecision::Exhausted
    );
}

#[test]
fn a_busy_reader_pool_leaves_the_turn_fence_unknown() {
    let fixture = ResultReadFixture::new();
    let node = db::get_agent_node_by_id(fixture.agent_id).unwrap();
    // Every reader is checked out, so the lifecycle stamp cannot be read.
    let readers = crate::db::test_support::saturate_readers();
    assert!(
        turn_fence(&node, Some("report:1")).is_none(),
        "a stamp that cannot be read must leave the fence unknown, not empty"
    );
    drop(readers);
    // The same fence resolves once a reader is free again: the unknown fence
    // was a shortage, not the end of the turn.
    let fence = turn_fence(&node, Some("report:1")).expect("the stamp resolves with a free reader");
    assert_eq!(fence.revision, "report:1");
    assert!(
        !fence.stamp.is_empty(),
        "a resolved fence carries its stamp"
    );
}

/// Issue #1533: a wake-up whose fence is unknown is never claimed, so the
/// attempt keeps its one wake for a later observation pass. Claiming it would
/// spend the nudge budget on a wake the dispatch side can only reject as
/// obsolete, and the finished background task would go unannounced until the
/// wait times out.
#[test]
fn an_unknown_fence_leaves_the_background_wake_claimable() {
    let mut fixture = ResultReadFixture::new();
    let gate = "implementation_classifier";
    fixture.view.step_mut(gate).unwrap().status = StepStatus::Unverified;
    let mut events = Vec::new();
    observe_waits_with(&fixture.view, &mut events, 1_000, |_, _| {
        Some(WaitObservation {
            progress: Some("report:1".into()),
            observed: true,
            yielded: true,
            yielded_budget_ms: 30_000,
            active_budget_ms: ACTIVE_WAIT_MS,
            stalled_task_id: Some("bg-finished".into()),
            fence: None,
        })
    });
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, CircuitEvent::NudgeIdleSession { .. })),
        "a wake-up with an unknown fence must not be claimed"
    );
    // Nothing was claimed, so the attempt still has its one wake.
    assert_eq!(
        fixture.view.nudge_decision(gate, 1, "report:1", "1:"),
        crate::circuit::stepper::NudgeDecision::Nudge
    );
    // A claim that carries an empty half is refused outright: the dispatch
    // side re-reads both halves and could never match it.
    assert_eq!(
        fixture.view.nudge_decision(gate, 1, "", "1:"),
        crate::circuit::stepper::NudgeDecision::Ignore
    );
    assert_eq!(
        fixture.view.nudge_decision(gate, 1, "report:1", ""),
        crate::circuit::stepper::NudgeDecision::Ignore
    );
}

#[test]
fn background_wake_rechecks_task_identity_activity_and_human_waits() {
    let mut fixture = ResultReadFixture::new();
    let gate = "implementation_classifier";
    fixture
        .view
        .context
        .set(&format!("node.{gate}.nudge.stamp"), "turn");
    fixture
        .view
        .context
        .set(&format!("node.{gate}.nudge.revision"), "report");
    fixture
        .view
        .context
        .set(&format!("node.{gate}.nudge.task_id"), "bg_finished");
    let check = |status, alive, stamp, revision, quiet_ms, task_id, lifecycle_allows_wake| {
        background_wake_is_current(
            &fixture.view,
            gate,
            fixture.agent_id,
            BackgroundWakeEvidence {
                status,
                alive,
                stamp,
                revision,
                quiet_ms,
                task_id,
                lifecycle_allows_wake,
            },
        )
    };
    assert!(check(
        SessionStatus::Running,
        true,
        Some("turn"),
        Some("report"),
        Some(60_000),
        Some("bg_finished"),
        true
    ));
    assert!(check(
        SessionStatus::Ready,
        true,
        Some("turn"),
        Some("report"),
        None,
        Some("bg_finished"),
        true
    ));
    for (status, alive, stamp, revision, quiet, task, permitted) in [
        (
            SessionStatus::Running,
            true,
            Some("turn"),
            Some("report"),
            Some(0),
            Some("bg_finished"),
            true,
        ),
        (
            SessionStatus::Ready,
            false,
            Some("turn"),
            Some("report"),
            None,
            Some("bg_finished"),
            true,
        ),
        (
            SessionStatus::Ready,
            true,
            Some("new-turn"),
            Some("report"),
            None,
            Some("bg_finished"),
            true,
        ),
        (
            SessionStatus::Ready,
            true,
            Some("turn"),
            Some("new-report"),
            None,
            Some("bg_finished"),
            true,
        ),
        (
            SessionStatus::Ready,
            true,
            Some("turn"),
            Some("report"),
            None,
            None,
            true,
        ),
        (
            SessionStatus::Ready,
            true,
            Some("turn"),
            Some("report"),
            None,
            Some("bg_other"),
            true,
        ),
        (
            SessionStatus::Ready,
            true,
            Some("turn"),
            Some("report"),
            None,
            Some("bg_finished"),
            false,
        ),
        (
            SessionStatus::AwaitingInput,
            true,
            Some("turn"),
            Some("report"),
            None,
            Some("bg_finished"),
            true,
        ),
    ] {
        assert!(!check(
            status, alive, stamp, revision, quiet, task, permitted
        ));
    }
}
