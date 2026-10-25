// Circuit worker result support tests. Included in the existing tests scope.
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

// Exercise real DB admission, process input, handoff marker and file reading.
struct ResultReadFixture {
    _db: crate::db::test_support::IsolatedDbGuard,
    _directory: tempfile::TempDir,
    agent_id: i64,
    view: RunView,
    result: std::path::PathBuf,
}

impl ResultReadFixture {
    fn new() -> Self {
        use crate::circuit::observation::{ObservationIdentity, ReportEvidence, WorkEvidence};
        static NEXT_ID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-721_000);
        let db = install_temp_db();
        let directory = tempfile::tempdir().unwrap();
        crate::preferences::init_for_tests(directory.path().to_path_buf());
        let path = directory.path().to_str().unwrap();
        let mesh = db::create_mesh("result-read", path).unwrap();
        let old_id = db::create_agent_node(db::NewAgentNode {
            mesh_id: mesh.id,
            name: "result writer",
            path,
            branch: "main",
            env: crate::models::EnvType::Windows,
            provider: "cline",
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
        .unwrap()
        .id;
        let agent_id = NEXT_ID.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        db::write_conn().execute(
            "UPDATE agent_nodes SET id=?1,cli_session_id='result-session',session_started_at=1,status='ready' WHERE id=?2",
            rusqlite::params![agent_id, old_id],
        ).unwrap();
        crate::agent::process::testing::insert_standin(
            &crate::agent::process::PROCESS_REGISTRY,
            agent_id,
        );
        let input = crate::agent::process::PROCESS_REGISTRY
            .input_stamp_result(agent_id)
            .unwrap();
        let mut view = RunView {
            run_id: 721_000 - agent_id,
            graph: CircuitGraph::issue_driven_autopilot_review("ready"),
            state: RunState::Running,
            context: CircuitContext::new(),
            steps: vec![
                StepView {
                    node_id: "implementer".into(),
                    status: StepStatus::Completed,
                    agent_node_id: Some(agent_id),
                    attempt: 1,
                    outcome: None,
                    error: None,
                },
                StepView {
                    node_id: "implementation_classifier".into(),
                    status: StepStatus::Running,
                    agent_node_id: None,
                    attempt: 1,
                    outcome: None,
                    error: None,
                },
            ],
        };
        let evidence = WorkEvidence {
            identity: Some(ObservationIdentity {
                run_id: view.run_id,
                step_id: "implementer".into(),
                attempt: 1,
                agent_node_id: agent_id,
                session_incarnation: Some("1".into()),
                session_id: Some("result-session".into()),
                turn_id: Some("turn-1".into()),
                report_revision: Some("native:1".into()),
            }),
            report: Some(ReportEvidence {
                text: "Finished work".into(),
                revision: "native:1".into(),
                input_stamp: Some(input),
                observed_at_ms: 2,
            }),
            foreground_terminated: true,
            assignment_completed: true,
            ownership_covered: true,
            ..Default::default()
        };
        view.context.set(
            "node.implementer.evidence.1",
            serde_json::to_string(&evidence).unwrap(),
        );
        crate::circuit::handoff::set_agent_turn_in(
            directory.path(),
            view.run_id,
            agent_id,
            Some(&crate::circuit::handoff::HandoffTurn {
                node_id: "implementation_classifier".into(),
                attempt: 1,
            }),
        );
        let result = crate::circuit::handoff::paths_in(
            directory.path(),
            view.run_id,
            "implementation_classifier",
            1,
        )
        .result;
        Self {
            _db: db,
            _directory: directory,
            agent_id,
            view,
            result,
        }
    }

    fn probe(
        &mut self,
    ) -> (
        Vec<CircuitEvent>,
        Vec<crate::circuit::stepper::Effect>,
        usize,
    ) {
        // Simulate worker restart to permit a pull without wall-clock sleeps.
        // Retry counts must survive loss of the in-memory evaluator state.
        evaluator::unregister(self.agent_id);
        evaluator::register_circuit(self.agent_id);
        let mut source = super::observe_parity_tests::Script::default();
        source.live_run = Some(self.view.run_id);
        let mut events = Vec::new();
        super::turn_classify::observe_gates_with(&self.view, &mut events, &mut source);
        let mut effects = Vec::new();
        for event in &events {
            effects.extend(advance(&mut self.view, event).effects);
        }
        let attention = source
            .calls
            .iter()
            .filter(|call| **call == "blocked")
            .count();
        (events, effects, attention)
    }
}

impl Drop for ResultReadFixture {
    fn drop(&mut self) {
        crate::agent::process::PROCESS_REGISTRY.kill_session(self.agent_id);
        evaluator::unregister(self.agent_id);
        crate::preferences::reset_for_tests();
    }
}
