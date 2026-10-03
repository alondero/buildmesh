//! Exercise the worker's decisions with RunViews and supplied observations.
use super::admission::{may_admit_run_with, observe_capacity_with, CapacityCounts};
use super::observation::{observe_close_agent_retries, observe_with};
use super::observe_parity_tests::{spawn, view, Script};
use super::restart::{
    reconcile_spawn_step, recover_run_observers_with_probe, ReconcileNodeState, SpawnReconciliation,
};
use super::turn_classify::{
    quiet_turn_is_current, recover_quiet_turn, reviewer_readiness, QuietTurnEvidence,
    ReviewerReadiness,
};
use super::*;
use crate::circuit::evaluator::Classification;
use crate::circuit::model::{CircuitEdge, CircuitNode, EdgeCondition, StepOutcome};
use crate::circuit::stepper::{Capacity, Effect};

#[test]
fn admission_preserves_supplied_queue_order_and_paused_reservations() {
    let mut pending = [
        view(spawn(), RunState::Pending, StepStatus::Queued, None),
        view(spawn(), RunState::Pending, StepStatus::Queued, None),
    ];
    pending[0].run_id = 20; // User moved this run ahead of the older run.
    pending[1].run_id = 10;
    for run in &mut pending {
        run.graph.nodes.push(CircuitNode {
            id: "trigger".into(),
            kind: CircuitNodeKind::Manual,
        });
        run.graph.edges.push(CircuitEdge {
            from: "trigger".into(),
            to: "step".into(),
            condition: EdgeCondition::Always,
        });
    }
    let mut admitted = 1; // A paused run retains one Mesh slot.
    let mut started = Vec::new();
    for run in &mut pending {
        if may_admit_run_with(run.state, 2, || Some(admitted)) {
            let transition = advance(run, &CircuitEvent::Triggered);
            assert_eq!(run.state, RunState::Running);
            assert!(transition.run_state_changed);
            let transition = advance(
                run,
                &observe_capacity_with(
                    1,
                    CapacityCounts {
                        circuit_running: 0,
                        reserved_for_run: 1,
                        owned_by_run: 0,
                        global_free_slots: 1,
                    },
                ),
            );
            assert!(transition
                .step_writes
                .iter()
                .any(|write| write.status == StepStatus::Running));
            assert!(transition.effects.contains(&Effect::SpawnAgentNode {
                node_id: "step".into()
            }));
            started.push(run.run_id);
            admitted += 1;
        }
    }
    assert_eq!(started, vec![20]);
    assert_eq!(pending[1].state, RunState::Pending);
    assert!(
        !may_admit_run_with(RunState::Pending, 2, || None),
        "unknown counts fail closed"
    );
    for state in [RunState::Running, RunState::Paused] {
        assert!(may_admit_run_with(state, 2, || panic!(
            "admitted runs need no count read"
        )));
    }
}

#[test]
fn capacity_tick_uses_lease_and_live_pool_and_never_restarts_cancelled_work() {
    let counts = CapacityCounts {
        circuit_running: 1,
        reserved_for_run: 3,
        owned_by_run: 1,
        global_free_slots: 1,
    };
    assert!(matches!(
        observe_capacity_with(3, counts),
        CircuitEvent::Tick(Capacity {
            circuit_free_slots: 2,
            agent_free_slots: 1,
        })
    ));
    let mut run = view(spawn(), RunState::Running, StepStatus::Queued, None);
    let transition = advance(
        &mut run,
        &observe_capacity_with(
            1,
            CapacityCounts {
                circuit_running: i64::MAX,
                reserved_for_run: 0,
                owned_by_run: 0,
                global_free_slots: 0,
            },
        ),
    );
    assert!(transition.effects.is_empty());
    assert_eq!(run.step("step").unwrap().status, StepStatus::Queued);

    run.steps[0].status = StepStatus::Running;
    run.steps[0].agent_node_id = Some(42);
    let transition = advance(&mut run, &CircuitEvent::AgentLost { agent_node_id: 42 });
    assert_eq!(run.state, RunState::Failed);
    assert_eq!(run.step("step").unwrap().status, StepStatus::Cancelled);
    assert!(transition
        .step_writes
        .iter()
        .any(|write| write.status == StepStatus::Cancelled
            && write.outcome == Some(Some(StepOutcome::Cancelled))));
    assert!(!transition.effects.iter().any(|effect| matches!(
        effect,
        Effect::SpawnAgentNode { .. } | Effect::InjectPty { .. }
    )));
    // Explicit user cancellation arrives with already-terminalised steps;
    // the DB cancellation setter owns those writes, rather than a later Tick.
    run.state = RunState::Cancelled;
    let transition = advance(
        &mut run,
        &CircuitEvent::Tick(Capacity {
            circuit_free_slots: 1,
            agent_free_slots: 1,
        }),
    );
    assert_eq!(run.state, RunState::Cancelled);
    assert_eq!(run.step("step").unwrap().status, StepStatus::Cancelled);
    assert!(transition.step_writes.is_empty());
    assert!(transition.effects.is_empty());
}

#[test]
fn ambiguous_restart_persists_unverified_and_never_spawns_again() {
    let mut run = view(spawn(), RunState::Running, StepStatus::Running, None);
    assert_eq!(
        reconcile_spawn_step(None, None),
        SpawnReconciliation::NeverAttached
    );
    let events = observe_with(&run, &mut Script::default());
    let event = events
        .iter()
        .find(|event| matches!(event, CircuitEvent::EffectUncertain { .. }))
        .unwrap();
    let transition = advance(&mut run, event);
    assert_eq!(run.step("step").unwrap().status, StepStatus::Unverified);
    assert_eq!(transition.step_writes[0].status, StepStatus::Unverified);
    assert!(transition.effects.is_empty());
    assert_eq!(
        reconcile_spawn_step(
            Some(42),
            Some(ReconcileNodeState {
                archived: false,
                worktree_dir_exists: Some(true),
            })
        ),
        SpawnReconciliation::Leave
    );
    assert_eq!(
        reconcile_spawn_step(
            Some(42),
            Some(ReconcileNodeState {
                archived: false,
                worktree_dir_exists: Some(false),
            })
        ),
        SpawnReconciliation::Lost
    );
}

#[test]
fn observer_reattachment_uses_injected_liveness_and_probe_and_retains_retry() {
    let mut run = view(spawn(), RunState::Running, StepStatus::Completed, Some(42));
    run.context.set("source.agent_id", "42"); // Same agent in two ownership paths.
    let mut attempts = Vec::new();
    recover_run_observers_with_probe(
        &run,
        |_| true,
        |_| true,
        |id| {
            attempts.push(id);
            Err("watcher unavailable".into())
        },
    );
    recover_run_observers_with_probe(
        &run,
        |_| true,
        |_| true,
        |id| {
            attempts.push(id);
            Ok(())
        },
    );
    assert_eq!(
        attempts,
        vec![42, 42],
        "a failed recovery remains eligible on the next supplied probe"
    );
    recover_run_observers_with_probe(
        &run,
        |_| true,
        |_| false,
        |_| panic!("cooldown denies reattachment"),
    );
    recover_run_observers_with_probe(
        &run,
        |_| false,
        |_| panic!("dead agent must not probe"),
        |_| panic!("dead agent"),
    );
    run.state = RunState::Cancelled;
    recover_run_observers_with_probe(
        &run,
        |_| panic!("cancelled run"),
        |_| panic!("cancelled run"),
        |_| panic!("cancelled run"),
    );
}

#[test]
fn cleanup_failure_replays_only_the_owned_round_and_stops_after_acknowledgement() {
    let mut run = view(
        CircuitNodeKind::CloseAgentNode {
            target_node_id: Some("spawn".into()),
        },
        RunState::Running,
        StepStatus::Completed,
        None,
    );
    run.graph.nodes.push(CircuitNode {
        id: "spawn".into(),
        kind: spawn(),
    });
    run.graph.edges.push(CircuitEdge {
        from: "spawn".into(),
        to: "step".into(),
        condition: EdgeCondition::Always,
    });
    run.steps.push(StepView {
        node_id: "spawn".into(),
        status: StepStatus::Completed,
        attempt: 1,
        agent_node_id: Some(42),
        outcome: Some(StepOutcome::Completed),
        error: None,
    });
    for _ in 0..2 {
        // The destructive effect failed; ownership remains durable.
        let mut events = Vec::new();
        observe_close_agent_retries(&run, &mut events);
        assert_eq!(events.len(), 1);
        let transition = advance(&mut run, &events[0]);
        assert_eq!(
            transition.effects,
            vec![Effect::CloseAgentNode {
                node_id: "step".into(),
                target_node_id: Some("spawn".into())
            }]
        );
        assert!(transition.step_writes.is_empty());
    }
    run.steps[1].attempt = 2;
    let mut events = Vec::new();
    observe_close_agent_retries(&run, &mut events);
    assert!(
        events.is_empty(),
        "old close cannot retire the next round's agent"
    );
    run.steps[1].agent_node_id = None;
    observe_close_agent_retries(&run, &mut events);
    assert!(events.is_empty());
}

#[test]
fn quiet_completion_rejects_stale_input_lifecycle_or_report_and_keeps_needs_input_distinct() {
    let before = QuietTurnEvidence {
        lifecycle: Some("turn:1".into()),
        input: Some("input:1".into()),
        report: Some("report:1".into()),
    };
    for changed in [
        QuietTurnEvidence {
            lifecycle: Some("turn:2".into()),
            ..before.clone()
        },
        QuietTurnEvidence {
            input: Some("input:2".into()),
            ..before.clone()
        },
        QuietTurnEvidence {
            report: Some("report:2".into()),
            ..before.clone()
        },
    ] {
        recover_quiet_turn(
            "finished",
            |_| Some(Classification::Completed),
            || {
                quiet_turn_is_current(
                    &before,
                    &changed,
                    true,
                    Some(60_000),
                    SessionStatus::Running,
                )
            },
            || panic!("stale completion must never publish"),
        );
    }
    for verdict in [Classification::Working, Classification::Continue] {
        recover_quiet_turn(
            "still working",
            |_| Some(verdict),
            || true,
            || panic!("not a completed turn or input request"),
        );
    }
    let mut published = 0;
    recover_quiet_turn(
        "please grant permission",
        |_| Some(Classification::Blocked),
        || true,
        || published += 1,
    );
    assert_eq!(published, 1);
    let run = view(
        CircuitNodeKind::ReviewVerdict {
            target_node_id: None,
        },
        RunState::Running,
        StepStatus::Running,
        Some(42),
    );
    assert_eq!(
        reviewer_readiness(
            &run,
            "step",
            SessionStatus::AwaitingInput,
            "Let me retry the review",
            |_| Some(Classification::Working)
        ),
        ReviewerReadiness::Working
    );
    assert_eq!(
        reviewer_readiness(
            &run,
            "step",
            SessionStatus::AwaitingInput,
            "Need your permission",
            |_| Some(Classification::Blocked)
        ),
        ReviewerReadiness::Reportable
    );
    assert_eq!(
        reviewer_readiness(&run, "step", SessionStatus::AwaitingInput, "report", |_| {
            None
        }),
        ReviewerReadiness::Unavailable
    );
}
