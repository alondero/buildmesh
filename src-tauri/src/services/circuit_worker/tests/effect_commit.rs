// Circuit worker effect commit tests. Included in the existing tests scope.
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
        Effect::NudgeIdleAgent {
            node_id: "status".into(),
            target_agent_id: 101,
            prompt: "wake".into(),
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
