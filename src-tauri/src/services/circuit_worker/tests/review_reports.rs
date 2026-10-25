// Circuit worker review reports tests. Included in the existing tests scope.
// The review-report gate fixture and its permission/background/verdict cases
// stay together to avoid duplicating review setup and to retain libtest names.
fn report_gate_view() -> RunView {
    let context = CircuitContext::new();
    RunView {
        run_id: 27,
        graph: CircuitGraph::issue_driven_autopilot_review("ready-for-agent"),
        state: RunState::Running,
        context,
        steps: vec![
            StepView {
                node_id: "finish_classifier".into(),
                status: StepStatus::Running,
                agent_node_id: None,
                attempt: 1,
                outcome: None,
                error: None,
            },
            StepView {
                node_id: "implementer".into(),
                status: StepStatus::Completed,
                agent_node_id: Some(900),
                attempt: 1,
                outcome: None,
                error: None,
            },
            StepView {
                node_id: "implementation_classifier".into(),
                status: StepStatus::Completed,
                agent_node_id: None,
                attempt: 1,
                outcome: Some(GraphStepOutcome::Completed),
                error: None,
            },
            StepView {
                node_id: "finish".into(),
                status: StepStatus::Completed,
                agent_node_id: None,
                attempt: 1,
                outcome: Some(GraphStepOutcome::Completed),
                error: None,
            },
            StepView {
                node_id: "trigger".into(),
                status: StepStatus::Completed,
                agent_node_id: None,
                attempt: 1,
                outcome: Some(GraphStepOutcome::Completed),
                error: None,
            },
            StepView {
                node_id: "collaborator_gate".into(),
                status: StepStatus::Completed,
                agent_node_id: None,
                attempt: 1,
                outcome: Some(GraphStepOutcome::Completed),
                error: None,
            },
            StepView {
                node_id: "finish_round".into(),
                status: StepStatus::Completed,
                agent_node_id: None,
                attempt: 1,
                outcome: Some(GraphStepOutcome::Completed),
                error: None,
            },
        ],
    }
}

#[test]
fn watchdog_native_completion_recovers_without_quiet_or_classifier_but_fences_old_turns() {
    let completion = crate::services::transcript_reader::NativeTurnCompletion {
        turn_id: "turn-1".into(),
        completed_at_ms: 1789324053252,
        final_report: None,
    };
    let published = std::cell::Cell::new(0);
    let stamp = "1789321859469:2026-09-13T18:25:20.569845100+00:00";
    assert!(recover_native_turn(
        Some(completion.clone()),
        Some(stamp),
        |_| true,
        || published.set(published.get() + 1)
    ));
    assert_eq!(published.get(), 1);
    for invalid in [
        None,
        Some("invalid"),
        Some("1789324054000:2026-09-13T18:25:20Z"),
        Some("1789321859469:2026-09-13T18:27:34Z"),
    ] {
        assert!(!recover_native_turn(
            Some(completion.clone()),
            invalid,
            |_| panic!("old or uncorrelated completion"),
            || panic!("must not publish")
        ));
    }
    assert!(!recover_native_turn(
        Some(completion),
        Some(stamp),
        |_| false,
        || panic!("input, lifecycle, or transcript changed during observation")
    ));
    assert!(!recover_native_turn(
        None,
        Some(stamp),
        |_| panic!("no evidence"),
        || panic!("no completion")
    ));
}

#[test]
fn circuit_status_projection_retains_yield_without_completing_assigned_work() {
    let _db = install_temp_db();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_str().unwrap();
    let mesh = db::create_mesh("evidence-projection", path).unwrap();
    let mut node = db::create_agent_node(db::NewAgentNode {
        mesh_id: mesh.id,
        name: "Assigned work",
        path,
        branch: "work",
        env: crate::models::EnvType::Windows,
        provider: "terminal",
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
    .unwrap();
    let mut graph = CircuitGraph::walking_skeleton("task");
    if let CircuitNodeKind::SpawnAgentNode { prompt, .. } = &mut graph
        .nodes
        .iter_mut()
        .find(|n| n.id == "spawn")
        .unwrap()
        .kind
    {
        *prompt = "Assigned work".into();
    }
    let mut view = RunView {
        run_id: 85,
        graph,
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![StepView {
            node_id: "spawn".into(),
            status: StepStatus::Running,
            agent_node_id: Some(node.id),
            attempt: 1,
            outcome: None,
            error: None,
        }],
    };
    for status in [
        SessionStatus::Ready,
        SessionStatus::AwaitingInput,
        SessionStatus::Completed,
    ] {
        node.status = status;
        db::update_agent_node_status(node.id, status).unwrap();
        let mut events = Vec::new();
        observe_agent_projection(&view, &view.steps[0], &node, &mut events);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], CircuitEvent::Observed { .. }));
        let transition = advance(&mut view, &events.remove(0));
        assert!(transition.effects.is_empty());
        assert_eq!(view.steps[0].status, StepStatus::Running);
        assert_eq!(
            transition.observations[0].disposition,
            crate::circuit::observation::ObservationDisposition::ReducedConfidence
        );
        observe_agent_projection(&view, &view.steps[0], &node, &mut events);
        assert!(
            events.is_empty(),
            "unchanged pull snapshots must not grow history each tick"
        );
    }
    // The caller's row may predate the lifecycle snapshot. Read status,
    // session and timestamp together rather than combining generations.
    db::write_conn().execute("UPDATE agent_nodes SET status='awaiting_input',session_started_at=123,status_changed_at='1970-01-01T00:00:01Z' WHERE id=?1",[node.id]).unwrap();
    let mut events = Vec::new();
    observe_agent_projection(&view, &view.steps[0], &node, &mut events);
    let CircuitEvent::Observed { observation, .. } = &events[0] else {
        panic!("status projection")
    };
    assert_eq!(observation.observed_at_ms, 1000);
    assert_eq!(
        observation.identity.session_incarnation.as_deref(),
        Some("123")
    );
    assert_eq!(
        observation.fact,
        crate::circuit::observation::ObservedWorkFact::NeedsInput
    );
}

#[test]
fn review_handoff_without_transcript_or_native_evidence_remains_unverified() {
    let _db = install_temp_db();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_str().unwrap();
    let mesh = db::create_mesh("review-no-transcript", path).unwrap();
    let node = db::create_agent_node(db::NewAgentNode {
        mesh_id: mesh.id,
        name: "Finished source",
        path,
        branch: "work",
        env: crate::models::EnvType::Windows,
        provider: "terminal",
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
    .unwrap();
    let mut view = RunView {
        run_id: 85,
        graph: CircuitGraph::agent_review(None, None, 3),
        state: RunState::Pending,
        context: CircuitContext::new(),
        steps: vec![],
    };
    view.context.set("source.agent_id", node.id.to_string());
    view.context.set("source.review_preset", "1");
    advance(&mut view, &CircuitEvent::Triggered);
    let capacity = crate::circuit::stepper::Capacity {
        agent_free_slots: 1,
    };
    advance(&mut view, &CircuitEvent::Tick(capacity));
    crate::circuit::evaluator::register_circuit(node.id);
    for status in ["ready", "completed"] {
        db::write_conn()
            .execute(
                "UPDATE agent_nodes SET status=?2 WHERE id=?1",
                rusqlite::params![node.id, status],
            )
            .unwrap();
        assert!(
            classify_step_turn(&active_run(85), &view, "await_source").is_none_or(|result| result
                .observation_blocker
                .is_some()
                && result.classification.is_none()
                && result.binding.is_none()),
            "Ready or Completed without identity-bound evidence cannot establish a handoff"
        );
    }
    let mut after_feedback = view.clone();
    after_feedback.context.set(
        &format!("agent.{}.previous_report_revision", node.id),
        "previous-turn",
    );
    assert!(
        classify_step_turn(&active_run(85), &after_feedback, "await_source").is_none_or(|result| {
            result.observation_blocker.is_some()
                && result.classification.is_none()
                && result.binding.is_none()
        }),
        "an injected prompt still requires a fresh report"
    );
    let mut permission = view.clone();
    db::write_conn()
        .execute(
            "UPDATE agent_nodes SET status='awaiting_input' WHERE id=?1",
            [node.id],
        )
        .unwrap();
    assert!(
        classify_step_turn(&active_run(85), &permission, "await_source").is_none_or(
            |result| result.observation_blocker.is_some()
                && result.classification.is_none()
                && result.binding.is_none()
        ),
        "missing evidence must not authorize a permission request"
    );
    permission.state = RunState::Cancelled;
    db::write_conn()
        .execute(
            "UPDATE agent_nodes SET status='completed' WHERE id=?1",
            [node.id],
        )
        .unwrap();
    assert!(classify_step_turn(&active_run(85), &permission, "await_source").is_none());
    let mut renamed = view.clone();
    renamed
        .graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "await_source")
        .unwrap()
        .id = "handoff".into();
    for edge in &mut renamed.graph.edges {
        if edge.from == "await_source" {
            edge.from = "handoff".into();
        }
        if edge.to == "await_source" {
            edge.to = "handoff".into();
        }
    }
    renamed
        .steps
        .iter_mut()
        .find(|step| step.node_id == "await_source")
        .unwrap()
        .node_id = "handoff".into();
    assert!(
        classify_step_turn(&active_run(85), &renamed, "handoff").is_none_or(|result| result
            .observation_blocker
            .is_some()
            && result.classification.is_none()
            && result.binding.is_none()),
        "renaming a step cannot bypass the evidence requirement"
    );
    let transition = advance(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "await_source".into(),
            classification: Some(crate::circuit::evaluator::Classification::Completed),
            output: Some("Looks done".into()),
        },
    );
    assert_eq!(
        view.step("await_source").unwrap().status,
        StepStatus::Unverified
    );
    assert!(transition.effects.is_empty());
    let scheduled = advance(&mut view, &CircuitEvent::Tick(capacity));
    assert!(!scheduled.effects.iter().any(|e| matches!(e,
        crate::circuit::stepper::Effect::SpawnAgentNode { node_id } if node_id == "reviewer")));
    crate::circuit::evaluator::unregister(node.id);
}

#[test]
fn review_handoff_clean_turn_does_not_require_task_completion_or_classifier() {
    use crate::circuit::evaluator::Classification;
    use crate::circuit::stepper::{Capacity, Effect};
    let mut view = RunView {
        run_id: 85,
        graph: CircuitGraph::agent_review(None, None, 3),
        state: RunState::Pending,
        context: CircuitContext::new(),
        steps: vec![],
    };
    view.context.set("source.agent_id", "3759");
    view.context.set("source.review_preset", "1");
    advance(&mut view, &CircuitEvent::Triggered);
    let capacity = Capacity {
        agent_free_slots: 1,
    };
    advance(&mut view, &CircuitEvent::Tick(capacity));
    let report = "Final Summary: 11 commits landed. Remaining work: per-adapter snapshot tests.";
    let classification =
        classify_gate_report(&view, "await_source", SessionStatus::Ready, report, |_| {
            None
        });
    assert_eq!(classification, Some(Classification::Completed));
    advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "await_source".into(),
            classification,
            output: Some(report.into()),
        },
    );
    // The source then publishes; a clean publication turn is a hand-off
    // in its own right and needs no task classifier either.
    let publish = advance(&mut view, &CircuitEvent::Tick(capacity));
    assert!(!publish
        .effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnAgentNode { .. })));
    advance(
        &mut view,
        &CircuitEvent::AgentReady {
            node_id: "publish".into(),
        },
    );
    let attempt = view.step("publish").unwrap().attempt;
    advance(
        &mut view,
        &CircuitEvent::PromptDelivered {
            node_id: "publish".into(),
            attempt,
        },
    );
    let published = "Opened https://github.com/example/repo/pull/7.";
    let classification = classify_gate_report(
        &view,
        "await_publish",
        SessionStatus::Ready,
        published,
        |_| None,
    );
    assert_eq!(classification, Some(Classification::Completed));
    advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "await_publish".into(),
            classification,
            output: Some(published.into()),
        },
    );
    let transition = advance(&mut view, &CircuitEvent::Tick(capacity));
    assert!(transition
        .effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnAgentNode { node_id } if node_id == "reviewer")));
    assert_eq!(view.context.get("source.output"), Some(published));
    // Persisted presets used LlmTurnClassifier for fixes. They must gain
    // the same behavior without rewriting an active graph's ledger.
    view.graph
        .nodes
        .iter_mut()
        .find(|n| n.id == "await_fixes")
        .unwrap()
        .kind = CircuitNodeKind::LlmTurnClassifier {
        target_node_id: Some("$source".into()),
    };
    assert_eq!(
        classify_gate_report(&view, "await_fixes", SessionStatus::Ready, report, |_| None),
        Some(Classification::Completed)
    );
    view.context.set("source.review_preset", "0");
    view.context.set("recovery.from_run_id", "84");
    assert_eq!(
        classify_gate_report(&view, "await_source", SessionStatus::Ready, report, |_| {
            None
        }),
        Some(Classification::Completed)
    );
    assert_eq!(
        classify_gate_report(
            &view,
            "await_fixes",
            SessionStatus::Completed,
            report,
            |_| None
        ),
        Some(Classification::Completed)
    );
}

#[test]
fn review_handoff_keeps_permission_background_and_verdict_checks() {
    use crate::circuit::evaluator::Classification;
    let mut view = report_gate_view();
    view.graph = CircuitGraph::agent_review(None, None, 3);
    view.context.set("source.review_preset", "1");
    for status in [
        SessionStatus::Running,
        SessionStatus::Spawning,
        SessionStatus::Error,
    ] {
        assert_eq!(
            classify_gate_report(&view, "await_source", status, "Earlier report", |_| panic!(
                "active or lost agent cannot be classified"
            )),
            None
        );
    }
    assert_eq!(
        classify_gate_report(&view, "await_source", SessionStatus::Ready, "", |_| panic!(
            "empty report"
        )),
        None
    );
    assert_eq!(
        classify_gate_report(
            &view,
            "await_source",
            SessionStatus::AwaitingInput,
            "Allow tests?",
            |prompt| {
                assert!(prompt.contains("ready for an independent code review"));
                Some(Classification::Blocked)
            }
        ),
        Some(Classification::Blocked)
    );
    assert_eq!(
        classify_gate_report(
            &view,
            "await_fixes",
            SessionStatus::AwaitingInput,
            "Tests running",
            |_| Some(Classification::Working)
        ),
        Some(Classification::Working)
    );
    assert_eq!(
        classify_gate_report(
            &view,
            "await_source",
            SessionStatus::AwaitingInput,
            "Report",
            |_| None
        ),
        None
    );
    // Without a backend the clean reviewer turn falls back to reading
    // the report deterministically (issue #1815).
    assert_eq!(
        classify_gate_report(
            &view,
            "verdict",
            SessionStatus::Ready,
            "Changes requested",
            |_| None
        ),
        Some(Classification::Working)
    );
    // AwaitingInput keeps the classifier as the tie-breaker.
    assert_eq!(
        classify_gate_report(
            &view,
            "verdict",
            SessionStatus::AwaitingInput,
            "Changes requested",
            |prompt| {
                assert!(prompt.contains("explicitly approves"));
                Some(Classification::Working)
            }
        ),
        Some(Classification::Working)
    );
    view.context.set("source.review_preset", "0");
    assert_eq!(
        classify_gate_report(
            &view,
            "await_source",
            SessionStatus::Ready,
            "Remaining work",
            |prompt| {
                assert!(prompt.contains("the assigned work is finished"));
                Some(Classification::Working)
            }
        ),
        Some(Classification::Working)
    );
    // A custom task-completion gate retains its original contract.
    view.graph = CircuitGraph::issue_driven_autopilot_review("run");
    assert_eq!(
        classify_gate_report(
            &view,
            "finish_classifier",
            SessionStatus::Ready,
            "Remaining work",
            |prompt| {
                assert!(prompt.contains("the assigned work is finished"));
                Some(Classification::Working)
            }
        ),
        Some(Classification::Working)
    );
}

#[test]
fn verdict_gate_waits_for_a_reviewer_that_yielded_mid_turn() {
    use crate::circuit::evaluator::Classification;
    let mut view = report_gate_view();
    view.graph = CircuitGraph::agent_review(None, None, 3);
    view.context.set("source.review_preset", "1");
    // Run 163's reviewer yielded `awaiting_input` mid-work; its latest
    // assistant text was the progress line below, not a review. The gate
    // must ask the reviewer readiness question and wait instead of
    // recording that line as the reviewer's verdict.
    let progress = "PowerShell NativeCommandError. Let me retry with the standard `.cmd` shim \
                    correctly and pick off the impacted suites plus a broader window.";
    assert_eq!(
        reviewer_readiness(
            &view,
            "verdict",
            SessionStatus::AwaitingInput,
            progress,
            |prompt| {
                assert!(
                    prompt.contains("whether this independent reviewer has finished its turn"),
                    "an unfinished reviewer must be judged by the reviewer readiness question, \
                 not the verdict question: {prompt}"
                );
                assert!(
                    !prompt.contains("reports a provider/API failure"),
                    "the implementation gate's readiness question calls a reported failure \
                 `BLOCKED`, which would send run 163's `…Let me retry…` line to the verdict \
                 classifier anyway: {prompt}"
                );
                Some(Classification::Working)
            }
        ),
        ReviewerReadiness::Working,
        "a mid-turn progress line must never be consumed as the reviewer's verdict (run 163)"
    );
    // Once the reviewer reports for real, the verdict question is asked as
    // before — the wait is not a permanent state.
    assert_eq!(
        reviewer_readiness(
            &view,
            "verdict",
            SessionStatus::AwaitingInput,
            "Findings: none. Verdict: approved.",
            |_| { Some(Classification::Completed) }
        ),
        ReviewerReadiness::Reportable
    );
    assert_eq!(
        classify_gate_report(
            &view,
            "verdict",
            SessionStatus::AwaitingInput,
            progress,
            |prompt| {
                assert!(prompt.contains("explicitly approves"), "{prompt}");
                Some(Classification::Completed)
            }
        ),
        Some(Classification::Completed)
    );
    // A clean lifecycle turn is authoritative and is not re-judged; a
    // reviewer that will not continue without a person is a real gate
    // outcome; an outage is unknown rather than unfinished; and only the
    // verdict gate consumes a review result.
    assert_eq!(
        reviewer_readiness(&view, "verdict", SessionStatus::Ready, progress, |_| {
            panic!("a clean lifecycle turn needs no readiness judgement")
        }),
        ReviewerReadiness::Reportable
    );
    assert_eq!(
        reviewer_readiness(
            &view,
            "verdict",
            SessionStatus::AwaitingInput,
            "May I install a package to run the suite?",
            |_| { Some(Classification::Blocked) }
        ),
        ReviewerReadiness::Reportable
    );
    assert_eq!(
        reviewer_readiness(
            &view,
            "verdict",
            SessionStatus::AwaitingInput,
            progress,
            |_| None
        ),
        ReviewerReadiness::Unavailable
    );
    assert_eq!(
        reviewer_readiness(
            &view,
            "await_source",
            SessionStatus::AwaitingInput,
            "Allow tests?",
            |_| { panic!("only the verdict gate consumes a review result") }
        ),
        ReviewerReadiness::Reportable
    );
}
