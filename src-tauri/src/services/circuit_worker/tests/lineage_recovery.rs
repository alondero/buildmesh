// Circuit worker lineage recovery tests. Included in the existing tests scope.
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
