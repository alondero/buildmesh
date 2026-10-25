// Circuit worker observer recovery tests. Included in the existing tests scope.
#[test]
fn observer_recovery_includes_silent_borrowed_sources_and_is_throttled() {
    let source = 9_860_001;
    let mut view = RunView {
        run_id: 86,
        graph: CircuitGraph::agent_review(None, None, 3),
        context: CircuitContext::new(),
        steps: vec![],
        state: RunState::Running,
    };
    view.context.set("source.agent_id", source.to_string());
    restore_run_evaluators(&view);
    let mut recovered = Vec::new();
    recover_run_observers_with(
        &view,
        |_| true,
        |id| {
            recovered.push(id);
            Ok(())
        },
    );
    assert_eq!(recovered, vec![source]);
    recover_run_observers_with(
        &view,
        |_| true,
        |_| panic!("recovery must respect the probe cooldown"),
    );
    assert!(!crate::circuit::evaluator::has_turn_start(source));
    crate::circuit::evaluator::unregister(source);

    restore_run_evaluators(&view);
    recover_run_observers_with(
        &view,
        |_| false,
        |_| panic!("dead processes must not acquire observers"),
    );
    view.state = RunState::Cancelled;
    recover_run_observers_with(
        &view,
        |_| true,
        |_| panic!("cancelled runs must not acquire observers"),
    );
    view.state = RunState::Paused;
    recover_run_observers_with(
        &view,
        |_| true,
        |_| panic!("paused runs must not acquire observers"),
    );
    crate::circuit::evaluator::unregister(source);
}

fn observer_node(provider: &str, session: Option<&str>) -> crate::models::AgentNode {
    crate::models::AgentNode {
        provider: provider.into(),
        cli_session_id: session.map(str::to_string),
        path: "X:\\src\\proj".into(),
        worktree_name: Some("gentle-fox".into()),
        use_worktree: true,
        ..Default::default()
    }
}

/// Issue #1794: Muse delivers its turn signal through the passive watcher,
/// so recovery must dispatch a Muse watcher — not only Command Code's.
#[test]
fn observer_restart_dispatches_muse_and_commandcode_only() {
    assert_eq!(
        observer_restart(&observer_node("muse", Some("sid"))),
        Some(PassiveWatcher::Muse)
    );
    assert_eq!(
        observer_restart(&observer_node("commandcode", Some("sid"))),
        Some(PassiveWatcher::CommandCode)
    );
    // A harness with an attention hook needs no passive watcher.
    assert_eq!(
        observer_restart(&observer_node("claude", Some("sid"))),
        None
    );
    // No durable identity means nothing to reattach yet.
    assert_eq!(observer_restart(&observer_node("muse", None)), None);
    assert_eq!(observer_restart(&observer_node("muse", Some(""))), None);
}

/// Issue #1794: a Muse watcher dropped mid-run is reattached by recovery.
/// The dispatch is exercised through injected starters so the Muse branch
/// is pinned without an `AppHandle` or a live backend.
#[test]
fn recovery_restarts_a_dropped_muse_watcher() {
    let mut node = observer_node("muse", Some("12345678-1234-4234-8234-123456789abc"));
    node.id = 9_860_777;
    // The watcher is not observing anything at recovery time.
    crate::services::muse_watcher::stop(node.id);

    let mut muse_starts = Vec::new();
    restart_passive_observer_with(
        &node,
        |_, _| panic!("a muse node must not start the Command Code watcher"),
        |session_id, _| {
            muse_starts.push(session_id.to_string());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        muse_starts,
        vec!["12345678-1234-4234-8234-123456789abc".to_string()]
    );
}

/// Issue #1794: muse-bound agents must be walked by the recovery pass even
/// when they are only a step's borrowed agent, matching Command Code.
#[test]
fn observer_recovery_covers_a_running_muse_agent() {
    let muse_agent = 9_860_778;
    let view = RunView {
        run_id: 87,
        graph: CircuitGraph::agent_review(None, None, 3),
        context: CircuitContext::new(),
        steps: vec![StepView {
            node_id: "source".into(),
            status: StepStatus::Running,
            outcome: None,
            error: None,
            agent_node_id: Some(muse_agent),
            attempt: 1,
        }],
        state: RunState::Running,
    };
    restore_run_evaluators(&view);
    let mut recovered = Vec::new();
    recover_run_observers_with(
        &view,
        |_| true,
        |id| {
            recovered.push(id);
            Ok(())
        },
    );
    assert!(
        recovered.contains(&muse_agent),
        "a muse agent must acquire an observer during recovery: {recovered:?}"
    );
    crate::circuit::evaluator::unregister(muse_agent);
}

#[test]
fn cancellation_marker_invalidates_batches_until_durable_ack() {
    let run_id = 9_876_543_210_i64;
    let permit = begin_circuit_effect_batch(run_id);
    assert!(!permit.is_cancelled());

    mark_circuit_run_cancelled(run_id);
    assert!(permit.is_cancelled());

    // A batch admitted while the command is waiting must inherit the
    // already-cancelled token rather than starting fresh work.
    let blocked = begin_circuit_effect_batch(run_id);
    assert!(blocked.is_cancelled());
    drop(blocked);

    // The marker is retained while the original batch is in flight and
    // only becomes removable after the durable cancellation is acknowledged.
    finish_circuit_run_cancellation(run_id);
    drop(permit);

    let fresh = begin_circuit_effect_batch(run_id);
    assert!(!fresh.is_cancelled());
    drop(fresh);
}

#[test]
fn circuit_old_close_cannot_retire_next_review_round() {
    let view = RunView {
        run_id: 1,
        graph: CircuitGraph::agent_review(None, None, 3),
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![
            StepView {
                node_id: "reviewer".into(),
                agent_node_id: Some(101),
                attempt: 2,
                status: StepStatus::Running,
                outcome: None,
                error: None,
            },
            StepView {
                node_id: "close_approved".into(),
                agent_node_id: None,
                attempt: 1,
                status: StepStatus::Completed,
                outcome: Some(StepOutcome::Completed),
                error: None,
            },
        ],
    };
    let mut events = vec![];
    observe_close_agent_retries(&view, &mut events);
    assert!(events.is_empty());
}

#[test]
fn circuit_restart_restores_completed_spawn_ownership_at_every_downstream_gate() {
    use crate::circuit::evaluator;
    let id = 910_018;
    for gate in [
        "implementation_classifier",
        "finish_classifier",
        "review_classifier",
        "feedback_classifier",
    ] {
        let spawn = if gate == "review_classifier" {
            "reviewer"
        } else {
            "implementer"
        };
        let view = RunView {
            run_id: 17,
            graph: CircuitGraph::issue_driven_autopilot_review("ready-for-agent"),
            state: RunState::Running,
            context: CircuitContext::new(),
            steps: vec![
                StepView {
                    node_id: spawn.into(),
                    agent_node_id: Some(id),
                    attempt: 1,
                    status: StepStatus::Completed,
                    outcome: Some(StepOutcome::Completed),
                    error: None,
                },
                StepView {
                    node_id: gate.into(),
                    agent_node_id: None,
                    attempt: 1,
                    status: StepStatus::Running,
                    outcome: None,
                    error: None,
                },
            ],
        };
        evaluator::unregister(id); // restart discards all in-memory ownership and output
        restore_run_evaluators(&view);
        evaluator::on_output(id, "Task complete after resume");
        assert!(
            evaluator::is_circuit_piloted(id),
            "lost ownership at {gate}"
        );
        assert_eq!(
            evaluator::cleaned_turn_tail(id),
            "Task complete after resume"
        );
        restore_run_evaluators(&view);
        assert_eq!(
            evaluator::cleaned_turn_tail(id),
            "Task complete after resume",
            "ticks must preserve output"
        );
    }
    evaluator::unregister(id);
}

/// The reviewer reaches its verdict gate through the `review_round` join that
/// also collects later re-review prompts; its report is still a hand-off.
#[test]
fn reviewer_report_hands_off_through_the_review_round_join() {
    for graph in [
        CircuitGraph::agent_review(None, None, 3),
        CircuitGraph::issue_driven_autopilot_review("ready-for-agent"),
    ] {
        let mut view = RunView {
            run_id: 1,
            graph,
            state: RunState::Running,
            context: CircuitContext::new(),
            steps: vec![],
        };
        assert!(spawn_hands_off_report(&view, "reviewer"));
        assert!(
            !spawn_hands_off_report(&view, "review_round"),
            "only a spawn hands off its report"
        );
        // A join that also feeds a non-judging step is not a hand-off path.
        let notify = view
            .graph
            .nodes
            .iter()
            .find(|node| matches!(node.kind, CircuitNodeKind::Notify { .. }))
            .unwrap()
            .id
            .clone();
        view.graph.edges.push(crate::circuit::model::CircuitEdge {
            from: "review_round".into(),
            to: notify,
            condition: Default::default(),
        });
        assert!(!spawn_hands_off_report(&view, "reviewer"));
    }
}

/// An approved review hands its implementation agent back open; the circuit
/// must stop piloting it (and the reviewer and borrowed source) once terminal.
#[test]
fn terminal_run_stops_piloting_every_agent_it_hands_back() {
    use crate::circuit::evaluator;
    let (implementer, reviewer, source) = (910_041, 910_042, 910_043);
    let attached = |node_id: &str, agent: i64| StepView {
        node_id: node_id.into(),
        agent_node_id: Some(agent),
        attempt: 1,
        status: StepStatus::Completed,
        outcome: Some(StepOutcome::Completed),
        error: None,
    };
    let mut view = RunView {
        run_id: 41,
        graph: CircuitGraph::issue_driven_autopilot_review("ready-for-agent"),
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![
            attached("implementer", implementer),
            attached("reviewer", reviewer),
        ],
    };
    view.context.set("source.agent_id", source.to_string());
    restore_run_evaluators(&view);
    assert!([implementer, reviewer, source]
        .iter()
        .all(|id| evaluator::is_circuit_piloted(*id)));

    view.state = RunState::Completed;
    release_run_evaluators(&view);
    for id in [implementer, reviewer, source] {
        assert!(
            !evaluator::is_circuit_piloted(id),
            "agent {id} is still piloted after the run ended"
        );
    }
}
