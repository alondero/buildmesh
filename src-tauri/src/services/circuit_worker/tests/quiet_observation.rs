// Readiness checks that must not raise a checkpoint for an agent that is
// simply starting up or working. Included in the existing tests scope so they
// share the review-report gate fixture.

fn gate_agent(status: SessionStatus) -> crate::models::AgentNode {
    crate::models::AgentNode {
        id: 900,
        provider: "codex".into(),
        status,
        cli_session_id: Some("codex-session".into()),
        ..Default::default()
    }
}

fn readiness_with_missing_process(
    status: SessionStatus,
) -> Result<Option<readiness::Candidate>, crate::circuit::observation::CircuitObservationBlocker> {
    readiness::prepare(
        &report_gate_view(),
        "finish_classifier",
        &gate_agent(status),
        Some("100:ready"),
        Err(crate::agent::process::InputUnavailable::MissingProcess),
        Err(crate::services::transcript_reader::report_snapshot::ReportReadError::Unsupported),
    )
}

/// Spawned steps routinely went Unverified ("The agent process is
/// unavailable") in the same millisecond they were spawned, because the
/// terminal had not registered yet (297 such sub-5-second episodes in a month
/// of the stable ledger). A launch still in progress is not a lost process.
#[test]
fn a_process_that_is_still_launching_is_not_a_checkpoint() {
    for status in [SessionStatus::Pending, SessionStatus::Spawning] {
        assert!(
            matches!(readiness_with_missing_process(status), Ok(None)),
            "{status:?} is still launching"
        );
    }
}

/// A process that should exist but does not (suspended after a restart,
/// closed, or failed) still needs a person.
#[test]
fn a_missing_process_after_launch_is_still_a_checkpoint() {
    for status in [
        SessionStatus::Running,
        SessionStatus::Ready,
        SessionStatus::Suspended,
        SessionStatus::Error,
    ] {
        assert!(
            matches!(
                readiness_with_missing_process(status),
                Err(crate::circuit::observation::CircuitObservationBlocker::ProcessUnavailable)
            ),
            "{status:?} has no live process"
        );
    }
}

fn readiness_for_busy_agent(
    view: &RunView,
) -> Result<Option<readiness::Candidate>, crate::circuit::observation::CircuitObservationBlocker> {
    readiness::prepare(
        view,
        "finish_classifier",
        &gate_agent(SessionStatus::Running),
        Some("100:ready"),
        Ok("input-1".into()),
        Err(crate::services::transcript_reader::report_snapshot::ReportReadError::Unsupported),
    )
}

/// Keeping a held step Running must not change what Buildmesh decides: for a
/// busy agent the gate gives the held step the same answer it gives the same
/// step marked Unverified, instead of skipping it as not ready.
#[test]
fn a_running_step_held_by_an_observation_blocker_is_rechecked_like_an_unverified_one() {
    let idle = report_gate_view();
    assert!(
        matches!(readiness_for_busy_agent(&idle), Ok(None)),
        "a busy agent with nothing recorded is simply not ready yet"
    );

    let mut held = report_gate_view();
    held.context.set(
        "node.finish_classifier.observation_blocker",
        serde_json::to_string(
            &crate::circuit::observation::CircuitObservationBlocker::ReportUnavailable {
                reason: "newer input or tool activity follows the assistant report".into(),
            },
        )
        .unwrap(),
    );
    let mut unverified = held.clone();
    unverified.step_mut("finish_classifier").unwrap().status = StepStatus::Unverified;

    let held_result = readiness_outcome(readiness_for_busy_agent(&held));
    assert_ne!(held_result, "not ready", "the held step is re-examined");
    assert_eq!(
        held_result,
        readiness_outcome(readiness_for_busy_agent(&unverified))
    );
}

fn readiness_outcome(
    result: Result<
        Option<readiness::Candidate>,
        crate::circuit::observation::CircuitObservationBlocker,
    >,
) -> String {
    match result {
        Ok(None) => "not ready".into(),
        Ok(Some(candidate)) => format!("candidate {}", candidate.turn_revision),
        Err(blocker) => format!("blocked {blocker:?}"),
    }
}

/// A settled foreground turn whose owned work the harness cannot report
/// (Codex, Antigravity) is likewise re-examined while the step stays Running.
#[test]
fn a_settled_turn_with_an_ownership_gap_is_rechecked_while_running() {
    let mut view = report_gate_view();
    view.context.set(
        "node.finish_classifier.evidence.1",
        serde_json::to_string(&crate::circuit::observation::WorkEvidence {
            foreground_terminated: true,
            lifecycle_invalidated: true,
            ..Default::default()
        })
        .unwrap(),
    );
    assert!(!matches!(readiness_for_busy_agent(&view), Ok(None)));
}
