// Circuit worker terminal cleanup tests. Included in the existing tests scope.
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
    assert!(dispatch::effect_allowed_in_state(
        "completed",
        true,
        &notify
    ));
    assert!(dispatch::effect_allowed_in_state(
        "completed",
        true,
        &set_status
    ));
    assert!(dispatch::effect_allowed_in_state("completed", true, &close));
    assert!(!dispatch::effect_allowed_in_state(
        "completed",
        true,
        &spawn
    ));
    assert!(!dispatch::effect_allowed_in_state(
        "completed",
        true,
        &inject
    ));
    for effect in [notify, set_status, close, spawn, inject] {
        assert!(!dispatch::effect_allowed_in_state(
            "completed",
            false,
            &effect
        ));
        assert!(!dispatch::effect_allowed_in_state(
            "cancelled",
            true,
            &effect
        ));
        assert_eq!(
            dispatch::effect_allowed_in_state("failed", true, &effect),
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
