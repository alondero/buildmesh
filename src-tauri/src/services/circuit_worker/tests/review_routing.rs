// Circuit worker review routing tests. Included in the existing tests scope.
#[test]
fn structured_review_routes_without_classifier() {
    use crate::circuit::evaluator::Classification;
    let mut view = report_gate_view();
    view.graph = CircuitGraph::agent_review(None, None, 3);
    for (verdict, expected) in [
        ("APPROVE", Classification::Completed),
        ("REQUEST_CHANGES", Classification::Working),
        ("BLOCKED", Classification::Blocked),
    ] {
        let report = format!("Review details and verification.\nBUILDMESH_REVIEW_V1: {verdict}");
        for status in [
            SessionStatus::Ready,
            SessionStatus::Completed,
            SessionStatus::AwaitingInput,
        ] {
            assert_eq!(
                reviewer_readiness(&view, "verdict", status, &report, |_| panic!(
                    "structured final review needs no readiness inference"
                )),
                ReviewerReadiness::Reportable
            );
            assert_eq!(
                classify_gate_report(&view, "verdict", status, &report, |_| panic!(
                    "structured verdict needs no inference"
                )),
                Some(expected)
            );
        }
    }
}

#[test]
fn circuit_issue_handoffs_route_without_classifier_after_dispatch() {
    use crate::circuit::evaluator::Classification;
    let view = report_gate_view();
    for (dispatch, gate) in [
        ("implementer", "implementation_classifier"),
        ("finish", "finish_classifier"),
        ("wrapup_correction", "finish_classifier"),
        ("follow_feedback", "feedback_classifier"),
    ] {
        assert!(
            report_contract::prompt(&view, dispatch, "Do the assigned phase")
                .contains("BUILDMESH_HANDOFF_V1: READY")
        );
        for (value, expected) in [
            ("READY", Classification::Completed),
            ("BLOCKED", Classification::Blocked),
        ] {
            let report =
                format!("Changes and verification for this phase.\nBUILDMESH_HANDOFF_V1: {value}");
            for status in [
                SessionStatus::Ready,
                SessionStatus::Completed,
                SessionStatus::AwaitingInput,
            ] {
                assert_eq!(
                    reviewer_readiness(&view, gate, status, &report, |_| panic!(
                        "explicit phase report needs no readiness inference"
                    )),
                    ReviewerReadiness::Reportable
                );
                assert_eq!(
                    classify_gate_report(&view, gate, status, &report, |_| panic!(
                        "explicit phase report needs no inference"
                    )),
                    Some(expected)
                );
            }
        }
    }
    let report = "Implementation details.\nBUILDMESH_HANDOFF_V1: READY";
    assert_eq!(
        reviewer_readiness(
            &view,
            "implementer",
            SessionStatus::AwaitingInput,
            report,
            |_| panic!("explicit first-turn report needs no readiness inference")
        ),
        ReviewerReadiness::Reportable
    );
    assert_eq!(
        classify_gate_report(
            &view,
            "implementer",
            SessionStatus::Ready,
            report,
            |_| panic!("spawn hands the report to its gate")
        ),
        Some(Classification::Completed)
    );
}

#[test]
fn circuit_agent_lookup_error_does_not_report_a_closed_node() {
    assert!(
        agent_lookup_for_observation::<crate::models::AgentNode>(
            4730,
            Err(rusqlite::Error::InvalidQuery),
        )
        .is_err(),
        "tick and startup must distinguish an unavailable database from an absent node"
    );
    assert!(agent_lookup_for_observation::<crate::models::AgentNode>(
        4730,
        Err(rusqlite::Error::QueryReturnedNoRows),
    )
    .unwrap()
    .is_none());
}

#[test]
fn review_verdict_falls_back_without_classifier_backend() {
    use crate::circuit::evaluator::Classification;
    let mut view = report_gate_view();
    view.graph = CircuitGraph::agent_review(None, None, 3);
    view.context.set("source.review_preset", "1");
    // Absent backend: the classifier yields nothing, so the gate reads
    // the report's explicit verdict instead of parking.
    let absent_backend = |_: &str| -> Option<Classification> { None };
    assert_eq!(
        classify_gate_report(
            &view,
            "verdict",
            SessionStatus::Ready,
            "Approved. No remaining findings.",
            absent_backend
        ),
        Some(Classification::Completed)
    );
    assert_eq!(
        classify_gate_report(
            &view,
            "verdict",
            SessionStatus::Completed,
            "Round 3: Approved",
            absent_backend
        ),
        Some(Classification::Completed)
    );
    assert_eq!(
        classify_gate_report(
            &view,
            "verdict",
            SessionStatus::Ready,
            "Changes requested: add regression tests",
            absent_backend
        ),
        Some(Classification::Working)
    );
    assert_eq!(
        classify_gate_report(
            &view,
            "verdict",
            SessionStatus::Ready,
            "Cannot assess this diff: missing access to the base ref.",
            absent_backend
        ),
        Some(Classification::Blocked)
    );
    // A live backend keeps precedence: its verdict stands even when the
    // report text would read differently.
    assert_eq!(
        classify_gate_report(
            &view,
            "verdict",
            SessionStatus::Ready,
            "Approved. No remaining findings.",
            |prompt| {
                assert!(prompt.contains("explicitly approves"));
                Some(Classification::Working)
            }
        ),
        Some(Classification::Working)
    );
    // AwaitingInput is not a clean yield: the classifier stays the
    // tie-breaker for permission prompts and questions.
    assert_eq!(
        classify_gate_report(
            &view,
            "verdict",
            SessionStatus::AwaitingInput,
            "Allow tests?",
            |prompt| {
                assert!(prompt.contains("explicitly approves"));
                Some(Classification::Blocked)
            }
        ),
        Some(Classification::Blocked)
    );
}

#[test]
fn review_handoff_clean_completion_reconsiders_a_parked_report_after_restart() {
    use crate::circuit::evaluator::Classification;
    let mut view = report_gate_view();
    view.graph = CircuitGraph::agent_review(None, None, 3);
    view.context.set("source.agent_id", "900");
    view.context.set("source.review_preset", "1");
    view.steps[0].node_id = "await_source".into();
    let report = "Final report with remaining tests";
    advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "await_source".into(),
            classification: Some(Classification::Working),
            output: Some(report.into()),
        },
    );
    view.context = CircuitContext::from_json(&view.context.to_json().unwrap()).unwrap();
    assert!(!should_classify_report(
        &view,
        "await_source",
        SessionStatus::AwaitingInput,
        report,
        None
    ));
    assert!(should_classify_report(
        &view,
        "await_source",
        SessionStatus::Ready,
        report,
        None
    ));
    assert_eq!(
        classify_gate_report(
            &view,
            "await_source",
            SessionStatus::Ready,
            report,
            |_| panic!("clean turn does not need a classifier")
        ),
        Some(Classification::Completed)
    );
    view.state = RunState::Cancelled;
    assert!(!should_classify_report(
        &view,
        "await_source",
        SessionStatus::Ready,
        report,
        None
    ));
    view.state = RunState::Running;
    view.steps[0].status = StepStatus::Completed;
    assert!(!should_classify_report(
        &view,
        "await_source",
        SessionStatus::Ready,
        report,
        None
    ));
}

#[test]
fn review_handoff_repeats_feedback_until_explicit_approval_for_new_and_saved_presets() {
    use crate::circuit::evaluator::Classification;
    use crate::circuit::stepper::{Capacity, Effect};
    for legacy in [false, true] {
        let mut view = RunView {
            run_id: 85,
            graph: CircuitGraph::agent_review(None, None, 3),
            state: RunState::Pending,
            context: CircuitContext::new(),
            steps: vec![],
        };
        view.context.set("source.agent_id", "3759");
        view.context.set("source.review_preset", "1");
        view.context.set("source.provider", "commandcode");
        view.context.set("review.provider", "agy");
        if legacy {
            view.graph
                .nodes
                .iter_mut()
                .find(|n| n.id == "await_fixes")
                .unwrap()
                .kind = CircuitNodeKind::LlmTurnClassifier {
                target_node_id: Some("$source".into()),
            };
        }
        let capacity = Capacity {
            agent_free_slots: 1,
        };
        let deliver = |view: &mut RunView, node: &str| {
            let dispatched = advance(
                view,
                &CircuitEvent::AgentReady {
                    node_id: node.into(),
                },
            );
            let attempt = view.step(node).unwrap().attempt;
            let delivered = advance(
                view,
                &CircuitEvent::PromptDelivered {
                    node_id: node.into(),
                    attempt,
                },
            );
            (dispatched.effects, delivered.effects)
        };
        advance(&mut view, &CircuitEvent::Triggered);
        advance(&mut view, &CircuitEvent::Tick(capacity));
        let mut tracker = crate::services::commandcode_watcher::TurnTracker::default();
        let native_report = |text: &str| {
            serde_json::json!({"type":"message", "id":"entry", "message": {
            "role":"assistant", "content":[{"type":"text", "text":text}],
            "meta":{"source":"model", "messageId":text}}})
            .to_string()
        };
        assert_eq!(
            tracker.observe_transcript_line(&native_report("Implementation report")),
            Some(crate::services::commandcode_watcher::TerminalTransition::TurnCompleted)
        );
        let classification = classify_gate_report(
            &view,
            "await_source",
            SessionStatus::Ready,
            "Implementation report",
            |_| None,
        );
        advance_with_report_evidence(
            &mut view,
            &CircuitEvent::TurnClassified {
                binding: None,
                node_id: "await_source".into(),
                classification,
                output: Some("Implementation report".into()),
            },
        );
        advance(&mut view, &CircuitEvent::Tick(capacity));
        let (publish, _) = deliver(&mut view, "publish");
        assert!(publish.iter().any(|e| matches!(e,
            Effect::InjectPty { target_node_id: Some(target), prompt, .. }
                if target == "$source" && prompt.contains("gh pr create"))));
        let published = "Opened https://github.com/example/repo/pull/7.";
        let classification = classify_gate_report(
            &view,
            "await_publish",
            SessionStatus::Ready,
            published,
            |_| None,
        );
        advance_with_report_evidence(
            &mut view,
            &CircuitEvent::TurnClassified {
                binding: None,
                node_id: "await_publish".into(),
                classification,
                output: Some(published.into()),
            },
        );
        let mut scheduled = advance(&mut view, &CircuitEvent::Tick(capacity));
        let reviewer_id = 4001;
        for round in 1..=3 {
            let spawns = scheduled
                .effects
                .iter()
                .filter(
                    |e| matches!(e, Effect::SpawnAgentNode { node_id } if node_id == "reviewer"),
                )
                .count();
            if round == 1 {
                assert_eq!(spawns, 1);
                let (provider, config) = resolve_review_spawn_inputs(
                    &view,
                    "reviewer",
                    None,
                    ExplicitSpawnOverrides::default(),
                    None,
                );
                assert_eq!(provider.as_deref(), Some("agy"));
                assert_eq!(config.model, None);
                view.attach_agent_node("reviewer", reviewer_id);
            } else {
                assert_eq!(spawns, 0, "round {round} re-prompts the open reviewer");
                let (reprompt, _) = deliver(&mut view, "re_review");
                assert!(reprompt.iter().any(|e| matches!(e,
                    Effect::InjectPty { target_node_id: Some(target), .. } if target == "reviewer")));
                assert_eq!(view.resolve_target_agent("re_review"), Some(reviewer_id));
            }
            assert_eq!(view.step("reviewer").unwrap().attempt, 1);
            let report = format!(
                "Round {round}: {}",
                if round == 3 {
                    "Approved"
                } else {
                    "Changes requested: add regression tests"
                }
            );
            if round == 1 {
                crate::circuit::test_support::advance_with_completion_evidence(
                    &mut view,
                    &CircuitEvent::AgentFinished {
                        agent_node_id: reviewer_id,
                        success: true,
                        output: Some(report.clone()),
                    },
                );
            }
            advance(&mut view, &CircuitEvent::Tick(capacity));
            assert_eq!(view.step("verdict").unwrap().attempt, round);
            let classification =
                classify_gate_report(&view, "verdict", SessionStatus::Ready, &report, |_| {
                    Some(if round == 3 {
                        Classification::Completed
                    } else {
                        Classification::Working
                    })
                });
            let mut verdict = advance_with_report_evidence(
                &mut view,
                &CircuitEvent::TurnClassified {
                    binding: None,
                    node_id: "verdict".into(),
                    classification,
                    output: Some(report.clone()),
                },
            );
            if round == 3 {
                verdict
                    .effects
                    .extend(advance(&mut view, &CircuitEvent::Tick(capacity)).effects);
                assert_eq!(
                    view.state,
                    RunState::Running,
                    "the merge request is still to be delivered"
                );
                let (merge, delivered) = deliver(&mut view, "merge");
                verdict.effects.extend(merge);
                verdict.effects.extend(delivered);
                assert_eq!(view.state, RunState::Completed);
                assert!(verdict
                    .effects
                    .iter()
                    .all(|e| !matches!(e, Effect::SpawnAgentNode { .. })));
                assert!(verdict.effects.iter().any(|e| matches!(e,
                    Effect::InjectPty { node_id, target_node_id: Some(target), .. }
                        if node_id == "merge" && target == "$source")));
                break;
            }
            assert_eq!(view.state, RunState::Running);
            let (feedback, delivered) = deliver(&mut view, "feedback");
            assert_eq!(view.resolve_target_agent("feedback"), Some(3759));
            assert!(feedback.iter().any(
                |e| matches!(e, Effect::InjectPty { prompt, .. } if prompt.contains(&report))
            ));
            assert!(feedback
                .iter()
                .chain(&delivered)
                .all(|e| !matches!(e, Effect::CloseAgentNode { .. })));
            // Restart of the durable context between delivery and the next report.
            view.context = CircuitContext::from_json(&view.context.to_json().unwrap()).unwrap();
            advance(&mut view, &CircuitEvent::Tick(capacity));
            let fixes = "Fixes made; some optional tests remain.";
            assert_eq!(
                tracker.observe_transcript_line(r#"{"type":"user_turn"}"#),
                None
            );
            assert_eq!(
                tracker.observe_transcript_line(&native_report(fixes)),
                Some(crate::services::commandcode_watcher::TerminalTransition::TurnCompleted)
            );
            let classification =
                classify_gate_report(&view, "await_fixes", SessionStatus::Ready, fixes, |_| None);
            scheduled = advance_with_report_evidence(
                &mut view,
                &CircuitEvent::TurnClassified {
                    binding: None,
                    node_id: "await_fixes".into(),
                    classification,
                    output: Some(fixes.into()),
                },
            );
            scheduled
                .effects
                .extend(advance(&mut view, &CircuitEvent::Tick(capacity)).effects);
            assert!(view.steps.iter().all(|s| s.agent_node_id != Some(3759)));
        }
    }
}
