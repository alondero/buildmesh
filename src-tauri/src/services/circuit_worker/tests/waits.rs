// Circuit worker waits tests. Included in the existing tests scope.
/// #1219 (PR #1666 review): the author's per-step `timeout_seconds` must
/// reach the watchdog seam, not just the launch carrier. `observe_waits`
/// builds the `WaitObserved` event the stepper enforces (`since_ms` +
/// `timeout_ms`), so pin the event's `timeout_ms` here: an explicit
/// 1800s budget must surface as 1_800_000ms, while `None` and `Some(0)`
/// keep the fixed default. The steps carry no attached agent, so this
/// exercises the DB-free prerequisites branch — the override applies
/// after the branch computation, identically for attached agents.
#[test]
fn observe_waits_uses_spawn_step_timeout_seconds_for_wait_observed() {
    fn wait_timeout_ms(timeout_seconds: Option<u32>) -> i64 {
        let view = RunView {
            run_id: 1,
            graph: CircuitGraph {
                version: CIRCUIT_GRAPH_VERSION,
                blueprint: None,
                nodes: vec![CircuitNode {
                    id: "worker".into(),
                    kind: CircuitNodeKind::SpawnAgentNode {
                        prompt: "p".into(),
                        name: None,
                        provider: None,
                        model: None,
                        effort: None,
                        extra_args: None,
                        timeout_seconds,
                    },
                }],
                edges: vec![],
            },
            state: RunState::Running,
            context: CircuitContext::new(),
            steps: vec![StepView {
                node_id: "worker".into(),
                status: StepStatus::Running,
                outcome: None,
                error: None,
                agent_node_id: None,
                attempt: 1,
            }],
        };
        let mut events = Vec::new();
        observe_waits(&view, &mut events);
        assert_eq!(
            events.len(),
            1,
            "one Running step must yield one WaitObserved"
        );
        match &events[0] {
            CircuitEvent::WaitObserved {
                node_id,
                timeout_ms,
                ..
            } => {
                assert_eq!(node_id, "worker");
                *timeout_ms
            }
            other => panic!("expected WaitObserved, got {other:?}"),
        }
    }

    assert_eq!(wait_timeout_ms(Some(1800)), 1_800_000);
    assert_eq!(
        wait_timeout_ms(None),
        YIELDED_WAIT_MS,
        "no budget keeps the fixed default for unattached steps"
    );
    assert_eq!(
        wait_timeout_ms(Some(0)),
        YIELDED_WAIT_MS,
        "Some(0) collapses to absent so it can't request instant expiry"
    );
}

/// A Running `SpawnAgentNode` step bound to `agent_node_id`, carrying an
/// optional authored `timeout_seconds` budget.
fn spawn_wait_view(agent_node_id: i64, timeout_seconds: Option<u32>) -> RunView {
    RunView {
        run_id: 1,
        graph: CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![CircuitNode {
                id: "worker".into(),
                kind: CircuitNodeKind::SpawnAgentNode {
                    prompt: "p".into(),
                    name: None,
                    provider: None,
                    model: None,
                    effort: None,
                    extra_args: None,
                    timeout_seconds,
                },
            }],
            edges: vec![],
        },
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![StepView {
            node_id: "worker".into(),
            status: StepStatus::Running,
            outcome: None,
            error: None,
            agent_node_id: Some(agent_node_id),
            attempt: 1,
        }],
    }
}

fn wait_event(events: &[CircuitEvent]) -> &CircuitEvent {
    assert_eq!(
        events.len(),
        1,
        "one Running step must yield one WaitObserved"
    );
    &events[0]
}

/// Run 335: a yielded implementer's report was classified WORKING 13 seconds
/// after the gate started, and the 30-second per-harness lifecycle budget then
/// parked the gate Unverified before the agent could resume. The budget is for
/// reconciling a yield that has no verdict yet; once the classifier has said
/// the agent still has work, waiting for its next report gets the documented
/// 15-minute yielded allowance.
#[test]
fn yielded_wait_after_a_working_verdict_gets_the_full_yielded_allowance() {
    const HARNESS_LIFECYCLE_BUDGET_MS: i64 = 30_000;
    fn yielded_timeout_ms(verdict: Option<(&str, &str)>) -> i64 {
        let mut view = spawn_wait_view(1, None);
        if let Some((classification, evaluated_attempt)) = verdict {
            view.context
                .set("node.worker.classification", classification);
            view.context
                .set("node.worker.evaluated_attempt", evaluated_attempt);
        }
        let mut events = Vec::new();
        observe_waits_with(&view, &mut events, 1_000, |_, _| {
            Some(WaitObservation {
                progress: Some("report:1".into()),
                observed: true,
                yielded: true,
                yielded_budget_ms: HARNESS_LIFECYCLE_BUDGET_MS,
                active_budget_ms: ACTIVE_WAIT_MS,
                stalled_task_id: None,
                fence: None,
            })
        });
        match wait_event(&events) {
            CircuitEvent::WaitObserved { timeout_ms, .. } => *timeout_ms,
            other => panic!("expected WaitObserved, got {other:?}"),
        }
    }

    assert_eq!(
        yielded_timeout_ms(None),
        HARNESS_LIFECYCLE_BUDGET_MS,
        "a yield with no verdict keeps the per-harness lifecycle budget"
    );
    assert_eq!(
        yielded_timeout_ms(Some(("working", "1"))),
        YIELDED_WAIT_MS,
        "a WORKING verdict means the next report is the agent's own, not lifecycle evidence"
    );
    assert_eq!(
        yielded_timeout_ms(Some(("continue", "1"))),
        YIELDED_WAIT_MS,
        "a delivered continuation waits for the agent's next report the same way"
    );
    assert_eq!(
        yielded_timeout_ms(Some(("working", "2"))),
        HARNESS_LIFECYCLE_BUDGET_MS,
        "a verdict recorded for an earlier attempt says nothing about this one"
    );
    assert_eq!(
        yielded_timeout_ms(Some(("completed", "1"))),
        HARNESS_LIFECYCLE_BUDGET_MS,
        "only a verdict that says work remains widens the window"
    );
}

/// Create a node and register it with the evaluator so `observe_waits`
/// reads it. Returns its id.
fn register_test_agent(mesh_id: i64, path: &str, name: &str) -> i64 {
    register_test_agent_with_provider(mesh_id, path, name, "claude")
}

fn register_test_agent_with_provider(mesh_id: i64, path: &str, name: &str, provider: &str) -> i64 {
    let agent = db::create_agent_node(db::NewAgentNode {
        mesh_id,
        name,
        path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider,
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: true,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    crate::circuit::evaluator::unregister(agent.id);
    crate::circuit::evaluator::register_circuit(agent.id);
    agent.id
}

/// #1791: a busy agent that has produced neither a session identity nor a
/// readable report is reported as unobserved, so the stepper can fail it
/// at the first-observation window instead of the active budget.
#[test]
fn observe_waits_flags_an_agent_without_session_identity_or_report() {
    let _db = install_temp_db();
    let mesh = db::create_mesh("wait-unobserved", "/tmp/wait-unobserved").unwrap();
    let agent_id = register_test_agent(mesh.id, &mesh.path, "worker");
    let view = spawn_wait_view(agent_id, None);

    let mut events = Vec::new();
    observe_waits(&view, &mut events);
    crate::circuit::evaluator::unregister(agent_id);

    match wait_event(&events) {
        CircuitEvent::WaitObserved {
            observed,
            explicit_budget,
            reason,
            timeout_ms,
            ..
        } => {
            assert!(
                !*observed,
                "no session identity and no report is the unobserved case"
            );
            assert!(!*explicit_budget);
            assert!(
                reason.contains("session identity"),
                "discovery reason: {reason}"
            );
            assert_eq!(*timeout_ms, ACTIVE_WAIT_MS);
        }
        other => panic!("expected WaitObserved, got {other:?}"),
    }
}

/// A session identity on its own is an observation: the agent is provably
/// running, so it must not be reported as unobserved.
#[test]
fn observe_waits_marks_a_session_identity_as_observed() {
    let _db = install_temp_db();
    let mesh = db::create_mesh("wait-observed", "/tmp/wait-observed").unwrap();
    let agent_id = register_test_agent(mesh.id, &mesh.path, "worker");
    db::write_conn()
        .execute(
            "UPDATE agent_nodes SET cli_session_id = 'test-session-1791' WHERE id = ?1",
            rusqlite::params![agent_id],
        )
        .unwrap();
    let view = spawn_wait_view(agent_id, None);

    let mut events = Vec::new();
    observe_waits(&view, &mut events);
    crate::circuit::evaluator::unregister(agent_id);

    match wait_event(&events) {
        CircuitEvent::WaitObserved {
            observed, reason, ..
        } => {
            assert!(*observed, "a captured session identity is an observation");
            assert!(
                reason.is_empty(),
                "observed and not yielded has no diagnostic: {reason}"
            );
        }
        other => panic!("expected WaitObserved, got {other:?}"),
    }
}

/// #1219: an authored per-step budget must reach the stepper flagged as an
/// override so it takes precedence over the unobserved fast fail.
#[test]
fn observe_waits_reports_an_explicit_step_budget() {
    let _db = install_temp_db();
    let mesh = db::create_mesh("wait-explicit-budget", "/tmp/wait-explicit-budget").unwrap();
    let agent_id = register_test_agent(mesh.id, &mesh.path, "worker");
    let view = spawn_wait_view(agent_id, Some(1800));

    let mut events = Vec::new();
    observe_waits(&view, &mut events);
    crate::circuit::evaluator::unregister(agent_id);

    match wait_event(&events) {
        CircuitEvent::WaitObserved {
            explicit_budget,
            timeout_ms,
            ..
        } => {
            assert!(
                *explicit_budget,
                "an authored budget must be flagged as an override"
            );
            assert_eq!(*timeout_ms, 1_800_000);
        }
        other => panic!("expected WaitObserved, got {other:?}"),
    }
}

/// Issue #1794: when muse's extended capture window gives up, the node has
/// neither a session identity nor a readable report — so `observe_waits`
/// reports it unobserved and the #1791 watchdog fails the wait at the
/// first-observation window instead of stalling silently for the full
/// active budget. The give-up is surfaced, not absorbed.
#[test]
fn observe_waits_marks_a_muse_agent_without_identity_as_unobserved() {
    let _db = install_temp_db();
    let mesh = db::create_mesh("wait-muse-unobserved", "/tmp/wait-muse-unobserved").unwrap();
    let agent_id = register_test_agent_with_provider(mesh.id, &mesh.path, "muse-worker", "muse");
    let view = spawn_wait_view(agent_id, None);

    let mut events = Vec::new();
    observe_waits(&view, &mut events);
    crate::circuit::evaluator::unregister(agent_id);

    match wait_event(&events) {
        CircuitEvent::WaitObserved {
            observed, reason, ..
        } => {
            assert!(
                !*observed,
                "a muse node with no captured identity must be unobserved so #1791 trips"
            );
            assert!(
                reason.contains("session identity"),
                "discovery reason: {reason}"
            );
        }
        other => panic!("expected WaitObserved, got {other:?}"),
    }
}
