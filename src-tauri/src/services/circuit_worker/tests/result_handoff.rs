// Result-file handoff for a harness with no lifecycle of its own (Freebuff,
// Perch, DeepSeek Harness). Included in the existing tests scope so the gate
// fixture is shared with the other review-report cases.

fn file_only_implementer() -> crate::models::AgentNode {
    crate::models::AgentNode {
        id: 900,
        provider: "freebuff".into(),
        status: SessionStatus::Ready,
        cli_session_id: Some("freebuff-session".into()),
        ..Default::default()
    }
}

fn file_only_candidate(
    view: &RunView,
    agent: &crate::models::AgentNode,
    result: &std::path::Path,
) -> Result<
    Option<(readiness::Candidate, bool)>,
    crate::circuit::observation::CircuitObservationBlocker,
> {
    // No transcript and no native receipt: the ordinary path cannot bind.
    let Err(crate::circuit::observation::CircuitObservationBlocker::ReportUnavailable { .. }) =
        readiness::prepare(
            view,
            "finish_classifier",
            agent,
            Some("100:ready"),
            Ok("input-1".into()),
            Err(crate::services::transcript_reader::report_snapshot::ReportReadError::Unsupported),
        )
    else {
        panic!("a yielded harness without a report adapter has no ordinary report");
    };
    let Some(candidate) = readiness::prepare_result_handoff(
        view,
        "finish_classifier",
        agent,
        Some("100:ready"),
        Ok("input-1".into()),
        result,
        chrono::Utc::now().timestamp_millis(),
    )?
    else {
        return Ok(None);
    };
    candidate.with_result_file(result, view, view.step("finish_classifier").unwrap())
}

#[test]
fn file_only_harness_hands_off_through_its_result_file_and_completes_the_gate() {
    assert!(super::result_handoff::completes_by_result_file_only(
        &file_only_implementer()
    ));
    let directory = tempfile::tempdir().unwrap();
    let result = directory.path().join("finish-attempt1.result.md");
    let report = "Opened the pull request and verified the build.\nBUILDMESH_HANDOFF_V1: READY";
    std::fs::write(&result, report).unwrap();
    let mut view = report_gate_view();
    let agent = file_only_implementer();

    let (candidate, missing) = file_only_candidate(&view, &agent, &result)
        .unwrap()
        .expect("a yielded agent binds its result file");
    assert!(!missing);
    assert_eq!(candidate.status, SessionStatus::Ready);
    assert_eq!(candidate.output, report);
    let binding = candidate.binding.clone();
    assert!(binding.report_revision.starts_with(&format!(
        "{}:result:",
        super::result_handoff::turn_revision(&result)
    )));
    assert_eq!(binding.input_guard.input_stamp, "input-1");
    assert_eq!(
        binding.input_guard.observed_at_ms,
        super::result_handoff::written_at_ms(&result).unwrap()
    );

    // The contract line decides without inference, and the stepper accepts
    // the file-backed binding as current report evidence.
    let classification = classify_gate_report(
        &view,
        "finish_classifier",
        candidate.status,
        &candidate.output,
        |_| panic!("a declared result line needs no classifier"),
    );
    assert_eq!(
        classification,
        Some(crate::circuit::evaluator::Classification::Completed)
    );
    let mut events = Vec::new();
    let mut source = crate::services::circuit_worker::observe_parity_tests::Script::default();
    source.turn = Some(ClassifiedTurn {
        classifier_error: None,
        observation_blocker: None,
        binding: Some(binding),
        agent_node_id: 900,
        classification,
        output: candidate.output,
        continuation: None,
        waiting_for_a_finished_turn: false,
        missing_result: None,
    });
    observe_gates_with(&view, &mut events, &mut source);
    for event in &events {
        advance(&mut view, event);
    }
    let gate = view.step("finish_classifier").unwrap();
    assert_eq!(gate.status, StepStatus::Completed, "{:?}", gate.error);
}

#[test]
fn file_only_handoff_waits_for_a_finished_turn_and_a_valid_result_line() {
    let directory = tempfile::tempdir().unwrap();
    let result = directory.path().join("finish-attempt1.result.md");
    let view = report_gate_view();

    // Still Running: only the watchdog's settled-file recovery may end it.
    let running = crate::models::AgentNode {
        status: SessionStatus::Running,
        ..file_only_implementer()
    };
    assert!(readiness::prepare_result_handoff(
        &view,
        "finish_classifier",
        &running,
        Some("100:ready"),
        Ok("input-1".into()),
        &result,
        0,
    )
    .unwrap()
    .is_none());

    // A finished turn with no file, or with a report that lacks the contract
    // line, takes the bounded reminder path instead of a guessed verdict.
    let agent = file_only_implementer();
    let (_, missing) = file_only_candidate(&view, &agent, &result)
        .unwrap()
        .unwrap();
    assert!(missing, "a missing file is owed");
    std::fs::write(&result, "Done, I think.").unwrap();
    let (_, missing) = file_only_candidate(&view, &agent, &result)
        .unwrap()
        .unwrap();
    assert!(missing, "a report without its result line is still owed");

    // Input ownership still fences the binding.
    std::fs::write(&result, "Done.\nBUILDMESH_HANDOFF_V1: READY").unwrap();
    assert_eq!(
        readiness::prepare_result_handoff(
            &view,
            "finish_classifier",
            &agent,
            Some("100:ready"),
            Err(crate::agent::process::InputUnavailable::Draft),
            &result,
            0,
        )
        .err(),
        Some(crate::circuit::observation::CircuitObservationBlocker::InputDraft)
    );
}
