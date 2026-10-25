// Circuit worker result repair tests. Included in the existing tests scope.
#[test]
fn result_invalid_encoding_requests_agent_repair_through_production_classifier() {
    for bytes in [
        vec![0xff, 0xfe, b'O', 0, b'K', 0], // Windows PowerShell Out-File / >
        vec![b'O', b'K', b' ', 0x97],       // Windows-1252 Set-Content
    ] {
        let mut fixture = ResultReadFixture::new();
        std::fs::write(&fixture.result, bytes).unwrap();
        let (events, effects, attention) = fixture.probe();
        assert!(events
            .iter()
            .any(|event| matches!(event, CircuitEvent::ResultFileMissing { .. })));
        assert_eq!(attention, 0);
        assert_eq!(effects.len(), 1);
        let crate::circuit::stepper::Effect::ContinueAgentTurn { prompt, .. } = &effects[0] else {
            panic!("invalid encoding must request an agent repair");
        };
        assert!(prompt.contains("UTF-8"));
        assert_eq!(
            fixture
                .view
                .context
                .get("node.implementation_classifier.result_reminders.1"),
            Some("1")
        );
        // Each delivered reminder gets a fresh finished turn, then the same
        // encoding error must exhaust the shared two-reminder budget.
        for turn in 2..=3 {
            advance(
                &mut fixture.view,
                &CircuitEvent::ContinuationDelivered {
                    node_id: "implementation_classifier".into(),
                    attempt: 1,
                },
            );
            db::write_conn()
                .execute(
                    "UPDATE agent_nodes SET status_changed_at=?2 WHERE id=?1",
                    rusqlite::params![fixture.agent_id, format!("turn-{turn}")],
                )
                .unwrap();
            let (events, effects, attention) = fixture.probe();
            assert!(matches!(
                events.as_slice(),
                [CircuitEvent::ResultFileMissing { .. }]
            ));
            assert_eq!(effects.len(), usize::from(turn == 2));
            assert_eq!(attention, usize::from(turn == 3));
        }
        assert_eq!(fixture.view.steps[1].status, StepStatus::Unverified);
        let (_, effects, attention) = fixture.probe();
        assert!(effects.is_empty());
        assert_eq!(attention, 0);
        std::fs::write(
            &fixture.result,
            "\u{feff}Repaired result\nBUILDMESH_HANDOFF_V1: READY\n",
        )
        .unwrap();
        fixture.probe();
        assert_eq!(fixture.view.steps[1].status, StepStatus::Completed);
    }
}

#[test]
fn result_read_error_then_invalid_encoding_can_request_repair_while_unverified() {
    let mut fixture = ResultReadFixture::new();
    std::fs::create_dir(&fixture.result).unwrap();
    fixture.probe();
    assert_eq!(fixture.view.steps[1].status, StepStatus::Unverified);
    std::fs::remove_dir(&fixture.result).unwrap();
    std::fs::write(&fixture.result, [0x97]).unwrap();
    let (_, effects, attention) = fixture.probe();
    assert_eq!(effects.len(), 1);
    assert_eq!(attention, 0);
    assert_eq!(fixture.view.steps[1].status, StepStatus::Running);
}

#[test]
fn result_read_error_count_is_attempt_scoped_and_rejects_stale_or_paused_events() {
    let mut fixture = ResultReadFixture::new();
    std::fs::create_dir(&fixture.result).unwrap();
    let (mut events, _, _) = fixture.probe();
    let stale = events.remove(0);
    fixture.view.steps[1].attempt = 2;
    let before = fixture.view.context.to_json().unwrap();
    assert!(!advance(&mut fixture.view, &stale).context_changed);
    assert_eq!(before, fixture.view.context.to_json().unwrap());
    let (events, effects, attention) = fixture.probe();
    assert_eq!(events.len(), 1);
    assert!(effects.is_empty());
    assert_eq!(attention, 0);
    assert_eq!(
        fixture
            .view
            .context
            .get("node.implementation_classifier.result_read_failures.1"),
        Some("1")
    );
    assert_eq!(
        fixture
            .view
            .context
            .get("node.implementation_classifier.result_read_failures.2"),
        Some("1")
    );
    fixture.view.state = RunState::Paused;
    let before = fixture.view.context.to_json().unwrap();
    assert!(!advance(&mut fixture.view, &events[0]).context_changed);
    assert_eq!(before, fixture.view.context.to_json().unwrap());
    assert!(classify_step_turn(
        &active_run(fixture.view.run_id),
        &fixture.view,
        "implementation_classifier"
    )
    .is_none());
}

#[test]
fn result_persistent_read_error_is_bounded_and_recovers_through_production_classifier() {
    let mut fixture = ResultReadFixture::new();
    std::fs::create_dir(&fixture.result).unwrap();
    for probe in 1..=6 {
        let (events, effects, attention) = fixture.probe();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            CircuitEvent::ObservationDeferred { .. }
        ));
        assert!(
            effects.is_empty(),
            "I/O failure must never send an agent prompt"
        );
        assert_eq!(
            attention,
            usize::from(probe == 3),
            "attention once at the bound"
        );
        assert_eq!(fixture.view.steps[1].status, StepStatus::Unverified);
        assert!(fixture.view.steps[1]
            .error
            .as_ref()
            .unwrap()
            .contains(&fixture.result.display().to_string()));
        assert_eq!(
            fixture
                .view
                .context
                .get("node.implementation_classifier.result_reminders.1"),
            None
        );
        fixture.view.context =
            CircuitContext::from_json(&fixture.view.context.to_json().unwrap()).unwrap();
    }
    std::fs::remove_dir(&fixture.result).unwrap();
    std::fs::write(
        &fixture.result,
        "Repaired result\nBUILDMESH_HANDOFF_V1: READY\n",
    )
    .unwrap();
    let (events, _, attention) = fixture.probe();
    assert_eq!(attention, 0);
    assert!(events.iter().any(|event| matches!(
        event,
        CircuitEvent::TurnClassified {
            classification: Some(evaluator::Classification::Completed),
            ..
        }
    )));
    assert_eq!(fixture.view.steps[1].status, StepStatus::Completed);
}

#[test]
fn circuit_archive_preserves_work_and_publishes_only_after_cleanup_receipt() {
    let _db = install_temp_db();
    let worktree = tempfile::tempdir().unwrap();
    let file = worktree.path().join("unfinished.txt");
    std::fs::write(&file, "uncommitted implementation").unwrap();
    let path = worktree.path().to_str().unwrap();
    let mesh = db::create_mesh("archive-recovery", path).unwrap();
    let node = db::create_agent_node(db::NewAgentNode {
        mesh_id: mesh.id,
        name: "Recover me",
        path,
        branch: "saved-branch",
        env: crate::models::EnvType::Windows,
        provider: "claude",
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
