use super::admission::*;
use super::github::*;
use super::observation::*;
use super::restart::*;
use super::spawn::*;
use super::turn_classify::*;
use super::*;
use crate::agent::spawn::ExplicitSpawnOverrides;
use crate::circuit::model::{CircuitNode, StepOutcome, CIRCUIT_GRAPH_VERSION};
use crate::circuit::test_support::advance_with_report_evidence;
use rusqlite::{Connection, OptionalExtension};

#[test]
fn worker_effect_persistence_mapping_covers_every_effect_variant() {
    use crate::circuit::model::{
        CircuitEdge, CircuitNodeKind, GithubActionKind, SessionStatusKind,
    };
    use crate::circuit::stepper::Effect;

    let view = RunView {
        run_id: 1908,
        state: RunState::Running,
        graph: CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                CircuitNode {
                    id: "spawn".into(),
                    kind: CircuitNodeKind::SpawnAgentNode {
                        prompt: "work".into(),
                        name: None,
                        provider: None,
                        model: None,
                        effort: None,
                        extra_args: None,
                        timeout_seconds: None,
                    },
                },
                CircuitNode {
                    id: "status".into(),
                    kind: CircuitNodeKind::SetNodeStatus {
                        status: SessionStatusKind::Completed,
                        target_node_id: Some("spawn".into()),
                    },
                },
            ],
            edges: vec![CircuitEdge {
                from: "spawn".into(),
                to: "status".into(),
                condition: Default::default(),
            }],
        },
        context: CircuitContext::default(),
        steps: vec![
            StepView {
                node_id: "spawn".into(),
                status: StepStatus::Running,
                outcome: None,
                error: None,
                agent_node_id: Some(101),
                attempt: 4,
            },
            StepView {
                node_id: "status".into(),
                status: StepStatus::Running,
                outcome: None,
                error: None,
                agent_node_id: None,
                attempt: 2,
            },
        ],
    };
    let effects = vec![
        Effect::SpawnAgentNode {
            node_id: "spawn".into(),
        },
        Effect::InjectPty {
            node_id: "status".into(),
            target_node_id: Some("spawn".into()),
            prompt: "follow up".into(),
        },
        Effect::CallGithub {
            node_id: "status".into(),
            action: GithubActionKind::PostComment,
            label: None,
            comment: Some("done".into()),
        },
        Effect::SetNodeStatus {
            node_id: "status".into(),
            status: "completed".into(),
            target_node_id: Some("spawn".into()),
        },
        Effect::ContinueAgentTurn {
            node_id: "status".into(),
            target_agent_id: 101,
            prompt: "continue".into(),
        },
        Effect::CloseAgentNode {
            node_id: "status".into(),
            target_node_id: Some("spawn".into()),
        },
        Effect::Notify {
            message: "finished".into(),
        },
    ];

    let (intents, status_effects) = effect_persistence_writes(&view, &effects).unwrap();
    assert_eq!(
        intents,
        vec![
            db::circuit::evidence::EffectIntent {
                node_id: "spawn".into(),
                attempt: 4,
                kind: db::circuit::evidence::EffectKind::Spawn,
            },
            db::circuit::evidence::EffectIntent {
                node_id: "status".into(),
                attempt: 2,
                kind: db::circuit::evidence::EffectKind::Prompt,
            },
            db::circuit::evidence::EffectIntent {
                node_id: "status".into(),
                attempt: 2,
                kind: db::circuit::evidence::EffectKind::Github,
            },
        ]
    );
    assert_eq!(
        status_effects,
        vec![db::circuit::evidence::AgentStatusEffect {
            node_id: "status".into(),
            attempt: 2,
            agent_node_id: 101,
            status: crate::models::SessionStatus::Completed,
        }]
    );
}

#[test]
fn prompt_submission_revision_allows_delivery_commit_without_weakening_fences() {
    use crate::circuit::model::{CircuitGraph, CircuitNodeKind};
    for (stale_revision, cancelled) in [(true, false), (false, false), (false, true)] {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        let graph = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            edges: vec![],
            nodes: vec![CircuitNode {
                id: "feedback".into(),
                kind: CircuitNodeKind::InjectPty {
                    target_node_id: Some("$source".into()),
                    prompt: "Apply the review findings".into(),
                },
            }],
        };
        conn.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(9,1,'source','/repo','ready');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'feedback',1,'running');
            INSERT INTO circuit_effects VALUES(1,'feedback',1,'prompt','possible_dispatch');").unwrap();
        conn.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [graph.to_json().unwrap()],
        )
        .unwrap();
        let mut view = RunView {
            run_id: 1,
            state: RunState::Running,
            graph,
            context: CircuitContext::default(),
            steps: vec![StepView {
                node_id: "feedback".into(),
                status: StepStatus::Running,
                attempt: 1,
                outcome: None,
                error: None,
                agent_node_id: None,
            }],
        };
        view.context.set("source.agent_id", "9");
        view.context
            .set("node.feedback.prompt_delivery.1", "intent");
        let revision = db::circuit::evidence::record_prompt_submission_locked(
            &conn,
            1,
            "feedback",
            1,
            9,
            "Apply the review findings",
        )
        .unwrap();
        assert!(revision > 0);
        view.context.set(
            "evidence.revision",
            if stale_revision { 0 } else { revision }.to_string(),
        );
        if cancelled {
            conn.execute("UPDATE autopilot_circuit_runs SET state='cancelled'", [])
                .unwrap();
        }
        let event = CircuitEvent::PromptDelivered {
            node_id: "feedback".into(),
            attempt: 1,
        };
        let result = advance_and_persist_observed_event(&mut view, &event, |view, transition| {
            persist_transition_checked_with(
                1,
                view,
                transition,
                |run_id, state, context, steps, evidence| {
                    db::circuit::evidence::commit_transition_locked(
                        &mut conn, run_id, state, context, steps, evidence,
                    )
                },
            )
        });
        assert_eq!(result.is_ok(), !stale_revision && !cancelled);
        let (status, effect): (String, String) = conn.query_row(
            "SELECT s.status,e.state FROM autopilot_circuit_run_steps s JOIN circuit_effects e ON e.run_id=s.run_id",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        if stale_revision || cancelled {
            assert_eq!(
                (status.as_str(), effect.as_str()),
                ("running", "possible_dispatch")
            );
        } else {
            assert_eq!(
                (status.as_str(), effect.as_str()),
                ("completed", "acknowledged")
            );
            assert_eq!(
                view.context.get("node.feedback.prompt_delivery.1"),
                Some("acknowledged")
            );
        }
    }
}

#[test]
fn prompt_acknowledgement_requires_completed_submission() {
    for result in [Ok(false), Err("Enter write failed".to_string())] {
        let delivered = dispatch_prompt_and_acknowledge(
            || result,
            || panic!("an unsent or failed submission must never write a delivery receipt"),
        );
        assert!(delivered.is_err());
    }
    let result =
        dispatch_prompt_and_acknowledge(|| Ok(true), || Err("receipt commit failed".into()));
    assert_eq!(result, Err("receipt commit failed".into()));
    assert_eq!(
        dispatch_prompt_and_acknowledge(|| Ok(true), || Ok(None)).unwrap(),
        None,
        "a cancelled or superseded attempt does not emit a delivery event"
    );
}

#[test]
fn unavailable_set_node_status_target_fails_run_without_retrying_forever() {
    use crate::circuit::model::{
        CircuitEdge, CircuitGraph, CircuitNodeKind, EdgeCondition, SessionStatusKind,
    };
    use crate::circuit::stepper::{Capacity, Effect};

    for (archived, cancel_before_failure_commit) in [(false, false), (true, false), (false, true)] {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        let graph = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                CircuitNode {
                    id: "agent".into(),
                    kind: CircuitNodeKind::SpawnAgentNode {
                        prompt: "work".into(),
                        name: None,
                        provider: None,
                        model: None,
                        effort: None,
                        extra_args: None,
                        timeout_seconds: None,
                    },
                },
                CircuitNode {
                    id: "status".into(),
                    kind: CircuitNodeKind::SetNodeStatus {
                        status: SessionStatusKind::Running,
                        target_node_id: Some("agent".into()),
                    },
                },
            ],
            edges: vec![CircuitEdge {
                from: "agent".into(),
                to: "status".into(),
                condition: EdgeCondition::Always,
            }],
        };
        conn.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(9,1,'agent','/repo','ready');")
            .unwrap();
        crate::db::circuit::ledger::create_autopilot_circuit_inner(
            &conn,
            1,
            "status target failure",
            "",
            &graph.to_json().unwrap(),
        )
        .unwrap();
        conn.execute_batch("INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
                VALUES(1,'agent',1,'completed',9),(1,'status',1,'pending_slot',NULL);")
            .unwrap();
        if archived {
            conn.execute("UPDATE agent_nodes SET status='archived' WHERE id=9", [])
                .unwrap();
        } else {
            conn.execute("DELETE FROM agent_nodes WHERE id=9", [])
                .unwrap();
        }

        let mut view = RunView {
            run_id: 1,
            state: RunState::Running,
            graph,
            context: CircuitContext::default(),
            steps: vec![
                StepView {
                    node_id: "agent".into(),
                    status: StepStatus::Completed,
                    outcome: Some(StepOutcome::Completed),
                    error: None,
                    agent_node_id: Some(9),
                    attempt: 1,
                },
                StepView {
                    node_id: "status".into(),
                    status: StepStatus::Queued,
                    outcome: None,
                    error: None,
                    agent_node_id: None,
                    attempt: 1,
                },
            ],
        };
        let event = CircuitEvent::Tick(Capacity {
            agent_free_slots: 1,
        });
        let result = advance_and_persist_observed_event(&mut view, &event, |view, transition| {
            persist_transition_checked_with(
                1,
                view,
                transition,
                |run_id, state, context, steps, evidence| {
                    db::circuit::evidence::commit_transition_locked(
                        &mut conn, run_id, state, context, steps, evidence,
                    )
                },
            )
        });
        let (expected, effects, message) = match result {
            Err(TransitionPersistFailure::AgentStatusEffectRejected {
                expected,
                effects,
                message,
            }) => (expected, effects, message),
            other => panic!("expected a rejected local status effect, got {other:?}"),
        };
        assert!(effects.iter().any(
            |effect| matches!(effect, Effect::SetNodeStatus { node_id, .. } if node_id == "status")
        ));
        assert_eq!(
            view.step("status").unwrap().status,
            StepStatus::Queued,
            "failed transition persistence must restore the pre-event view before terminalizing it"
        );
        if cancel_before_failure_commit {
            crate::db::circuit::ledger::cancel_circuit_run_locked(&mut conn, 1).unwrap();
        }
        let failure_commit = fail_rejected_agent_status_effects(
            &mut view,
            &expected,
            &effects,
            &message,
            |context, steps, expected| {
                db::circuit::evidence::commit_transition_locked(
                    &mut conn,
                    1,
                    Some(RunState::Failed.as_db_str()),
                    context,
                    steps,
                    db::circuit::evidence::EvidenceWrite {
                        expected: Some(expected),
                        ..Default::default()
                    },
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
            },
        );

        if cancel_before_failure_commit {
            assert!(
                failure_commit.is_err(),
                "cancellation must invalidate the failure fallback fence"
            );
            let durable_state: String = conn
                .query_row(
                    "SELECT state FROM autopilot_circuit_runs WHERE id=1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(durable_state, "cancelled");
            assert_eq!(
                view.state,
                RunState::Running,
                "a rejected failure fallback must not mutate the stale view"
            );
            continue;
        }
        failure_commit.unwrap();

        assert_eq!(view.state, RunState::Failed);
        assert_eq!(view.step("status").unwrap().status, StepStatus::Failed);
        let durable_state: String = conn
            .query_row(
                "SELECT state FROM autopilot_circuit_runs WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let durable_step: String = conn.query_row(
            "SELECT status FROM autopilot_circuit_run_steps WHERE run_id=1 AND node_id='status'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(
            (durable_state.as_str(), durable_step.as_str()),
            ("failed", "failed")
        );
        let target_status: Option<String> = conn
            .query_row("SELECT status FROM agent_nodes WHERE id=9", [], |row| {
                row.get(0)
            })
            .optional()
            .unwrap();
        assert_eq!(target_status.as_deref(), archived.then_some("archived"));
    }
}

#[test]
fn codex_recheck_input_fence_rejection_commits_unverified_without_stale_evidence() {
    use crate::circuit::observation::{CircuitObservation, ObservationIdentity, ObservedWorkFact};
    use crate::circuit::stepper::{ObservationInputFence, StepView};

    let identity = ObservationIdentity {
        run_id: 42,
        step_id: "spawn".into(),
        attempt: 1,
        agent_node_id: 9,
        session_incarnation: Some("1000".into()),
        session_id: Some("session".into()),
        turn_id: Some("turn-1".into()),
        report_revision: None,
    };
    let mut view = RunView {
        run_id: 42,
        state: RunState::Running,
        graph: CircuitGraph::walking_skeleton("synthetic"),
        context: CircuitContext::default(),
        steps: vec![StepView {
            node_id: "spawn".into(),
            attempt: 1,
            status: StepStatus::Running,
            outcome: None,
            error: None,
            agent_node_id: Some(9),
        }],
    };
    view.context.set("node.spawn.recheck_only", "1");
    let observations = [
        ObservedWorkFact::ForegroundTerminated,
        ObservedWorkFact::OwnershipUnavailable {
            reason: "Codex cannot enumerate owned work".into(),
        },
    ]
    .into_iter()
    .enumerate()
    .map(|(index, fact)| CircuitObservation {
        identity: identity.clone(),
        source: "codex_rollout_task_complete".into(),
        source_id: Some(format!("turn-1:{index}")),
        observed_at_ms: 2_000,
        authoritative: true,
        fact,
    })
    .collect();
    let event = CircuitEvent::ObservationBatch {
        receipt_id: 0,
        expected: identity,
        observations,
        stale: false,
        input_guard: Some(ObservationInputFence {
            transcript_guard: None,
            report_guard: None,
            agent_node_id: 9,
            input_stamp: "1:2".into(),
            observed_at_ms: 2_000,
            session_id: "session".into(),
            session_incarnation: "1000".into(),
        }),
    };

    let mut persist_calls = 0;
    let mut committed_step = None;
    let (transition, turn_boundary_changed) =
        advance_and_persist_observed_event(&mut view, &event, |view, transition| {
            persist_calls += 1;
            if persist_calls == 1 {
                assert!(transition.input_guard.is_some());
                return Err(TransitionPersistFailure::FreshnessRejected(
                    "commit failed: submitted input is newer than the Codex completion".into(),
                ));
            }
            assert!(transition.input_guard.is_none());
            assert!(transition.observations.is_empty());
            assert!(transition.effects.is_empty());
            committed_step = Some(view.steps[0].clone());
            Ok(false)
        })
        .unwrap();

    assert_eq!(persist_calls, 2);
    assert!(!turn_boundary_changed);
    assert_eq!(transition.step_writes.len(), 1);
    assert_eq!(transition.step_writes[0].status, StepStatus::Unverified);
    assert_eq!(transition.step_writes[0].attempt, 1);
    assert_eq!(committed_step.unwrap().status, StepStatus::Unverified);
    assert_eq!(view.steps[0].status, StepStatus::Unverified);
    assert_eq!(view.steps[0].attempt, 1);
    assert!(view.steps[0]
        .error
        .as_deref()
        .unwrap()
        .contains("freshness fence"));
    assert!(
        view.context.get("node.spawn.evidence.1").is_none(),
        "rejected observations must not be retained"
    );
    assert_eq!(view.context.get("node.spawn.recheck_only"), Some("1"));
}

#[test]
fn circuit_completion_allows_its_terminal_actions_but_not_stale_effects_or_spawns() {
    use crate::circuit::stepper::Effect;
    let notify = Effect::Notify {
        message: "approved".into(),
    };
    let set_status = Effect::SetNodeStatus {
        node_id: "source-status".into(),
        status: "completed".into(),
        target_node_id: Some("$source".into()),
    };
    let close = Effect::CloseAgentNode {
        node_id: "close".into(),
        target_node_id: Some("reviewer".into()),
    };
    let spawn = Effect::SpawnAgentNode {
        node_id: "reviewer".into(),
    };
    let inject = Effect::InjectPty {
        node_id: "feedback".into(),
        target_node_id: Some("$source".into()),
        prompt: "fix".into(),
    };
    assert!(effect_allowed_in_state("completed", true, &notify));
    assert!(effect_allowed_in_state("completed", true, &set_status));
    assert!(effect_allowed_in_state("completed", true, &close));
    assert!(!effect_allowed_in_state("completed", true, &spawn));
    assert!(!effect_allowed_in_state("completed", true, &inject));
    for effect in [notify, set_status, close, spawn, inject] {
        assert!(!effect_allowed_in_state("completed", false, &effect));
        assert!(!effect_allowed_in_state("cancelled", true, &effect));
        assert_eq!(
            effect_allowed_in_state("failed", true, &effect),
            matches!(
                effect,
                Effect::Notify { .. }
                    | Effect::SetNodeStatus { .. }
                    | Effect::CloseAgentNode { .. }
            )
        );
    }
}

#[test]
fn terminal_cleanup_injected_kill_or_archive_failure_keeps_notifications_quiet() {
    let setup = || {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn.execute_batch("INSERT INTO meshes (id, name, path) VALUES (1, 'cleanup-failure', '/repo');
            INSERT INTO agent_nodes (id, mesh_id, name, path, status) VALUES (42,1,'owned','/repo','ready'), (41,1,'implementer','/repo','ready');
            INSERT INTO autopilot_circuits (id, mesh_id, name, graph_json) VALUES (1,1,'cleanup-failure','{}');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,trigger_identity,state,context_json)
                VALUES (1,1,1,'failure','failed','{\"cleanup.pending\":\"1\"}');
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,agent_node_id,parent_agent_node_id,status)
                VALUES (1,'owned',42,41,'failed');").unwrap();
        let claim = crate::db::circuit::claim_circuit_agent_cleanup_inner(&conn, 42)
            .unwrap()
            .unwrap();
        (conn, claim)
    };
    let assert_recovery = |conn: &Connection| {
        assert!(
            crate::db::circuit::circuit_agent_cleanup_claim_inner(conn, 42)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            crate::db::circuit::failed_circuit_agents_for_cleanup_inner(conn).unwrap(),
            vec![42]
        );
        assert_eq!(
            conn.query_row(
                "SELECT cleanup_requested FROM agent_node_lifecycle_leases WHERE node_id=42",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT status FROM agent_nodes WHERE id=42", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
            "ready"
        );
        assert_eq!(conn.query_row("SELECT agent_node_id FROM autopilot_circuit_run_steps WHERE run_id=1 AND node_id='owned'", [], |row| row.get::<_, i64>(0)).unwrap(), 42);
        assert!(conn.query_row("SELECT json_extract(context_json, '$.\"cleanup.retired.42\"') FROM autopilot_circuit_runs WHERE id=1", [], |row| row.get::<_, Option<String>>(0)).unwrap().is_none());
    };

    let (kill_conn, kill_claim) = setup();
    let kill_failed = archive_failed_circuit_agent_with(
        42,
        || Err("kill failed".to_string()),
        || Ok(Vec::new()),
        |_, _| panic!("cleanup notification must follow committed archive"),
        |error| {
            crate::db::circuit::release_circuit_agent_cleanup_inner(&kill_conn, 42, &kill_claim)
                .unwrap_or_else(|release| panic!("{error}: {release}"))
        },
    );
    assert_eq!(kill_failed, Err("kill failed".to_string()));
    assert_recovery(&kill_conn);

    let (archive_conn, archive_claim) = setup();
    let archive_failed = archive_failed_circuit_agent_with(
        42,
        || Ok(()),
        || Err(rusqlite::Error::InvalidQuery),
        |_, _| panic!("cleanup notification must follow committed archive"),
        |error| {
            crate::db::circuit::release_circuit_agent_cleanup_inner(
                &archive_conn,
                42,
                &archive_claim,
            )
            .unwrap_or_else(|release| panic!("{error}: {release}"))
        },
    );
    assert!(archive_failed.is_err());
    assert_recovery(&archive_conn);
}

fn safety(has_uncommitted: bool) -> crate::git::worktree::WorktreeCloseSafety {
    crate::git::worktree::WorktreeCloseSafety {
        worktree_path: Some("/repo".into()),
        has_uncommitted,
        has_unpushed: true,
        is_detached: false,
    }
}

#[test]
fn a_close_never_deletes_an_implementation_worktree_with_uncommitted_changes() {
    let blocker = close_blocker(false, &safety(true)).expect("dirty work is protected");
    assert!(blocker.contains("uncommitted changes"), "{blocker}");

    // A clean worktree closes, whatever its commits' push state: after a
    // squash-merge the branch's own commits never appear in the base.
    assert_eq!(close_blocker(false, &safety(false)), None);
}

#[test]
fn a_helper_agent_is_closed_regardless_of_its_worktree() {
    // The caller turns a failed safety lookup into has_uncommitted=true before
    // reaching close_blocker, so helpers still close on the same path.
    assert_eq!(close_blocker(true, &safety(true)), None);
    assert_eq!(close_blocker(true, &safety(false)), None);
}

#[test]
fn notification_severity_does_not_call_unapproved_findings_success() {
    assert_eq!(
        notification_severity("Review approved for Fix parser"),
        "success"
    );
    assert_eq!(
        notification_severity("Latest fixes have not been approved; inspect the report"),
        "warning"
    );
    assert_eq!(
        notification_severity("Review not approved for Fix parser"),
        "warning"
    );
    assert_eq!(notification_severity("Review needs attention"), "warning");
}

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
        Some(ObserverRestart::Muse)
    );
    assert_eq!(
        observer_restart(&observer_node("commandcode", Some("sid"))),
        Some(ObserverRestart::CommandCode)
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
    let mut node = db::create_agent_node(
        mesh.id,
        "Assigned work",
        path,
        "work",
        crate::models::EnvType::Windows,
        "terminal",
        None,
        None,
        None,
        None,
        false,
        None,
        None,
        None,
    )
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
    let node = db::create_agent_node(
        mesh.id,
        "Finished source",
        path,
        "work",
        crate::models::EnvType::Windows,
        "terminal",
        None,
        None,
        None,
        None,
        false,
        None,
        None,
        None,
    )
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

#[test]
fn circuit_continuation_rejects_user_input_regeneration_and_new_report() {
    let mut view = report_gate_view();
    view.context
        .set("node.finish_classifier.continuation.stamp", "100:yield");
    view.context
        .set("node.finish_classifier.continuation.revision", "report-1");
    let valid = |status, stamp, revision| {
        continuation_is_current(
            &view,
            "finish_classifier",
            900,
            status,
            true,
            Some(stamp),
            Some(revision),
        )
    };
    assert!(valid(SessionStatus::Ready, "100:yield", "report-1"));
    // A finished turn that owes a result file is reminded as Completed, so the
    // reminder must still be accepted when its delivery is confirmed current.
    assert!(valid(SessionStatus::Completed, "100:yield", "report-1"));
    assert!(!valid(SessionStatus::Running, "100:yield", "report-1"));
    assert!(!valid(SessionStatus::Ready, "200:yield", "report-1"));
    assert!(!valid(SessionStatus::Ready, "100:new-input", "report-1"));
    assert!(!valid(SessionStatus::Ready, "100:yield", "report-2"));
}

#[test]
fn circuit_continuation_accepts_only_the_agent_a_spawn_step_owns() {
    let spawn_node = CircuitNode {
        id: "implement".into(),
        kind: crate::circuit::model::CircuitNodeKind::SpawnAgentNode {
            prompt: "implement".into(),
            name: None,
            provider: None,
            model: None,
            effort: None,
            extra_args: None,
            timeout_seconds: None,
        },
    };
    let mut view = RunView {
        run_id: 27,
        graph: crate::circuit::model::CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![spawn_node],
            edges: vec![],
        },
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![StepView {
            node_id: "implement".into(),
            status: StepStatus::Running,
            agent_node_id: Some(900),
            attempt: 1,
            outcome: None,
            error: None,
        }],
    };
    view.context
        .set("node.implement.continuation.stamp", "100:yield");
    view.context
        .set("node.implement.continuation.revision", "report-1");
    let valid = |target| {
        continuation_is_current(
            &view,
            "implement",
            target,
            SessionStatus::Ready,
            true,
            Some("100:yield"),
            Some("report-1"),
        )
    };
    assert!(valid(900));
    assert!(!valid(901));
}

#[test]
fn circuit_report_dedupe_reconsiders_identical_text_from_a_new_native_turn() {
    let mut view = report_gate_view();
    let report = "Still working";
    advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "finish_classifier".into(),
            classification: Some(crate::circuit::evaluator::Classification::Working),
            output: Some(report.into()),
        },
    );
    assert!(!should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        report,
        None
    ));
    let old_revision = view
        .context
        .get("node.finish_classifier.classified_report_revision")
        .unwrap()
        .to_string();
    let binding = crate::circuit::test_support::record_report_evidence_for_turn(
        &mut view,
        "finish_classifier",
        report,
        "new-native-turn",
    );
    assert_eq!(
        binding.report_revision, old_revision,
        "same text digest, different turn identity"
    );
    assert!(should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        report,
        None
    ));
    assert!(has_unconsumed_classifier_evidence(
        &view,
        "finish_classifier"
    ));
    advance(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: Some(binding),
            node_id: "finish_classifier".into(),
            classification: Some(crate::circuit::evaluator::Classification::Working),
            output: Some(report.into()),
        },
    );
    assert!(!has_unconsumed_classifier_evidence(
        &view,
        "finish_classifier"
    ));
}

#[test]
fn circuit_report_dedupe_is_durable_and_scoped_to_gate_attempt() {
    use crate::circuit::evaluator::Classification;
    let mut view = report_gate_view();
    let report = "Still working; waiting for the requested credentials.";
    // The implementation gate consumed the same agent's output. That
    // global clock must not suppress this finish gate's first evaluation.
    assert!(should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        report,
        Some(0)
    ));
    advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "finish_classifier".into(),
            classification: Some(Classification::Working),
            output: Some(report.into()),
        },
    );
    view.context = CircuitContext::from_json(&view.context.to_json().unwrap()).unwrap();
    assert!(
        !should_classify_report(
            &view,
            "finish_classifier",
            SessionStatus::AwaitingInput,
            report,
            None
        ),
        "restart must not reclassify a consumed report"
    );
    assert!(
        should_classify_report(
            &view,
            "finish_classifier",
            SessionStatus::AwaitingInput,
            "Wrap-up complete",
            None
        ),
        "recover a new transcript without PTY clocks"
    );
    view.steps[0].attempt += 1;
    assert!(
        should_classify_report(
            &view,
            "finish_classifier",
            SessionStatus::AwaitingInput,
            report,
            Some(0)
        ),
        "a new round may produce an identical report"
    );
    view.state = RunState::Paused;
    assert!(!should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        report,
        None
    ));
}

#[test]
fn circuit_classifier_failure_retries_silent_turn_and_persists_reason() {
    let mut view = report_gate_view();
    let report = "Wrap-up complete: all tests passed and the PR is open.";
    let result = advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "finish_classifier".into(),
            classification: None,
            output: Some(report.into()),
        },
    );
    assert!(result.step_writes[0]
        .error
        .as_ref()
        .and_then(|error| error.as_ref())
        .is_some_and(|error| error.contains("retrying")));
    assert!(!should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        report,
        Some(59_999)
    ));
    assert!(should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        report,
        Some(60_000)
    ));
    assert!(
        should_classify_report(
            &view,
            "finish_classifier",
            SessionStatus::AwaitingInput,
            report,
            None
        ),
        "restart permits recovery"
    );
    advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "finish_classifier".into(),
            classification: Some(crate::circuit::evaluator::Classification::Completed),
            output: Some(report.into()),
        },
    );
    assert_eq!(
        view.step("finish_classifier").unwrap().status,
        StepStatus::Completed
    );
    assert!(view.step("finish_classifier").unwrap().error.is_none());
    assert_eq!(
        view.step("open_pr").map(|step| step.status),
        Some(StepStatus::Running),
        "{:?}",
        view.steps
    );
    assert!(!should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        report,
        None
    ));
}

#[test]
fn circuit_report_selection_rejects_pre_prompt_transcript_and_resume_redraw() {
    use crate::circuit::evaluator;
    let id = 910_019;
    let old = "Implementation complete.";
    let old_report = || {
        Some(crate::services::transcript_reader::AssistantReport {
            text: old.into(),
            revision: "turn-1".into(),
        })
    };
    evaluator::register_circuit(id);
    evaluator::on_output(id, old);
    evaluator::note_turn_start(id);
    // A gate just opened, but the agent has not answered the new prompt.
    assert_eq!(
        select_turn_report(
            old_report(),
            Some("turn-1"),
            evaluator::has_turn_start(id),
            || evaluator::cleaned_turn_tail(id)
        ),
        None
    );
    evaluator::on_output(id, "Permission required to run tests.");
    assert_eq!(
        select_turn_report(
            old_report(),
            Some("turn-1"),
            evaluator::has_turn_start(id),
            || evaluator::cleaned_turn_tail(id)
        ),
        Some("Permission required to run tests.".into())
    );
    evaluator::unregister(id);
    evaluator::register_circuit(id);
    evaluator::on_output(id, old); // resume redraw, not a new response
    assert_eq!(
        select_turn_report(
            old_report(),
            Some("turn-1"),
            evaluator::has_turn_start(id),
            || evaluator::cleaned_turn_tail(id)
        ),
        None
    );
    assert_eq!(
        select_turn_report(None, None, false, || "unanchored redraw".into()),
        None,
        "unreadable transcript cannot authorize a resume redraw"
    );
    assert_eq!(
        select_turn_report(
            Some(crate::services::transcript_reader::AssistantReport {
                text: "   \n".into(),
                revision: "turn-empty".into(),
            }),
            None,
            true,
            || "\t".into(),
        ),
        None,
        "empty reports are not classifier input",
    );
    // A concise new response made while capture was offline is recoverable.
    let report = select_turn_report(
        Some(crate::services::transcript_reader::AssistantReport {
            text: "Done. PR updated.".into(),
            revision: "turn-2".into(),
        }),
        Some("turn-1"),
        evaluator::has_turn_start(id),
        || evaluator::cleaned_turn_tail(id),
    )
    .unwrap();
    assert_eq!(report, "Done. PR updated.");
    assert!(should_classify_report(
        &report_gate_view(),
        "finish_classifier",
        SessionStatus::AwaitingInput,
        &report,
        None
    ));
    assert!(crate::circuit::evaluator::circuit_classify_prompt(&report).contains(&report));
    assert_eq!(
        select_turn_report(
            Some(crate::services::transcript_reader::AssistantReport {
                text: old.into(),
                revision: "turn-2".into()
            }),
            Some("turn-1"),
            false,
            String::new
        ),
        Some(old.into()),
        "identical text in a new assistant response is fresh"
    );
    evaluator::unregister(id);
}

// -- draft-first drive gate (issue #1356) ----------------------------------

#[test]
fn disabled_circuits_still_drive_manual_trigger_now_runs() {
    assert!(should_drive_circuit_run(true, "interval:1", false));
    assert!(should_drive_circuit_run(true, "manual:1", false));
    assert!(
        should_drive_circuit_run(false, "manual:1724000000000", false),
        "Trigger Now is the dry-run seam on a draft circuit"
    );
    assert!(
        !should_drive_circuit_run(false, "interval:1", false),
        "background interval runs stay parked while disabled"
    );
    assert!(!should_drive_circuit_run(
        false,
        "issue:42:buildmesh:run",
        false
    ));
    assert!(should_drive_circuit_run(
        false,
        "issue:42:buildmesh:run",
        true
    ));
}

// ------------------------------------------------------------------
// Circuit-run admission gate (issue #1467).
//
// These tests pin the pure gate helper [`may_admit_run`] in three
// shapes — empty mesh / under-cap mesh / full mesh — against a
// database installed for their own thread (`test_support::isolated`,
// issue #2048). The DB-layer contracts
// (`count_active_circuit_runs` / terminal commit) are pinned
// separately in `db/circuit_tests.rs`; here we verify the gate
// composes correctly with state transitions on real rows.
// ------------------------------------------------------------------

/// `running` and `paused` runs always pass the gate — they already
/// hold a slot from their `pending` admission.
#[test]
fn may_admit_run_running_and_paused_unconditional_pass() {
    let mesh = crate::models::Mesh {
        id: 9_999_001,
        circuit_run_capacity: 1,
        ..zero_test_mesh()
    };
    let running = active_row_with_state(9_999_001, 9_999_010, "running");
    assert!(may_admit_run(&running, &mesh));
    let paused = active_row_with_state(9_999_001, 9_999_011, "paused");
    assert!(may_admit_run(&paused, &mesh));
}

/// The defer path: a `pending` run on a mesh whose admitted-run
/// count equals the configured `circuit_run_capacity` returns
/// `false`. Initialized against a real temp DB so the test
/// exercises the production read path through
/// `db::count_active_circuit_runs` (no shadow helpers).
#[test]
fn may_admit_run_pending_saturated_mesh_defers() {
    let _db = install_temp_db();
    let mesh = crate::db::create_mesh("may-admit-defer", "/tmp/may-admit-defer").unwrap();
    // Two admitted runs saturate the expected review-flow capacity.
    crate::db::set_mesh_circuit_run_capacity(mesh.id, 2).unwrap();
    let c1 = crate::db::create_autopilot_circuit(
        mesh.id,
        "c1",
        "",
        &crate::circuit::model::CircuitGraph::walking_skeleton("fixture")
            .to_json()
            .unwrap(),
    )
    .unwrap();
    let c2 = crate::db::create_autopilot_circuit(
        mesh.id,
        "c2",
        "",
        &crate::circuit::model::CircuitGraph::walking_skeleton("fixture")
            .to_json()
            .unwrap(),
    )
    .unwrap();
    let c3 = crate::db::create_autopilot_circuit(
        mesh.id,
        "c3",
        "",
        &crate::circuit::model::CircuitGraph::walking_skeleton("fixture")
            .to_json()
            .unwrap(),
    )
    .unwrap();

    let mesh_row = crate::db::get_mesh_by_id(mesh.id).unwrap();

    // Mesh starts below cap and admits the first run.
    let pending_run = crate::db::create_circuit_run(c1.id, mesh.id, "", "{}").unwrap();
    let pending_row = crate::db::list_active_circuit_runs()
        .unwrap()
        .into_iter()
        .find(|r| r.run.id == pending_run)
        .expect("pending run should be in list_active_circuit_runs");
    assert!(
        may_admit_run(&pending_row, &mesh_row),
        "below cap — must admit",
    );

    // Admit the second run, then verify the third stays pending.
    crate::db::set_circuit_run_state(pending_run, "running").unwrap();
    let pending_run_2 = crate::db::create_circuit_run(c2.id, mesh.id, "", "{}").unwrap();
    let pending_row_2 = crate::db::list_active_circuit_runs()
        .unwrap()
        .into_iter()
        .find(|r| r.run.id == pending_run_2)
        .expect("second pending run must be in the active list");

    assert!(
        may_admit_run(&pending_row_2, &mesh_row),
        "the second run must admit below cap 2"
    );
    crate::db::set_circuit_run_state(pending_run_2, "running").unwrap();
    let pending_run_3 = crate::db::create_circuit_run(c3.id, mesh.id, "", "{}").unwrap();
    let pending_row_3 = crate::db::list_active_circuit_runs()
        .unwrap()
        .into_iter()
        .find(|r| r.run.id == pending_run_3)
        .expect("third pending run must be in the active list");

    assert_eq!(
        crate::db::count_active_circuit_runs(mesh.id).unwrap(),
        2,
        "two running runs consume both run-admission slots"
    );
    assert_eq!(
        mesh_row.circuit_run_capacity, 2,
        "sanity: cap is 2, so two admitted runs fill it"
    );
    assert!(
        !may_admit_run(&pending_row_3, &mesh_row),
        "at cap — must defer to next pass (FIFO)",
    );

    // Terminal the running run via `commit_circuit_advance`'s
    // idempotent terminal-state branch — the third pending run
    // now admits on the next observation pass.
    crate::db::commit_circuit_advance(pending_run, Some("completed"), None, &[]).unwrap();
    assert_eq!(
        crate::db::count_active_circuit_runs(mesh.id).unwrap(),
        1,
        "terminal commit frees one admitted-run slot"
    );
    assert!(
        may_admit_run(&pending_row_3, &mesh_row),
        "after terminal — third pending must admit (FIFO promotion)",
    );
}

/// Install this test's private database.
///
/// Used by the run-admission integration tests in this module. The caller
/// must bind the returned guard (`let _db = install_temp_db();`) for the rest
/// of the test: it is what keeps the database installed. This replaced a
/// process-global temp-file database (issue #2048), which also means there is
/// no file path to clean up and no separate preferences directory to seed.
fn install_temp_db() -> crate::db::test_support::IsolatedDbGuard {
    crate::db::test_support::isolated()
}

#[test]
fn circuit_archive_preserves_work_and_publishes_only_after_cleanup_receipt() {
    let _db = install_temp_db();
    let worktree = tempfile::tempdir().unwrap();
    let file = worktree.path().join("unfinished.txt");
    std::fs::write(&file, "uncommitted implementation").unwrap();
    let path = worktree.path().to_str().unwrap();
    let mesh = db::create_mesh("archive-recovery", path).unwrap();
    let node = db::create_agent_node(
        mesh.id,
        "Recover me",
        path,
        "saved-branch",
        crate::models::EnvType::Windows,
        "claude",
        None,
        None,
        None,
        None,
        true,
        None,
        None,
        None,
    )
    .unwrap();
    db::write_conn().execute("UPDATE agent_nodes SET worktree_path=?2, cli_session_id='saved-harness-session' WHERE id=?1", rusqlite::params![node.id, path]).unwrap();
    let circuit = db::create_autopilot_circuit(
        mesh.id,
        "recovery",
        "",
        &CircuitGraph::walking_skeleton("task").to_json().unwrap(),
    )
    .unwrap();
    let run_id = db::create_circuit_run(circuit.id, mesh.id, "manual:archive-test", "{}").unwrap();
    db::commit_circuit_advance(
        run_id,
        Some("running"),
        None,
        &[db::CircuitStepOp {
            node_id: "spawn".into(),
            status: "running".into(),
            attempt: 1,
            outcome: None,
            error: None,
            agent_node_id: Some(node.id),
            fresh_attempt: false,
        }],
    )
    .unwrap();
    // Archival is for helper agents: this one was launched for an implementer.
    {
        let writer = db::write_conn();
        writer
            .execute(
                "INSERT INTO agent_nodes (mesh_id, name, path) VALUES (?1, 'implementer', ?2)",
                rusqlite::params![mesh.id, path],
            )
            .unwrap();
        writer
            .execute(
                "UPDATE autopilot_circuit_run_steps SET parent_agent_node_id = last_insert_rowid() WHERE run_id = ?1",
                rusqlite::params![run_id],
            )
            .unwrap();
    }
    db::commit_circuit_advance(run_id, Some("failed"), None, &[]).unwrap();
    let calls = std::cell::Cell::new(0);
    let claim = db::claim_circuit_agent_cleanup(node.id).unwrap().unwrap();
    archive_failed_circuit_agent(node.id, &claim, |id, state| {
        calls.set(calls.get() + 1);
        assert_eq!((id, state.as_str()), (run_id, "failed"));
        assert_eq!(
            db::get_agent_node_by_id(node.id).unwrap().status,
            SessionStatus::Archived
        );
        assert!(!db::list_failed_circuit_agents_for_cleanup()
            .unwrap()
            .contains(&node.id));
    })
    .unwrap();
    assert_eq!(calls.get(), 1);
    let saved = db::get_agent_node_by_id(node.id).unwrap();
    assert_eq!(
        saved.cli_session_id.as_deref(),
        Some("saved-harness-session")
    );
    assert_eq!(saved.worktree_path.as_deref(), Some(path));
    assert_eq!(
        std::fs::read_to_string(file).unwrap(),
        "uncommitted implementation"
    );
    assert_eq!(
        db::list_circuit_run_steps(run_id).unwrap()[0].agent_node_id,
        Some(node.id)
    );
    db::update_agent_node_status(node.id, SessionStatus::Running).unwrap();
    assert!(!db::list_failed_circuit_agents_for_cleanup()
        .unwrap()
        .contains(&node.id));
    db::clear_finished_circuit_cleanup().unwrap();
    assert!(!db::get_circuit_run(run_id)
        .unwrap()
        .unwrap()
        .context_json
        .contains("cleanup.pending"));
}

/// The structural pin: `may_admit_run` short-circuits for `running`
/// and `paused` without touching the DB, so a no-DB-init unit test
/// can still verify the helper's contract for those branches.
/// (The `pending` branch reads the count; the integration path
/// through `run_pass` is what init's the DB, tested separately by
/// the `db::circuit_tests::count_active_circuit_runs_*` suite.)
#[test]
fn may_admit_run_signature_compiles_for_running_state() {
    let run = active_row_with_state(1, 2, "running");
    let mesh = zero_test_mesh();
    assert!(may_admit_run(&run, &mesh));
}

#[test]
fn global_agent_reservation_counts_occupied_pool_slots() {
    assert!(
        global_agent_reservation_fits(2, 0, 0, Some(2)),
        "an empty optional global pool should fit the requested lease"
    );
    // A peer run's reservation consumes the optional global pool before
    // its second process has been created.
    assert!(!global_agent_reservation_fits(2, 1, 0, Some(2)));
    assert!(!global_agent_reservation_fits(1, 0, 1, Some(1)));
    // Legacy (non-circuit) agents remain part of global accounting.
    assert!(!global_agent_reservation_fits(1, 0, 2, Some(2)));
}

#[test]
fn observed_capacity_ignores_legacy_mesh_node_cap() {
    let _db = install_temp_db();
    let mesh = crate::db::create_mesh("observe-capacity", "/tmp/observe-capacity").unwrap();
    crate::db::write_conn()
        .execute(
            "UPDATE meshes SET autopilot_concurrency_limit=1 WHERE id=?1",
            [mesh.id],
        )
        .unwrap();
    let circuit = crate::db::create_autopilot_circuit(
        mesh.id,
        "observe-capacity-circuit",
        "",
        &crate::circuit::model::CircuitGraph::walking_skeleton("fixture")
            .to_json()
            .unwrap(),
    )
    .unwrap();
    let run_id =
        crate::db::create_circuit_run(circuit.id, mesh.id, "manual:observe-capacity", "{}")
            .unwrap();
    crate::db::set_circuit_run_state(run_id, "running").unwrap();
    assert!(crate::db::reserve_circuit_agent_slots(run_id, 2).unwrap());

    let active = crate::db::list_active_circuit_runs()
        .unwrap()
        .into_iter()
        .find(|active| active.run.id == run_id)
        .expect("running test circuit should be observable");
    // This is the production capacity-observation seam used by
    // `observe`, exercised without a Tauri runtime or PTY. A two-slot
    // lease remains available even though the legacy mesh cap is one.
    let event = observe_capacity(&active, None);
    match event {
        CircuitEvent::Tick(capacity) => {
            assert_eq!(capacity.agent_free_slots, 2);
        }
        other => panic!("expected a capacity tick, got {other:?}"),
    }
}

/// ADR 0042 / issue #2114: sibling runs of one circuit used to time-share a
/// per-circuit step budget. On the live ledger two runs of a "Review agent"
/// circuit each held an `await_source` step, so the third admitted run's step
/// parked with "all 2 of this circuit's step slots are busy" while the mesh
/// allowed more runs. This drives three runs of that real graph through the
/// worker's own load, observe-capacity, advance and commit path against a real
/// database, and checks the ledger. A stale value in the retired
/// `concurrency_limit` column must be ignored.
#[test]
fn three_admitted_review_runs_of_one_circuit_all_start_their_first_step() {
    let _db = install_temp_db();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_str().unwrap();
    let mesh = db::create_mesh("sibling-review-runs", path).unwrap();
    let circuit = db::create_autopilot_circuit(
        mesh.id,
        "Review agent",
        "",
        &CircuitGraph::agent_review(None, None, 3).to_json().unwrap(),
    )
    .unwrap();
    db::write_conn()
        .execute(
            "UPDATE autopilot_circuits SET concurrency_limit = 2 WHERE id = ?1",
            [circuit.id],
        )
        .unwrap();

    let running_steps_on_circuit = || -> i64 {
        db::read_conn()
            .query_row(
                "SELECT COUNT(*) FROM autopilot_circuit_run_steps s \
                 JOIN autopilot_circuit_runs r ON r.id = s.run_id \
                 WHERE r.circuit_id = ?1 AND s.status = 'running'",
                [circuit.id],
                |row| row.get(0),
            )
            .unwrap()
    };
    let active_for = |run_id: i64| {
        db::list_active_circuit_runs()
            .unwrap()
            .into_iter()
            .find(|active| active.run.id == run_id)
            .expect("run should be active")
    };
    // The worker's commit: the transition's step writes plus any state flip.
    let commit = |run_id: i64, view: &RunView, transition: &crate::circuit::stepper::Transition| {
        let ops: Vec<db::CircuitStepOp> = transition
            .step_writes
            .iter()
            .map(|w| db::CircuitStepOp {
                node_id: w.node_id.clone(),
                status: w.status.as_db_str().to_string(),
                outcome: w.outcome.map(|o| o.map(|v| v.as_db_str().to_string())),
                error: w.error.clone(),
                agent_node_id: None,
                attempt: w.attempt,
                fresh_attempt: w.fresh_attempt,
            })
            .collect();
        let context = view.context.to_json().unwrap();
        db::commit_circuit_advance(
            run_id,
            transition
                .run_state_changed
                .then_some(view.state.as_db_str()),
            Some(&context),
            &ops,
        )
        .unwrap();
    };

    let mut run_ids = Vec::new();
    for n in 0..3 {
        let source = db::create_agent_node(
            mesh.id,
            &format!("Source {n}"),
            path,
            "work",
            crate::models::EnvType::Windows,
            "terminal",
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
        )
        .unwrap();
        let context = serde_json::json!({
            "source.agent_id": source.id.to_string(),
            "source.review_preset": "1",
        })
        .to_string();
        let run_id =
            db::create_circuit_run(circuit.id, mesh.id, &format!("manual:review-{n}"), &context)
                .unwrap();
        run_ids.push(run_id);
    }

    for (n, run_id) in run_ids.iter().copied().enumerate() {
        if n == 2 {
            assert_eq!(
                running_steps_on_circuit(),
                2,
                "two sibling runs already hold a running step: the old budget of 2 was saturated here"
            );
        }
        let active = active_for(run_id);
        let mut view = RunView {
            run_id,
            graph: CircuitGraph::from_json(&active.circuit_graph_json).unwrap(),
            state: RunState::from_db_str(&active.run.state),
            context: CircuitContext::from_json(&active.run.context_json).unwrap(),
            steps: load_steps(run_id).unwrap(),
        };
        let triggered = advance(&mut view, &CircuitEvent::Triggered);
        commit(run_id, &view, &triggered);
        // Admission reserves the blueprint's declared footprint (issue #1467).
        assert!(db::reserve_circuit_agent_slots(run_id, required_agent_slots(&active)).unwrap());
        let event = observe_capacity(&active_for(run_id), None);
        let ticked = advance(&mut view, &event);
        commit(run_id, &view, &ticked);
    }

    for run_id in run_ids {
        let steps = db::list_circuit_run_steps(run_id).unwrap();
        let await_source = steps
            .iter()
            .find(|step| step.node_id == "await_source")
            .unwrap_or_else(|| panic!("run {run_id} never reached await_source: {steps:?}"));
        assert_eq!(
            await_source.status, "running",
            "run {run_id} must progress, not park behind its siblings"
        );
    }
    assert_eq!(running_steps_on_circuit(), 3);
}

/// Test helper: an `ActiveCircuitRun` with only `mesh_id`, `id`,
/// and `state` populated — `may_admit_run` reads only those three
/// fields on the `pending`-vs-other branch, so the rest can stay
/// empty for the structural pin above.
fn active_row_with_state(mesh_id: i64, run_id: i64, state: &'static str) -> db::ActiveCircuitRun {
    db::ActiveCircuitRun {
        run: crate::models::AutopilotCircuitRun {
            id: run_id,
            circuit_id: 1,
            mesh_id,
            source_agent_node_id: None,
            trigger_identity: String::new(),
            state: state.to_string(),
            context_json: "{}".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        },
        circuit_enabled: true,
        circuit_graph_json: "{}".to_string(),
        circuit_name: String::new(),
    }
}

/// Test helper: a `Mesh` with empty placeholders for every field
/// except the one the test exercises. `may_admit_run` reads
/// `mesh_id` for log output and `circuit_run_capacity` for the
/// (caller-resolved) cap — the helper itself only receives the
/// mesh as a reference for the cache invalidation contract and the
/// future extension to read more fields.
fn zero_test_mesh() -> crate::models::Mesh {
    crate::models::Mesh {
        id: 0,
        name: String::new(),
        path: String::new(),
        layout: "grid".into(),
        position: 0,
        created_at: chrono::Utc::now(),
        build_command: None,
        run_command: None,
        model: None,
        effort: None,
        use_worktree: true,
        worktree_mode: None,
        default_provider: None,
        base_ref: "origin/main".into(),
        scratchpad: String::new(),
        sandbox: false,
        pre_spawn_pool_size: 0,
        color: None,

        root_build_command: None,
        root_run_command: None,

        circuit_run_capacity: 2,
        worktree_directory: None,
    }
}

#[test]
fn close_agent_retry_is_not_observed_after_close_clears_spawn_association() {
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
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        CircuitEvent::CloseAgentRetry { node_id } if node_id == "close_approved"
    ));

    // This is the in-memory half of the real CloseAgentNode effect. The
    // database half clears the same `reviewer` spawn step below it.
    let spawn_step_id = close_target_spawn_step_id(&view, "close_approved", Some("reviewer"), 701);
    assert_eq!(spawn_step_id, "reviewer");
    view.step_mut(&spawn_step_id).unwrap().agent_node_id = None;

    events.clear();
    observe_close_agent_retries(&view, &mut events);
    assert!(
        events.is_empty(),
        "a completed close must not be replayed after its target association is cleared"
    );
}

// -- startup reconciliation -------------------------------------------------

fn node_state(archived: bool, worktree_dir_exists: Option<bool>) -> ReconcileNodeState {
    ReconcileNodeState {
        archived,
        worktree_dir_exists,
    }
}

#[test]
fn a_running_spawn_step_without_an_attached_agent_is_the_commit_crash_gap() {
    // The one state observation can never repair: nothing in the
    // world maps to an event, so it must fail loudly at startup.
    assert_eq!(
        reconcile_spawn_step(None, None),
        SpawnReconciliation::NeverAttached
    );
    assert_eq!(
        reconcile_spawn_step(None, Some(node_state(false, Some(true)))),
        SpawnReconciliation::NeverAttached,
        "even a healthy-looking node row doesn't help — no attach ever landed"
    );
}

#[test]
fn a_missing_or_archived_piloted_agent_is_lost() {
    assert_eq!(
        reconcile_spawn_step(Some(7), None),
        SpawnReconciliation::Lost,
        "node row deleted while offline"
    );
    assert_eq!(
        reconcile_spawn_step(Some(7), Some(node_state(true, Some(true)))),
        SpawnReconciliation::Lost,
        "archived while offline"
    );
}

#[test]
fn startup_requires_a_resolved_target_before_confirming_lineage_loss() {
    assert_eq!(
        reconcile_lineage_target(None, |_| -> db::SqlResult<SessionStatus> {
            panic!("no agent lookup is valid when the step has no lineage")
        }),
        LineageReconciliation::Leave,
        "a step with no resolvable lineage has no agent whose absence can be confirmed"
    );
    assert_eq!(
        reconcile_lineage_target(Some(7), |_| Err(rusqlite::Error::QueryReturnedNoRows)),
        LineageReconciliation::Lost,
        "a looked-up missing row is lost"
    );
    assert_eq!(
        reconcile_lineage_target(Some(7), |_| Ok(SessionStatus::Archived)),
        LineageReconciliation::Lost,
    );
    assert_eq!(
        reconcile_lineage_target(Some(7), |_| Ok(SessionStatus::Running)),
        LineageReconciliation::Leave,
    );
    assert_eq!(
        reconcile_lineage_target(Some(7), |_| Err(rusqlite::Error::InvalidQuery)),
        LineageReconciliation::PreserveOnLookupError,
        "a failed database read is not confirmed loss"
    );
}

#[test]
fn borrowed_source_loss_persists_cancelled_step_and_its_checkpoint() {
    use crate::circuit::model::{CircuitGraph, CircuitNodeKind};
    use crate::circuit::stepper::StepView;

    let mut conn = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    conn.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');")
        .unwrap();
    let graph = CircuitGraph {
        version: CIRCUIT_GRAPH_VERSION,
        blueprint: None,
        nodes: vec![CircuitNode {
            id: "work".into(),
            kind: CircuitNodeKind::SpawnAgentNode {
                prompt: "work".into(),
                name: None,
                provider: None,
                model: None,
                effort: None,
                extra_args: None,
                timeout_seconds: None,
            },
        }],
        edges: vec![],
    };
    crate::db::circuit::ledger::create_autopilot_circuit_inner(
        &conn,
        1,
        "source-loss",
        "",
        &graph.to_json().unwrap(),
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state)
        VALUES(1,1,1,'running');",
    )
    .unwrap();
    let checkpoint = "Classifier unavailable after five attempts.";
    conn.execute(
        "INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id,error_message)
         VALUES(1,'work',1,'running',88,?1)",
        [checkpoint],
    ).unwrap();

    let mut view = RunView {
        run_id: 1,
        graph,
        state: RunState::Running,
        context: CircuitContext::default(),
        steps: vec![StepView {
            node_id: "work".into(),
            status: StepStatus::Running,
            outcome: None,
            error: Some(checkpoint.into()),
            agent_node_id: Some(88),
            attempt: 1,
        }],
    };
    persist_source_agent_loss(&mut view, 77, |view, transition| {
        persist_transition_checked_with(
            1,
            view,
            transition,
            |run_id, state, context, steps, evidence| {
                crate::db::circuit::evidence::commit_transition_locked(
                    &mut conn, run_id, state, context, steps, evidence,
                )
            },
        )
    })
    .expect("the source-loss transition should commit atomically");

    let (run_state, step_status, error): (String, String, String) = conn
        .query_row(
            "SELECT r.state,s.status,s.error_message FROM autopilot_circuit_runs r
         JOIN autopilot_circuit_run_steps s ON s.run_id=r.id WHERE r.id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (run_state.as_str(), step_status.as_str()),
        ("failed", "cancelled")
    );
    assert!(error.contains("Piloted agent node 77"));
    assert!(error.contains(checkpoint));
}

#[test]
fn a_vanished_worktree_directory_counts_as_lost_git_state() {
    assert_eq!(
        reconcile_spawn_step(Some(7), Some(node_state(false, Some(false)))),
        SpawnReconciliation::Lost,
        "resume would spawn into a nonexistent worktree"
    );
}

#[test]
fn recoverable_states_leave_the_step_alone() {
    assert_eq!(
        reconcile_spawn_step(Some(7), Some(node_state(false, Some(true)))),
        SpawnReconciliation::Leave,
        "intact worktree → auto-resume carries on"
    );
    assert_eq!(
        reconcile_spawn_step(Some(7), Some(node_state(false, None))),
        SpawnReconciliation::Leave,
        "root-repo spawn has no worktree to lose"
    );
    assert_eq!(
        reconcile_spawn_step(Some(7), Some(node_state(false, None))),
        SpawnReconciliation::Leave
    );
}

// -- orphan-detection regression (mesh 65 / runs 3, 5, 6) -----------------
//
// The legacy pass skipped non-SpawnAgentNode steps with a "Nothing to
// decide" comment. Live incident 2026-09-02 showed the assumption was
// wrong: when a piloted agent row is deleted while the run is in
// flight, an `InjectPty` / `LlmTurnClassifier` / `CloseAgentNode` /
// `SetNodeStatus` step that waits on it stays `running` forever,
// holding a `circuit_run_capacity` slot. The tests below pin the fix
// on both observation paths (per-tick `observe` and the one-shot
// `startup_reconcile_pass`).

/// Build a `RunView` whose `implementer` spawn step has the given
/// `agent_node_id` (None = unattached, mirroring a deleted row) and
/// whose `follow_feedback` step is `Running` with NULL `agent_node_id`
/// (the canonical shape of an orphaned inject step on the review
/// blueprint). Used by the orphan-detection regression tests below.
fn review_blueprint_view_with_orphan_inject(orphan_target_id: Option<i64>) -> RunView {
    RunView {
        run_id: 7,
        graph: CircuitGraph::issue_driven_autopilot_review("buildmesh:run"),
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![
            StepView {
                node_id: "trigger".into(),
                status: StepStatus::Completed,
                outcome: Some(GraphStepOutcome::Completed),
                error: None,
                agent_node_id: None,
                attempt: 1,
            },
            StepView {
                node_id: "implementer".into(),
                status: StepStatus::Completed,
                outcome: Some(GraphStepOutcome::Completed),
                error: None,
                agent_node_id: orphan_target_id,
                attempt: 1,
            },
            StepView {
                node_id: "follow_feedback".into(),
                status: StepStatus::Running,
                outcome: None,
                error: None,
                agent_node_id: None,
                attempt: 1,
            },
        ],
    }
}

#[test]
fn observed_agent_for_step_resolves_lineage_for_running_inject_step() {
    let view = review_blueprint_view_with_orphan_inject(Some(42));
    let follow = view
        .step("follow_feedback")
        .expect("fixture includes follow_feedback");
    assert_eq!(
        observed_agent_for_step(
            follow,
            &view.graph,
            &view.steps,
            view.context.source_agent_id()
        ),
        Some(42),
        "InjectPty lineage walks back to implementer's agent_node_id"
    );
}

#[test]
fn observed_agent_for_step_returns_none_when_lineage_spawn_has_no_agent() {
    let view = review_blueprint_view_with_orphan_inject(None);
    let follow = view.step("follow_feedback").unwrap();
    assert_eq!(
        observed_agent_for_step(
            follow,
            &view.graph,
            &view.steps,
            view.context.source_agent_id()
        ),
        None,
        "no spawn attachment → no observed agent"
    );
}

#[test]
fn observed_agent_for_step_resolves_source_binding_from_run_context() {
    let graph = CircuitGraph {
        version: 1,
        blueprint: None,
        nodes: vec![CircuitNode {
            id: "inject".into(),
            kind: CircuitNodeKind::InjectPty {
                prompt: "follow-up".into(),
                target_node_id: Some("$source".into()),
            },
        }],
        edges: vec![],
    };
    let step = StepView {
        node_id: "inject".into(),
        status: StepStatus::Running,
        outcome: None,
        error: None,
        agent_node_id: None,
        attempt: 1,
    };
    let mut context = CircuitContext::new();
    context.set("source.agent_id", "77");
    assert_eq!(
        observed_agent_for_step(
            &step,
            &graph,
            std::slice::from_ref(&step),
            context.source_agent_id()
        ),
        Some(77),
        "borrowed source bindings must remain visible to lifecycle observation"
    );
}

#[test]
fn observed_agent_for_step_prefers_direct_id_when_present() {
    let graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
    let step = StepView {
        node_id: "reviewer".into(),
        status: StepStatus::Running,
        outcome: None,
        error: None,
        agent_node_id: Some(99),
        attempt: 1,
    };
    // Empty steps slice is fine here — the early-return on direct id
    // short-circuits before any lineage walk.
    assert_eq!(observed_agent_for_step(&step, &graph, &[], None), Some(99));
}

#[test]
fn observed_agent_for_step_returns_none_for_non_piloted_steps() {
    let graph = CircuitGraph::walking_skeleton("{{issue.prefill}}");
    graph.validate().unwrap();
    let step = StepView {
        node_id: graph.nodes[0].id.clone(),
        status: StepStatus::Running,
        outcome: None,
        error: None,
        agent_node_id: None,
        attempt: 1,
    };
    assert_eq!(observed_agent_for_step(&step, &graph, &[], None), None);
}

#[test]
fn observed_agent_for_step_ignores_open_pr_worktree_observation() {
    let view = open_pr_run();
    let open_pr = view.step("open_pr").unwrap();
    assert_eq!(
        observed_agent_for_step(
            open_pr,
            &view.graph,
            &view.steps,
            view.context.source_agent_id()
        ),
        None,
        "OpenPr inspects a worktree but never observes or pilots the agent process"
    );
}

#[test]
fn observed_agent_for_step_walks_lineage_when_target_node_id_is_none() {
    // Regression for review feedback — the helper previously short-
    // circuited on `target_node_id = None` because the `?` on the
    // `Option<&str>` match arms extracted the inner `&str` instead of
    // passing the option through. The default AST representation for
    // any step relying on upstream BFS lineage uses `None`, so this
    // case is the COMMON one — every non-explicit target resolves via
    // BFS.
    use crate::circuit::model::{CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind};
    let graph = CircuitGraph {
        version: 1,
        blueprint: None,
        nodes: vec![
            CircuitNode {
                id: "trigger".into(),
                kind: CircuitNodeKind::Manual,
            },
            CircuitNode {
                id: "spawn".into(),
                kind: CircuitNodeKind::SpawnAgentNode {
                    prompt: "fix it".into(),
                    name: None,
                    provider: None,
                    model: None,
                    effort: None,
                    extra_args: None,
                    timeout_seconds: None,
                },
            },
            CircuitNode {
                id: "inject".into(),
                kind: CircuitNodeKind::InjectPty {
                    prompt: "follow-up".into(),
                    target_node_id: None,
                },
            },
        ],
        edges: vec![
            CircuitEdge {
                from: "trigger".into(),
                to: "spawn".into(),
                condition: Default::default(),
            },
            CircuitEdge {
                from: "spawn".into(),
                to: "inject".into(),
                condition: Default::default(),
            },
        ],
    };
    let steps = vec![
        StepView {
            node_id: "trigger".into(),
            status: StepStatus::Completed,
            outcome: Some(GraphStepOutcome::Completed),
            error: None,
            agent_node_id: None,
            attempt: 1,
        },
        StepView {
            node_id: "spawn".into(),
            status: StepStatus::Completed,
            outcome: Some(GraphStepOutcome::Completed),
            error: None,
            agent_node_id: Some(42),
            attempt: 1,
        },
        StepView {
            node_id: "inject".into(),
            status: StepStatus::Running,
            outcome: None,
            error: None,
            agent_node_id: None,
            attempt: 1,
        },
    ];
    let inject = steps.iter().find(|s| s.node_id == "inject").unwrap();
    assert_eq!(
        observed_agent_for_step(inject, &graph, &steps, None),
        Some(42),
        "InjectPty with target_node_id=None must walk upstream BFS, not short-circuit"
    );
}

#[test]
fn observed_agent_for_step_returns_none_for_healthy_step_with_no_lineage() {
    // Defensive: a Running step whose kind has no lineage arm AND
    // whose own `agent_node_id` is None (e.g. a `Notify` step)
    // must return None — startup_reconcile's `_` arm then leaves it
    // alone rather than falsely cancelling.
    use crate::circuit::model::{CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind};
    let graph = CircuitGraph {
        version: 1,
        blueprint: None,
        nodes: vec![
            CircuitNode {
                id: "trigger".into(),
                kind: CircuitNodeKind::Manual,
            },
            CircuitNode {
                id: "notify".into(),
                kind: CircuitNodeKind::Notify {
                    message: "done".into(),
                },
            },
        ],
        edges: vec![CircuitEdge {
            from: "trigger".into(),
            to: "notify".into(),
            condition: Default::default(),
        }],
    };
    let steps = vec![StepView {
        node_id: "notify".into(),
        status: StepStatus::Running,
        outcome: None,
        error: None,
        agent_node_id: None,
        attempt: 1,
    }];
    let notify = steps.first().unwrap();
    assert_eq!(
        observed_agent_for_step(notify, &graph, &steps, None),
        None,
        "Notify has no lineage arm → no agent to check → helper returns None"
    );
}

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
fn active_run(id: i64) -> db::ActiveCircuitRun {
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

// -- worker panic isolation (issue #1235) -----------------------------
//
// Headline regression for the circuits worker: a single panic inside
// `run_pass` (e.g. a serde edge case on a freshly-loaded graph)
// unwinds the worker thread and the circuits stop advancing forever.
// The fix wraps each per-pass body in `run_worker_pass`. The test
// below drives the same call shape as `start_circuit_worker`'s loop
// body and asserts the second + third tick still run after the
// first panicked.

#[test]
fn circuit_worker_loop_survives_a_panicking_drive_pass() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let successes = AtomicUsize::new(0);
    for tick in 0..3 {
        let successes_ref = &successes;
        crate::process_util::run_worker_pass("circuits:drive", move || {
            if tick == 0 {
                panic!("fault injection: first drive pass panics");
            }
            successes_ref.fetch_add(1, Ordering::SeqCst);
        });
    }
    assert_eq!(
        successes.load(Ordering::SeqCst),
        2,
        "ticks 1 + 2 must succeed after tick 0 panicked"
    );
}

fn open_pr_run() -> RunView {
    let graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
    let steps = graph
        .nodes
        .iter()
        .map(|node| StepView {
            node_id: node.id.clone(),
            status: if node.id == "open_pr" {
                StepStatus::Running
            } else {
                StepStatus::Completed
            },
            outcome: None,
            error: None,
            agent_node_id: if node.id == "implementer" {
                Some(700)
            } else {
                None
            },
            attempt: 1,
        })
        .collect();
    RunView {
        run_id: 42,
        graph,
        state: RunState::Running,
        context: CircuitContext::new(),
        steps,
    }
}

fn pushed_implementation(agent: i64) -> Result<crate::circuit::verification::WrapupState, String> {
    assert_eq!(agent, 700);
    Ok(crate::circuit::verification::WrapupState {
        dirty: false,
        pushed: true,
        branch: Some("renamed-implementation".into()),
        pr_url: None,
        pr_number: None,
        pr_required: false,
        repo_error: None,
    })
}

fn implementation_pr() -> crate::services::github::PullRequest {
    serde_json::from_value(serde_json::json!({
        "number": 314, "html_url": "https://github.com/example/repo/pull/314",
        "title": "Implementation", "head": { "ref": "renamed-implementation" }
    }))
    .unwrap()
}

fn set_open_pr_policy(run: &mut RunView, policy: Option<crate::circuit::model::OpenPrPolicy>) {
    if let Some(node) = run.graph.nodes.iter_mut().find(|node| node.id == "open_pr") {
        if let CircuitNodeKind::GithubAction { open_pr_policy, .. } = &mut node.kind {
            *open_pr_policy = policy;
        }
    }
}

#[test]
fn open_pr_acknowledges_existing_pr_and_publishes_review_context() {
    let mut run = open_pr_run();
    let event = github::ensure_open_pr(
        &run,
        "open_pr",
        Some(crate::circuit::model::OpenPrPolicy::RequireExisting),
        pushed_implementation,
        |head| {
            assert_eq!(head, "renamed-implementation");
            Ok(Some(implementation_pr()))
        },
        |_, _| panic!("existing PR must not be created again"),
    )
    .unwrap();
    let transition = advance(&mut run, &event);
    assert!(transition
        .step_writes
        .iter()
        .any(|write| write.node_id == "open_pr" && write.status == StepStatus::Completed));
    assert_eq!(run.context.get("pr.number"), Some("314"));
    assert_eq!(
        run.context.get("pr.url"),
        Some("https://github.com/example/repo/pull/314")
    );
    assert_eq!(
        run.context.get("pr.head_ref"),
        Some("renamed-implementation")
    );
}

#[test]
fn open_pr_target_is_recorded_before_lookup_or_create() {
    let run = open_pr_run();
    let calls = std::cell::RefCell::new(Vec::new());
    let event = github::ensure_open_pr_with_target(
        &run,
        "open_pr",
        None,
        pushed_implementation,
        |head| {
            assert_eq!(head, "renamed-implementation");
            calls.borrow_mut().push("target");
            Ok(())
        },
        |head| {
            assert_eq!(head, "renamed-implementation");
            assert_eq!(&*calls.borrow(), &["target"]);
            calls.borrow_mut().push("find");
            Ok(None)
        },
        |head, title| {
            assert_eq!(head, "renamed-implementation");
            assert_eq!(title, "Circuit run");
            assert_eq!(&*calls.borrow(), &["target", "find"]);
            calls.borrow_mut().push("create");
            Ok(implementation_pr())
        },
    )
    .unwrap();
    assert_eq!(&*calls.borrow(), &["target", "find", "create"]);
    assert!(matches!(
        event,
        CircuitEvent::GithubActionResult {
            success: true,
            pr_number: Some(314),
            ..
        }
    ));
}

#[test]
fn open_pr_review_requires_agent_pr_without_creating_one() {
    let error = github::ensure_open_pr(
        &open_pr_run(),
        "open_pr",
        Some(crate::circuit::model::OpenPrPolicy::RequireExisting),
        pushed_implementation,
        |_| Ok(None),
        |_, _| panic!("review blueprint delegates creation to its agent"),
    )
    .unwrap_err();
    assert!(error.contains("did not raise an open pull request"));
}

#[test]
fn open_pr_lookup_error_does_not_create_or_report_missing_pr() {
    let mut run = open_pr_run();
    set_open_pr_policy(&mut run, None);
    let error = github::ensure_open_pr(
        &run,
        "open_pr",
        None,
        pushed_implementation,
        |_| Err("GitHub unavailable".into()),
        |_, _| panic!("unknown is not absent"),
    )
    .unwrap_err();
    assert_eq!(error, "GitHub unavailable");
}

#[test]
fn open_pr_replay_after_creation_discovers_pr_without_duplicate() {
    let mut run = open_pr_run();
    set_open_pr_policy(&mut run, None);
    let created = std::cell::RefCell::new(None);
    let creates = std::cell::Cell::new(0);
    // Simulate losing the first result before its ledger commit: replay
    // the same Running snapshot against the now-existing remote PR.
    for _ in 0..2 {
        let event = github::ensure_open_pr(
            &run,
            "open_pr",
            None,
            pushed_implementation,
            |_| Ok(created.borrow().clone()),
            |head, _| {
                assert_eq!(head, "renamed-implementation");
                creates.set(creates.get() + 1);
                let pr = implementation_pr();
                let url = pr.clone();
                *created.borrow_mut() = Some(pr);
                Ok(url)
            },
        )
        .unwrap();
        assert!(matches!(
            event,
            CircuitEvent::GithubActionResult {
                success: true,
                pr_number: Some(314),
                ..
            }
        ));
    }
    assert_eq!(creates.get(), 1);
}

#[test]
fn open_pr_failed_git_observation_does_not_query_github() {
    let error = github::ensure_open_pr(
        &open_pr_run(),
        "open_pr",
        Some(crate::circuit::model::OpenPrPolicy::RequireExisting),
        |_| Err("worktree unavailable".into()),
        |_| panic!("branch identity is unknown"),
        |_, _| panic!("branch identity is unknown"),
    )
    .unwrap_err();
    assert_eq!(error, "worktree unavailable");
}

#[test]
fn determine_github_target_routes_correctly() {
    use crate::circuit::model::{
        CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, GithubActionKind,
    };
    use crate::circuit::stepper::{RunState, RunView};

    let graph = CircuitGraph {
        version: 1,
        blueprint: None,
        nodes: vec![
            CircuitNode {
                id: "t".into(),
                kind: CircuitNodeKind::Manual,
            },
            CircuitNode {
                id: "pre_label".into(),
                kind: CircuitNodeKind::GithubAction {
                    action: GithubActionKind::AddLabel,
                    open_pr_policy: None,
                    label: Some("in-progress".into()),
                    comment: None,
                },
            },
            CircuitNode {
                id: "open_pr".into(),
                kind: CircuitNodeKind::GithubAction {
                    action: GithubActionKind::OpenPr,
                    open_pr_policy: Some(crate::circuit::model::OpenPrPolicy::RequireExisting),
                    label: None,
                    comment: None,
                },
            },
            CircuitNode {
                id: "post_label".into(),
                kind: CircuitNodeKind::GithubAction {
                    action: GithubActionKind::AddLabel,
                    open_pr_policy: None,
                    label: Some("approved".into()),
                    comment: None,
                },
            },
        ],
        edges: vec![
            CircuitEdge {
                from: "t".into(),
                to: "pre_label".into(),
                condition: Default::default(),
            },
            CircuitEdge {
                from: "pre_label".into(),
                to: "open_pr".into(),
                condition: Default::default(),
            },
            CircuitEdge {
                from: "open_pr".into(),
                to: "post_label".into(),
                condition: Default::default(),
            },
        ],
    };

    let mut ctx = CircuitContext::new();
    ctx.set("issue.number", "42");
    ctx.set("pr.number", "1361");

    let view = RunView {
        run_id: 1,
        graph,
        state: RunState::Running,
        context: ctx,
        steps: vec![],
    };

    // Before OpenPr, AddLabel targets the issue (#42)
    let pre_target =
        determine_github_target(&view, "pre_label", GithubActionKind::AddLabel).unwrap();
    assert_eq!(pre_target, ("issue", 42));

    // After OpenPr, AddLabel targets the created PR (#1361)
    let post_target =
        determine_github_target(&view, "post_label", GithubActionKind::AddLabel).unwrap();
    assert_eq!(post_target, ("pr", 1361));

    // CloseIssue explicitly targets the issue (#42)
    let close_target =
        determine_github_target(&view, "post_label", GithubActionKind::CloseIssue).unwrap();
    assert_eq!(close_target, ("issue", 42));
}

// -----------------------------------------------------------------------
// `resolve_circuit_spawn_inputs` (issue #1358 / slice 3 of #1355)
//
// The pure seam that translates a `CircuitNodeKind::SpawnAgentNode` into
// the inputs the worker's impure wrapper threads into `create_pending`
// (for the `provider` column) and `SpawnRequest::with_explicit(...)`
// (for cascade layer-1 model/effort/extra_args). Mirrors the existing
// impure-wrapper / pure-core pattern this file uses for
// `reconcile_spawn_step` (line ~947) so the cascade + capability-mask
// contract can be tested without a Tauri runtime, AppHandle, or DB.
// -----------------------------------------------------------------------

fn spawn_kind(
    provider: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
    extra_args: Option<&str>,
    timeout_seconds: Option<u32>,
) -> CircuitNodeKind {
    CircuitNodeKind::SpawnAgentNode {
        prompt: "implement the fix".to_string(),
        name: Some("implementer".to_string()),
        provider: provider.map(str::to_string),
        model: model.map(str::to_string),
        effort: effort.map(str::to_string),
        extra_args: extra_args.map(str::to_string),
        timeout_seconds,
    }
}

#[test]
fn circuit_first_turn_prefills_when_the_harness_supports_it() {
    use crate::agent::launch::{initial_prompt_delivery, InitialPromptDelivery};
    assert_eq!(
        initial_prompt_delivery("claude", "implement the issue"),
        InitialPromptDelivery::Prefill
    );
    assert_eq!(
        initial_prompt_delivery("codex:custom-provider", "review the PR"),
        InitialPromptDelivery::Prefill
    );
}

#[test]
fn circuit_first_turn_falls_back_to_pty_injection_without_prefill() {
    use crate::agent::launch::{initial_prompt_delivery, InitialPromptDelivery};
    assert_eq!(
        initial_prompt_delivery("kimi", "implement the issue"),
        InitialPromptDelivery::InjectAfterSpawn
    );
    assert_eq!(
        initial_prompt_delivery("dsh", "review the PR"),
        InitialPromptDelivery::InjectAfterSpawn
    );
}

#[test]
fn circuit_empty_first_turn_stays_fresh() {
    use crate::agent::launch::{initial_prompt_delivery, InitialPromptDelivery};
    assert_eq!(
        initial_prompt_delivery("claude", "  \n\t"),
        InitialPromptDelivery::Fresh
    );
}

/// A node-authored `provider: Some("codex")` flows through into the
/// row column AND into the resolved Provider the capability mask uses.
/// Without this seam the worker would never honour a per-step
/// provider override.
#[test]
fn circuit_spawn_resolves_provider_override() {
    let kind = spawn_kind(Some("codex"), None, None, None, None);
    let resolved = resolve_circuit_spawn_inputs(&kind).expect("valid spawn");
    assert_eq!(resolved.provider_str.as_deref(), Some("codex"));
}

#[test]
fn review_provider_selection_preserves_explicit_and_inherits_parent() {
    assert_eq!(
        inherited_review_provider(None, Some("codex")),
        Some("codex".to_string())
    );
    assert_eq!(
        inherited_review_provider(Some("  claude  "), Some("codex")),
        Some("claude".to_string())
    );
    assert_eq!(
        inherited_review_provider(Some(" \t"), Some("  kimi ")),
        Some("kimi".to_string())
    );
    assert_eq!(inherited_review_provider(None, None), None);
}

#[test]
fn circuit_review_spawn_uses_frozen_plan_before_graph_parent_and_defaults() {
    let mut context = CircuitContext::new();
    context.set("source.review_preset", "1");
    context.set("source.provider", "claude");
    context.set("review.provider", "kimi");
    let mut prefs = crate::preferences::AppPreferences::default();
    prefs.harness_defaults.insert(
        "codex".into(),
        crate::preferences::HarnessConfigValue {
            model: Some("gpt-6-luna".into()),
            effort: Some("low".into()),
        },
    );
    let plan =
        crate::preferences::launch_configurations::capture(&prefs, "codex", &Default::default())
            .unwrap();
    let snapshot = crate::preferences::launch_configurations::snapshot(plan);
    context.set(
        "review.launch.reviewer",
        serde_json::to_string(&snapshot).unwrap(),
    );
    let view = RunView {
        run_id: 1,
        graph: CircuitGraph::agent_review(None, None, 2),
        state: RunState::Running,
        context,
        steps: vec![],
    };
    let (provider, explicit) = resolve_review_spawn_inputs(
        &view,
        "reviewer",
        Some("claude".into()),
        ExplicitSpawnOverrides {
            model: Some("mutable-model".into()),
            effort: Some("high".into()),
            extra_args: Some("mutable-args".into()),
            timeout_seconds: Some(90),
        },
        Some("opencode"),
    );
    assert_eq!(provider.as_deref(), Some(snapshot.id.as_str()));
    assert_eq!(explicit.model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(explicit.effort.as_deref(), Some("low"));
    assert_eq!(explicit.extra_args, None);
    assert_eq!(explicit.timeout_seconds, Some(90));
}

#[test]
fn review_spawn_cascade_applies_reviewer_precedence() {
    let mut context = CircuitContext::new();
    context.set("source.provider", "source-provider");
    context.set("source.model", "source-model");
    context.set("source.effort", "source-effort");
    let view = RunView {
        run_id: 1,
        graph: CircuitGraph::agent_review_with_provider(Some("graph-provider"), None, None, 2),
        state: RunState::Running,
        context,
        steps: vec![],
    };
    let (provider, explicit) = resolve_review_spawn_inputs(
        &view,
        "reviewer",
        Some("circuit-provider".into()),
        ExplicitSpawnOverrides {
            model: Some("circuit-model".into()),
            effort: Some("circuit-effort".into()),
            extra_args: None,
            timeout_seconds: None,
        },
        Some("parent-provider"),
    );
    assert_eq!(provider.as_deref(), Some("circuit-provider"));
    assert_eq!(explicit.model.as_deref(), Some("circuit-model"));
    assert_eq!(explicit.effort.as_deref(), Some("circuit-effort"));

    let mut preset_context = CircuitContext::new();
    preset_context.set("source.review_preset", "1");
    preset_context.set("source.provider", "source-provider");
    preset_context.set("source.model", "source-model");
    preset_context.set("source.effort", "source-effort");
    let preset_view = RunView {
        run_id: 2,
        graph: CircuitGraph::agent_review_with_provider(
            Some("stale-graph-provider"),
            None,
            None,
            2,
        ),
        state: RunState::Running,
        context: preset_context,
        steps: vec![],
    };
    let (provider, explicit) = resolve_review_spawn_inputs(
        &preset_view,
        "reviewer",
        Some("stale-graph-provider".into()),
        ExplicitSpawnOverrides::default(),
        Some("parent-provider"),
    );
    assert_eq!(provider.as_deref(), Some("source-provider"));
    assert_eq!(
        explicit.model, None,
        "preset metadata must not override the selected harness configuration"
    );
    assert_eq!(explicit.effort, None);

    let mut configured_context = CircuitContext::new();
    configured_context.set("source.review_preset", "1");
    configured_context.set("source.provider", "source-provider");
    configured_context.set("review.provider", "reviewer-provider");
    configured_context.set("source.model", "incompatible-source-model");
    configured_context.set("source.effort", "incompatible-source-effort");
    let configured_view = RunView {
        run_id: 3,
        graph: CircuitGraph::agent_review_with_provider(
            Some("stale-graph-provider"),
            None,
            None,
            2,
        ),
        state: RunState::Running,
        context: configured_context,
        steps: vec![],
    };
    let (provider, explicit) = resolve_review_spawn_inputs(
        &configured_view,
        "reviewer",
        Some("stale-graph-provider".into()),
        ExplicitSpawnOverrides {
            model: Some("stale-preset-model".into()),
            effort: Some("stale-preset-effort".into()),
            ..Default::default()
        },
        Some("parent-provider"),
    );
    assert_eq!(provider.as_deref(), Some("reviewer-provider"));
    assert_eq!(explicit.model, None);
    assert_eq!(explicit.effort, None);
}

#[test]
fn step_parent_resolution_handles_review_graphs_and_source_fallback() {
    let review = CircuitGraph::agent_review(None, None, 2);
    let mut context = CircuitContext::new();
    context.set("source.review_preset", "1");
    let view = RunView {
        run_id: 1,
        graph: review,
        state: RunState::Running,
        context,
        steps: vec![],
    };
    assert!(is_review_spawn_step(&view, "reviewer"));
    assert_eq!(resolve_step_parent_agent_id(&view, "reviewer"), None);

    let mut source_context = CircuitContext::new();
    source_context.set("source.review_preset", "1");
    source_context.set("source.agent_id", "77");
    let source_view = RunView {
        run_id: 3,
        graph: CircuitGraph::agent_review(None, None, 2),
        state: RunState::Running,
        context: source_context,
        steps: vec![],
    };
    assert_eq!(
        resolve_step_parent_agent_id(&source_view, "reviewer"),
        Some(77)
    );

    let issue_graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
    let issue_steps = issue_graph
        .nodes
        .iter()
        .map(|node| StepView {
            node_id: node.id.clone(),
            status: StepStatus::Completed,
            outcome: None,
            error: None,
            agent_node_id: (node.id == "implementer").then_some(42),
            attempt: 1,
        })
        .collect();
    let issue_view = RunView {
        run_id: 4,
        graph: issue_graph,
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: issue_steps,
    };
    assert_eq!(
        resolve_step_parent_agent_id(&issue_view, "reviewer"),
        Some(42)
    );

    let ordinary = CircuitGraph::walking_skeleton("work");
    let plain = RunView {
        run_id: 2,
        graph: ordinary,
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![],
    };
    assert!(!is_review_spawn_step(&plain, "spawn"));
}

#[test]
fn issue_review_spawn_seam_persists_parent_and_inherits_provider() {
    let _db = install_temp_db();
    let mesh = db::create_mesh(
        "issue-review-parent-provider",
        "/tmp/issue-review-parent-provider",
    )
    .unwrap();
    let source = db::create_agent_node(
        mesh.id,
        "Implementation",
        &mesh.path,
        "main",
        crate::models::EnvType::Windows,
        "parent-provider",
        None,
        None,
        None,
        None,
        true,
        None,
        None,
        None,
    )
    .unwrap();
    let reviewer = db::create_agent_node(
        mesh.id,
        "Review",
        &mesh.path,
        "main",
        crate::models::EnvType::Windows,
        "placeholder",
        None,
        None,
        None,
        None,
        true,
        None,
        None,
        None,
    )
    .unwrap();
    let graph = CircuitGraph::issue_driven_autopilot_review("ready-for-agent");
    let circuit = db::create_autopilot_circuit(
        mesh.id,
        "issue-review-parent-provider",
        "",
        &graph.to_json().unwrap(),
    )
    .unwrap();
    let run_id =
        db::create_circuit_run(circuit.id, mesh.id, "issue:42:ready-for-agent", "{}").unwrap();
    db::commit_circuit_advance(
        run_id,
        Some("running"),
        None,
        &[
            db::CircuitStepOp {
                node_id: "implementer".into(),
                status: "completed".into(),
                outcome: Some(Some("completed".to_string())),
                error: None,
                agent_node_id: Some(source.id),
                attempt: 1,
                fresh_attempt: false,
            },
            db::CircuitStepOp {
                node_id: "reviewer".into(),
                status: "running".into(),
                outcome: None,
                error: None,
                agent_node_id: None,
                attempt: 1,
                fresh_attempt: false,
            },
        ],
    )
    .unwrap();

    let mut view = RunView {
        run_id,
        graph,
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![
            StepView {
                node_id: "implementer".into(),
                status: StepStatus::Completed,
                outcome: Some(StepOutcome::Completed),
                error: None,
                agent_node_id: Some(source.id),
                attempt: 1,
            },
            StepView {
                node_id: "reviewer".into(),
                status: StepStatus::Running,
                outcome: None,
                error: None,
                agent_node_id: None,
                attempt: 1,
            },
        ],
    };
    // Exercise the same production resolver used by spawn_step_agent:
    // parent discovery, conditional provider inheritance, and the
    // provider/model/effort cascade all happen inside this call.
    let ReviewSpawnResolution {
        parent_agent_node_id: parent_id,
        provider,
        explicit,
    } = resolve_review_spawn_configuration(
        &view,
        "reviewer",
        None,
        ExplicitSpawnOverrides::default(),
        Some("parent-provider"),
    );
    assert_eq!(parent_id, Some(source.id));
    assert_eq!(provider.as_deref(), Some("parent-provider"));
    assert!(explicit.model.is_none());
    assert!(explicit.effort.is_none());

    db::write_conn().execute("INSERT INTO circuit_effects(run_id,node_id,attempt,kind,state) VALUES (?1,'reviewer',1,'spawn','possible_dispatch')", [run_id]).unwrap();
    spawn::attach_spawned_agent(run_id, &mut view, "reviewer", reviewer.id, parent_id).unwrap();
    let persisted_parent = db::list_circuit_agent_ownerships()
        .unwrap()
        .into_iter()
        .find(|row| row.0 == reviewer.id && row.1 == run_id)
        .and_then(|row| row.5);
    assert_eq!(persisted_parent, Some(source.id));
}

/// The cascade layer-1 (explicit) override slot must carry the per-node
/// `model`. Empty / whitespace-only inputs collapse to absent so the
/// cascade falls through (issue #1148 AC #32).
#[test]
fn circuit_spawn_passes_model_through_explicit_override() {
    let kind = spawn_kind(Some("anthropic"), Some("opus-4-1"), None, None, None);
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert_eq!(resolved.explicit.model.as_deref(), Some("opus-4-1"));
    assert_eq!(resolved.explicit.effort, None);
}

/// `effort` and `extra_args` ride the same explicit slot — pin both so
/// the seam doesn't accidentally drop one of them on its way through.
#[test]
fn circuit_spawn_passes_effort_and_extra_args_through_explicit_override() {
    let kind = spawn_kind(
        Some("anthropic"),
        None,
        Some("high"),
        Some("--dangerously-skip-permissions"),
        None,
    );
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert_eq!(resolved.explicit.effort.as_deref(), Some("high"));
    assert_eq!(
        resolved.explicit.extra_args.as_deref(),
        Some("--dangerously-skip-permissions")
    );
}

/// Unknown provider strings stay as-is in the seam so the row
/// carries the user-authored value. The Anthropic fallback
/// (`Provider::from_db_str("...")` returning `Anthropic` for unknown
/// ids) runs downstream in `spawn_with_intent` against the row —
/// the seam does not parse it. Pin that contract so a future
/// refactor can't normalize the value too early and break the row's
/// author-visible identity.
#[test]
fn circuit_spawn_preserves_unknown_provider_string() {
    let kind = spawn_kind(Some("not-a-real-thing"), None, None, None, None);
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert_eq!(resolved.provider_str.as_deref(), Some("not-a-real-thing"));
}

/// Whitespace-only model / effort / extra_args collapse to absent so
/// the cascade falls through.
#[test]
fn circuit_spawn_whitespace_overrides_collapse_to_absent() {
    let kind = spawn_kind(None, Some("   "), Some("\t\n"), Some("   \t  "), None);
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert!(resolved.explicit.model.is_none());
    assert!(resolved.explicit.effort.is_none());
    assert!(resolved.explicit.extra_args.is_none());
}

/// `name` is pass-through (no cascade layer owns it).
#[test]
fn circuit_spawn_name_passes_through_unchanged() {
    let kind = spawn_kind(Some("claude_code"), None, None, None, None);
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert_eq!(resolved.name.as_deref(), Some("implementer"));
}

/// A non-SpawnAgentNode kind is a hard error — the seam narrows
/// before destructuring.
#[test]
fn circuit_spawn_rejects_non_spawn_kind() {
    let inject = CircuitNodeKind::InjectPty {
        prompt: "hi".into(),
        target_node_id: None,
    };
    let err = resolve_circuit_spawn_inputs(&inject).unwrap_err();
    assert!(
        err.contains("not a spawn"),
        "rejection message must name the kind: {err}"
    );
}

/// `provider: None` leaves the AST override empty. The worker then
/// resolves the effective provider through the shared explicit -> mesh ->
/// application default spawn chain before creating the
/// row, so this pure resolver remains free of database access.
#[test]
fn circuit_spawn_default_provider_is_none_when_unset() {
    let kind = spawn_kind(None, None, None, None, None);
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert!(
        resolved.provider_str.is_none(),
        "None -> resolve the mesh/application default at spawn time"
    );
}

/// #1219 review: a user-authored `timeout_seconds: Some(1800)` must
/// ride the explicit-override seam so the launch phase can log it (and
/// a future process-level watchdog can consume it without re-deriving
/// from the AST). The circuit-level watchdog reads the graph node
/// directly in `observe_waits`; this test pins the carrier behaviour
/// so the launch seam can trust `explicit.timeout_seconds`. The
/// inspector's contract is "0 or blank = inherit default".
#[test]
fn circuit_spawn_passes_timeout_seconds_through_explicit_override() {
    let kind = spawn_kind(Some("anthropic"), None, None, None, Some(1800));
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert_eq!(resolved.explicit.timeout_seconds, Some(1800));
}

/// #1219 review: `Some(0)` collapses to `None` at the seam (the
/// inspector's "0 = inherit default" affordance) so the cascade
/// falls through. Without this collapse a zero-int overflow at save
/// time could request an instant expiry once the watchdog slice
/// lands. Pin the contract here.
#[test]
fn circuit_spawn_zero_timeout_seconds_collapses_to_none() {
    let kind = spawn_kind(Some("anthropic"), None, None, None, Some(0));
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert!(
        resolved.explicit.timeout_seconds.is_none(),
        "Some(0) must collapse to None so the cascade falls through"
    );
}

/// #1219 review: `None` carries through unchanged — the inspector's
/// "blank" affordance is semantically identical to "inherit the
/// orchestrator default" and must not become `Some(0)` at the seam.
#[test]
fn circuit_spawn_none_timeout_seconds_stays_none() {
    let kind = spawn_kind(Some("anthropic"), None, None, None, None);
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert_eq!(resolved.explicit.timeout_seconds, None);
}

/// #1219 (round-2 review): the carrier seam must thread the value
/// end-to-end. `ResolvedCircuitSpawn` carries `explicit.timeout_seconds`
/// and is later flattened into `SpawnOptions::explicit_timeout_seconds`
/// by the orchestrator — this test asserts the carrier shape stays
/// populated so the launch phase can log the budget without
/// re-deriving from the AST. (The circuit watchdog itself reads the
/// graph node in `observe_waits`; see
/// `observe_waits_uses_spawn_step_timeout_seconds_for_wait_observed`.)
/// A future refactor that flattens the carrier without copying the
/// timeout would drop the wiring silently; the field pin in
/// `prepare_tests::spawn_options_carries_explicit_slots` catches that
/// at compile time, this test catches the runtime regression.
#[test]
fn circuit_spawn_carries_timeout_through_to_resolved_carrier() {
    let kind = spawn_kind(Some("anthropic"), None, None, None, Some(1800));
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert_eq!(
        resolved.explicit.timeout_seconds,
        Some(1800),
        "carrier must thread timeout end-to-end so the orchestrator can \
         pass it into SpawnOptions"
    );
}

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
    let agent = db::create_agent_node(
        mesh_id,
        name,
        path,
        "main",
        crate::models::EnvType::Windows,
        provider,
        None,
        None,
        None,
        None,
        true,
        None,
        None,
        None,
    )
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
