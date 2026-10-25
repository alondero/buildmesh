// Circuit worker watchdog tests. Included in the existing tests scope.
// -- lost-turn watchdog eligibility -------------------------------------------

#[test]
fn watchdog_run_104_background_report_does_not_publish_a_turn() {
    use crate::circuit::evaluator::Classification;
    use crate::circuit::stepper::Capacity;
    let report = "The tests are progressing (1-9 passed, including `controller_accessories`, `haptics_race_e2e`, `eeprom_exit_flush`, and `controller_pak_rom_filesystem`). Waiting for the final e2e tests to finish.";
    assert!(should_check_quiet_turn(
        true,
        Some(60_012),
        SessionStatus::Running
    ));
    for classification in [
        Some(Classification::Working),
        Some(Classification::Continue),
        None,
    ] {
        let mut view = RunView {
            run_id: 104,
            graph: CircuitGraph::agent_review(None, None, 3),
            state: RunState::Pending,
            context: CircuitContext::new(),
            steps: vec![],
        };
        view.context.set("source.agent_id", "3914");
        let capacity = Capacity {
            agent_free_slots: 1,
        };
        advance(&mut view, &CircuitEvent::Triggered);
        advance(&mut view, &CircuitEvent::Tick(capacity));
        advance_with_report_evidence(
            &mut view,
            &CircuitEvent::TurnClassified {
                binding: None,
                node_id: "await_source".into(),
                classification: Some(Classification::Completed),
                output: Some("Implementation done".into()),
            },
        );
        advance(&mut view, &CircuitEvent::Tick(capacity));
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
        advance_with_report_evidence(
            &mut view,
            &CircuitEvent::TurnClassified {
                binding: None,
                node_id: "await_publish".into(),
                classification: Some(Classification::Completed),
                output: Some("PR opened".into()),
            },
        );
        advance(&mut view, &CircuitEvent::Tick(capacity));
        assert_eq!(view.step("reviewer").unwrap().status, StepStatus::Running);
        view.attach_agent_node("reviewer", 3923);
        recover_quiet_turn(
            report,
            |_| classification,
            || true,
            || {
                crate::circuit::test_support::advance_with_completion_evidence(
                    &mut view,
                    &CircuitEvent::AgentFinished {
                        agent_node_id: 3923,
                        success: true,
                        output: Some(report.into()),
                    },
                );
            },
        );
        assert_eq!(
            view.step("reviewer").unwrap().status,
            StepStatus::Running,
            "background/unknown evidence must not finish the reviewer step"
        );
        assert!(advance(&mut view, &CircuitEvent::Tick(capacity))
            .effects
            .is_empty());
        assert_eq!(view.state, RunState::Running);
        assert!(view.step("verdict").is_none());
        // The eventual final report still releases the same reviewer.
        recover_quiet_turn(
            "Review complete. Approved.",
            |_| Some(Classification::Completed),
            || true,
            || {
                crate::circuit::test_support::advance_with_completion_evidence(
                    &mut view,
                    &CircuitEvent::AgentFinished {
                        agent_node_id: 3923,
                        success: true,
                        output: Some("Review complete. Approved.".into()),
                    },
                );
            },
        );
        advance(&mut view, &CircuitEvent::Tick(capacity));
        assert_eq!(view.step("verdict").unwrap().status, StepStatus::Running);
    }
}

#[test]
fn watchdog_recovers_final_reports_and_real_input_requests() {
    use crate::circuit::evaluator::Classification;
    for (report, classification) in [
        (
            "Review complete. Changes requested: fix the shutdown flush.",
            Classification::Completed,
        ),
        ("May I run the test command?", Classification::Blocked),
    ] {
        let published = std::cell::Cell::new(false);
        recover_quiet_turn(
            report,
            |prompt| {
                assert!(prompt.ends_with(report));
                assert!(prompt.contains("Waiting for tests is not BLOCKED"));
                Some(classification)
            },
            || true,
            || published.set(true),
        );
        assert!(published.get());
    }
    recover_quiet_turn(
        " ",
        |_| panic!("empty report must not invoke classifier"),
        || panic!("no observation to recheck"),
        || panic!("no report is not a turn"),
    );
}

#[test]
fn watchdog_discards_a_turn_that_changed_during_classification() {
    use crate::circuit::evaluator::Classification;
    let snapshot = || QuietTurnEvidence {
        lifecycle: Some("turn-1".into()),
        input: Some("input-1".into()),
        report: Some("report-1".into()),
    };
    for change in 0..6 {
        let after = std::cell::RefCell::new(snapshot());
        let alive = std::cell::Cell::new(true);
        let quiet_ms = std::cell::Cell::new(Some(60_000));
        let status = std::cell::Cell::new(SessionStatus::Running);
        recover_quiet_turn(
            "Review complete. Approved.",
            |_| {
                match change {
                    0 => after.borrow_mut().lifecycle = Some("turn-2".into()),
                    1 => after.borrow_mut().input = Some("input-2".into()),
                    2 => after.borrow_mut().report = Some("report-2".into()),
                    3 => alive.set(false),
                    4 => quiet_ms.set(Some(0)),
                    5 => status.set(SessionStatus::Ready),
                    _ => unreachable!(),
                }
                Some(Classification::Completed)
            },
            || {
                quiet_turn_is_current(
                    &snapshot(),
                    &after.borrow(),
                    alive.get(),
                    quiet_ms.get(),
                    status.get(),
                )
            },
            || panic!("changed observation {change} must not publish attention"),
        );
    }
    // Optional correlation data may be absent, but explicit changes fence.
    let absent = QuietTurnEvidence {
        lifecycle: None,
        input: None,
        report: None,
    };
    assert!(quiet_turn_is_current(
        &absent,
        &absent,
        true,
        Some(60_000),
        SessionStatus::Running
    ));
}

#[test]
fn watchdog_needs_alive_quiet_and_still_running() {
    let running = SessionStatus::Running;
    assert!(should_check_quiet_turn(
        true,
        Some(LOST_TURN_QUIET_MS),
        running
    ));
    // Below the window: the agent may legitimately be mid-tool-call.
    assert!(!should_check_quiet_turn(
        true,
        Some(LOST_TURN_QUIET_MS - 1),
        running
    ));
    // No output timing known (never registered): conservative no-op.
    assert!(!should_check_quiet_turn(true, None, running));
    // Dead process: AgentLost observation owns that path.
    assert!(!should_check_quiet_turn(
        false,
        Some(LOST_TURN_QUIET_MS * 10),
        running
    ));
    // Already yielded (awaiting/completed/idle...): observation owns it.
    for status in [
        SessionStatus::AwaitingInput,
        SessionStatus::Completed,
        SessionStatus::Error,
    ] {
        assert!(!should_check_quiet_turn(
            true,
            Some(LOST_TURN_QUIET_MS),
            status
        ));
    }
}

// -- approvals sweep (issue #1263) -----------------------------------------

/// Build a minimal `ActiveCircuitRun` fixture with only `run.id`
/// populated — the sweep only reads `r.run.id`, every other field is
/// a placeholder. Avoids sprawling struct literals across the three
/// tests below.
pub(super) fn active_run(id: i64) -> db::ActiveCircuitRun {
    db::ActiveCircuitRun {
        run: crate::models::AutopilotCircuitRun {
            id,
            circuit_id: 0,
            mesh_id: 0,
            source_agent_node_id: None,
            trigger_identity: String::new(),
            state: String::new(),
            context_json: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            attention_dismissed_at: None,
        },
        circuit_enabled: true,
        circuit_graph_json: String::new(),
        circuit_name: String::new(),
    }
}

#[test]
fn approvals_sweep_drops_entries_for_vanished_runs() {
    const RUN_A: i64 = 910_001;
    const RUN_B: i64 = 910_002;
    const RUN_C: i64 = 910_003;
    let queued = vec![
        (RUN_A, "node-a".into()),
        (RUN_B, "node-b".into()),
        (RUN_C, "node-c".into()),
    ];

    // RUN_B vanished (deleted/completed) between the click and this
    // pass. The sweep must drop ONLY its entry; RUN_A and RUN_C
    // survive. A local queue keeps this test independent of parallel
    // callers that mutate the process-wide approvals queue.
    let active = vec![active_run(RUN_A), active_run(RUN_C)];
    let (remaining, dropped) = retain_active_approvals(queued, &active);

    assert_eq!(dropped, 1, "only the vanished run's approval is evicted");
    assert_eq!(
        remaining,
        vec![(RUN_A, "node-a".into()), (RUN_C, "node-c".into())],
        "live runs retain their approvals and queue order"
    );
}

// ---- Poison-recovery regression (issue #1224) ----
//
// Both worker statics used to be locked with `.unwrap()` — a single
// panic while holding either guard permanently poisoned the mutex and
// every subsequent call re-panicked. The recovery helper
// `lock_circuit_worker_static` calls `into_inner()` instead so the
// circuit poller keeps waking and draining approvals after any
// one-off failure. These tests are the regression pin: poison each
// static inside `catch_unwind`, then re-lock and prove normal
// push/drain/wake still works.
fn poison_circuit_worker_static<T>(mutex: &'static Mutex<T>) {
    assert!(
        !mutex.is_poisoned(),
        "the poison fixture must begin with an unpoisoned mutex"
    );
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = mutex.lock().expect("first lock must succeed (test setup)");
        panic!("intentional circuit-worker poison for issue #1224 regression test");
    }));
    assert!(
        result.is_err(),
        "test fixture must panic to poison the mutex"
    );
    assert!(
        mutex.is_poisoned(),
        "the caught panic must poison the held mutex"
    );
}

#[test]
fn approvals_recovers_from_poison() {
    poison_circuit_worker_static(&APPROVALS);
    // Production path: `request_circuit_approval` is the entrypoint
    // the IPC layer calls when a user clicks Approve. Walk through
    // it so the regression test mirrors real traffic.
    let run_id = i64::MAX - 401;
    let node_id = "issue-1224-poison-node".to_string();
    request_circuit_approval(run_id, node_id.clone());
    // Drain must find the entry that was pushed after the panic —
    // if the recovery shape regressed, the Vec would be empty.
    let drained = drain_approvals_for(run_id);
    assert_eq!(
        drained,
        vec![node_id],
        "approval pushed after mutex poison must survive into the drain (issue #1224)"
    );
    // Re-drain proves the queue is empty, not just hidden by the
    // recovery.
    assert!(
        drain_approvals_for(run_id).is_empty(),
        "drain must be idempotent after recovery"
    );
}

#[test]
fn wake_condvar_recovers_from_poison() {
    // WAKE holds a `Mutex<()>` paired with a `Condvar`. The lock
    // itself guards nothing — only the wait/notify handshake —
    // so poisoning it freezes the worker thread on the next
    // `wait_timeout`. The recovery shape is the same as for
    // APPROVALS; this test pins that path.
    let (lock, _cvar) = &*WAKE;
    poison_circuit_worker_static(lock);
    // After the panic, the OLD `.lock().unwrap()` form would now
    // return `Err(Poisoned)`. The recovery helper must hand back
    // a usable guard that can be passed to `wait_timeout` (the
    // real production call site — issue #1207 milestone 2).
    let guard = lock_circuit_worker_static(lock);
    // We deliberately do NOT call `wait_timeout` here — that would
    // block the test on the real condvar. Proving the helper
    // returns a guard is enough to assert the recovery path is
    // live; the `circuit_worker_smoke` integration test exercises
    // the full wait/notify handshake end-to-end.
    drop(guard);
}

// -- blueprint contract: per-blueprint worker seam (#1469) -----------
//
// The blueprint contract matrix (`circuit::blueprint_contract`)
// pins the walking skeleton as the canonical minimal preset. The
// worker-seam helpers below are the impure-side equivalents: the
// seam observes per-blueprint state and turns it into pure events.
// These tests pin that the seam treats the walking skeleton
// correctly — no gates/retries/closes to mishandle — and that the
// review blueprint's close_retry observer stops firing once its
// target's association clears (the otherwise-loop-forever trap).

#[test]
fn walking_skeleton_close_retry_observer_emits_nothing() {
    // The walking skeleton has no CloseAgentNode —
    // `observe_close_agent_retries` must scan all steps and find
    // zero Completed close steps. Pins the negative contract: a
    // refactor that accidentally treats every Completed step as a
    // close retry would otherwise emit spurious retries for the
    // walking skeleton.
    let mut view = RunView {
        run_id: 42,
        graph: CircuitGraph::walking_skeleton("do the thing"),
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![
            StepView {
                node_id: "spawn".into(),
                status: StepStatus::Completed,
                outcome: Some(GraphStepOutcome::Completed),
                error: None,
                agent_node_id: Some(900),
                attempt: 1,
            },
            StepView {
                node_id: "inject".into(),
                status: StepStatus::Completed,
                outcome: Some(GraphStepOutcome::Completed),
                error: None,
                agent_node_id: None,
                attempt: 1,
            },
            StepView {
                node_id: "notify".into(),
                status: StepStatus::Completed,
                outcome: Some(GraphStepOutcome::Completed),
                error: None,
                agent_node_id: None,
                attempt: 1,
            },
        ],
    };
    let mut events = Vec::new();
    observe_close_agent_retries(&view, &mut events);
    assert!(
        events.is_empty(),
        "walking skeleton has no CloseAgentNode; observer must emit nothing: {events:?}"
    );

    // Sanity: even a stray CloseAgentNode with no resolvable target
    // agent stays silent. The observer requires `resolve_target_agent`
    // to be Some, and the walking skeleton's spawn has no agent
    // lineage for a stray close to target.
    view.steps.push(StepView {
        node_id: "ghost_close".into(),
        status: StepStatus::Completed,
        outcome: Some(GraphStepOutcome::Completed),
        error: None,
        agent_node_id: None,
        attempt: 1,
    });
    observe_close_agent_retries(&view, &mut events);
    assert!(
        events.is_empty(),
        "a close with no resolvable target agent must NOT emit a retry"
    );
}

#[test]
fn walking_skeleton_spawn_recovery_treats_missing_agent_as_never_attached() {
    // The contract pins walking skeleton's only spawned node as a
    // single-slot spawn. On startup recovery, an unattached Running
    // spawn step is the commit-crash gap — the worker seam fails
    // it loudly (the only state observation nothing can repair).
    assert_eq!(
        reconcile_spawn_step(None, None),
        SpawnReconciliation::NeverAttached
    );
    // Even if the worktree is healthy-looking, an unattached spawn
    // step is still NeverAttached — there is no row to resume into.
    assert_eq!(
        reconcile_spawn_step(
            None,
            Some(ReconcileNodeState {
                archived: false,
                worktree_dir_exists: Some(true),
            })
        ),
        SpawnReconciliation::NeverAttached
    );
}

#[test]
fn review_blueprint_close_retry_observer_stops_after_target_clears() {
    // Companion to `walking_skeleton_close_retry_observer_emits_nothing`:
    // the review blueprint DOES have a `close_reviewer` step, and the
    // contract pins that the observer only re-emits the retry while
    // the spawn's agent_node_id is still attached. After the seam's
    // DB half clears the association, the observer MUST stop firing
    // (otherwise it would loop forever closing an already-closed
    // node).
    let mut view = RunView {
        run_id: 42,
        graph: CircuitGraph::issue_driven_autopilot_review("buildmesh:run"),
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![
            StepView {
                node_id: "reviewer".into(),
                status: StepStatus::Completed,
                outcome: Some(GraphStepOutcome::Completed),
                error: None,
                agent_node_id: Some(701),
                attempt: 1,
            },
            StepView {
                node_id: "close_approved".into(),
                status: StepStatus::Completed,
                outcome: Some(GraphStepOutcome::Completed),
                error: None,
                agent_node_id: None,
                attempt: 1,
            },
        ],
    };
    let mut events = Vec::new();
    observe_close_agent_retries(&view, &mut events);
    assert_eq!(
        events.len(),
        1,
        "review blueprint's close_approved MUST emit one retry while its target agent is attached"
    );
    assert!(
        matches!(&events[0], CircuitEvent::CloseAgentRetry { node_id } if node_id == "close_approved"),
        "emitted retry must be for close_approved: got {:?}",
        events[0]
    );

    // Simulate the DB-half clearing the reviewer step's agent association
    // (the worker's real CloseAgentNode effect path).
    for step in &mut view.steps {
        if step.node_id == "reviewer" {
            step.agent_node_id = None;
        }
    }
    events.clear();
    observe_close_agent_retries(&view, &mut events);
    assert!(
        events.is_empty(),
        "after the reviewer association clears, close_retry must NOT re-emit (would loop forever)"
    );
}
