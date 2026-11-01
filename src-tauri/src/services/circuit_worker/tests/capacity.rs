// Circuit worker capacity tests. Included in the existing tests scope.
#[test]
fn pending_retained_reviewer_transfers_into_its_lease_without_overadmitting_a_sibling() {
    for existing_pending_lease in [false, true] {
        let _db = install_temp_db();
        {
            let conn = db::write_conn();
            conn.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'retained','/repo');
                INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(9,1,'source','/repo','ready'),(10,1,'reviewer','/review','ready');
                INSERT INTO autopilot_circuits(id,mesh_id,name,graph_json) VALUES(1,1,'review','{}');
                INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,source_agent_node_id,trigger_identity) VALUES(1,1,1,'pending',9,'manual:extension'),(2,1,1,'pending',NULL,'manual:sibling');
                INSERT INTO autopilot_circuit_run_steps(run_id,node_id,status,agent_node_id,parent_agent_node_id) VALUES(1,'reviewer','completed',10,9);").unwrap();
            conn.execute(
                "UPDATE autopilot_circuits SET graph_json=?1",
                [CircuitGraph::agent_review(None, None, 6).to_json().unwrap()],
            )
            .unwrap();
        }
        if existing_pending_lease {
            assert!(db::reserve_circuit_agent_slots(1, 1).unwrap());
        }
        let runs = db::list_active_circuit_runs().unwrap();
        let mut reserved = db::count_reserved_circuit_agent_slots_total().unwrap();
        assert_eq!(db::count_retained_circuit_agent_nodes_total().unwrap(), 1);
        let mut unleased = db::count_unleased_circuit_agent_nodes_total().unwrap();
        assert_eq!(
            (reserved, unleased),
            if existing_pending_lease {
                (1, 0)
            } else {
                (0, 1)
            }
        );
        assert!(reserve_pending_agents(
            runs.iter().find(|run| run.run.id == 1).unwrap(),
            &mut reserved,
            &mut unleased,
            Some(1)
        ));
        assert_eq!((reserved, unleased), (1, 0));
        assert_eq!(db::circuit_agent_slots_reserved(1).unwrap(), 1);
        db::set_circuit_run_state(1, "running").unwrap();
        assert_eq!(db::count_reserved_circuit_agent_slots_total().unwrap(), 1);
        assert_eq!(db::count_retained_circuit_agent_nodes_total().unwrap(), 0);
        assert!(!reserve_pending_agents(
            runs.iter().find(|run| run.run.id == 2).unwrap(),
            &mut reserved,
            &mut unleased,
            Some(1)
        ));
        assert_eq!(db::circuit_agent_slots_reserved(2).unwrap(), 0);
        assert_eq!(db::get_circuit_run(2).unwrap().unwrap().state, "pending");
    }
}

#[test]
fn a_pool_reduction_still_counts_a_leased_pending_reviewer_at_tick() {
    use crate::circuit::stepper::Capacity;
    let _db = install_temp_db();
    {
        let conn = db::write_conn();
        conn.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'pool-reduction','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(10,1,'pending-reviewer','/review','ready'),(11,1,'active','/active','ready');
            INSERT INTO autopilot_circuits(id,mesh_id,name,graph_json) VALUES(1,1,'review','{}');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,trigger_identity) VALUES(1,1,1,'pending','manual:pending'),(2,1,1,'running','manual:active');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,status,agent_node_id) VALUES(1,'reviewer','completed',10),(2,'reviewer','running',11);").unwrap();
        conn.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [CircuitGraph::agent_review(None, None, 6).to_json().unwrap()],
        )
        .unwrap();
    }
    assert!(db::reserve_circuit_agent_slots(1, 1).unwrap());
    assert!(db::reserve_circuit_agent_slots(2, 2).unwrap());
    let active = db::list_active_circuit_runs()
        .unwrap()
        .into_iter()
        .find(|run| run.run.id == 2)
        .unwrap();
    assert_eq!(db::count_active_circuit_agent_nodes_total().unwrap(), 1);
    assert_eq!(db::count_retained_circuit_agent_nodes_total().unwrap(), 1);
    assert_eq!(db::count_unleased_circuit_agent_nodes_total().unwrap(), 0);
    assert!(matches!(
        observe_capacity(&active, Some(3)),
        CircuitEvent::Tick(Capacity {
            agent_free_slots: 1
        })
    ));
    assert!(matches!(
        observe_capacity(&active, Some(2)),
        CircuitEvent::Tick(Capacity {
            agent_free_slots: 0
        })
    ));
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
            .unwrap()
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
        let source = db::create_agent_node(db::NewAgentNode {
            mesh_id: mesh.id,
            name: &format!("Source {n}"),
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
            attention_dismissed_at: None,
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
