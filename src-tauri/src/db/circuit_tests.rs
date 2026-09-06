//! Autopilot Circuits ledger tests (spec #1205 / walking skeleton #1206).
//!
//! Exercises the three v34 tables through the `db::circuit` accessors
//! against temp-dir SQLite files (the `mesh_tests` pattern) and pins the
//! v33 → v34 migration (fresh table creation via the new `AlwaysStep`).

use super::*;
use crate::autopilot::circuit::model::CircuitGraph;
use rusqlite::Connection;

/// Temp-dir DB init, serialised by unique filename (`mesh_tests`
/// pattern). Tests in this file share the process-global DB, so they
/// must run with `--test-threads=1`; CI's cargo invocation already pins
/// that for the db test modules.
fn init_temp_db(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "buildmesh_circuit_test_{}_{}.db",
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    init(&path).unwrap();
    path
}

fn sample_graph_json() -> String {
    CircuitGraph::walking_skeleton("fix the flaky test")
        .to_json()
        .unwrap()
}

#[test]
fn node_review_borrows_source_deduplicates_and_cancels_only_reviewer() {
    init_temp_db("node-review");
    let mesh = create_mesh("node-review-mesh", "/tmp/node-review").unwrap();
    let source = create_agent_node(mesh.id, "Fix parser", &mesh.path, "pr-head", EnvType::Windows,
        "claude", None, None, None, None, true, None, None, None).unwrap();
    update_agent_node_status(source.id, SessionStatus::Ready).unwrap();
    let run_id = create_node_circuit_run(source.id, None, 3).unwrap();
    assert_eq!(create_node_circuit_run(source.id, None, 3).unwrap(), run_id);
    assert!(list_autopilot_circuits(mesh.id).unwrap().is_empty(), "preset is hidden from user blueprints");
    let preset_count: i64 = write_conn().query_row(
        "SELECT COUNT(*) FROM autopilot_circuits WHERE mesh_id = ?1 AND is_preset = 1",
        rusqlite::params![mesh.id], |row| row.get(0),
    ).unwrap();
    assert_eq!(preset_count, 1);
    let run = get_circuit_run(run_id).unwrap().unwrap();
    assert_eq!(run.source_agent_node_id, Some(source.id));
    let ctx = crate::autopilot::circuit::context::CircuitContext::from_json(&run.context_json).unwrap();
    assert_eq!(ctx.source_agent_id(), Some(source.id));
    assert_eq!(ctx.get("source.base_ref"), Some(mesh.base_ref.as_str()));
    assert!(list_circuit_agent_ownerships().unwrap().iter().any(|row| row.0 == source.id && row.1 == run_id));
    commit_circuit_advance(run_id, Some("running"), None, &[CircuitStepOp {
        node_id: "reviewer".into(), status: "running".into(), outcome: None, error: None,
        agent_node_id: Some(123456), attempt: 1, fresh_attempt: false,
    }]).unwrap();
    assert_eq!(cancel_circuit_run(run_id).unwrap(), vec![123456]);
    assert!(get_agent_node_by_id(source.id).is_ok());
    assert!(!list_circuit_agent_ownerships().unwrap().iter().any(|row| row.0 == source.id));
    let history = list_circuits_with_recent_runs(mesh.id, 10).unwrap();
    assert_eq!(history.len(), 1, "the preset is history-visible after completion");
    assert!(history[0].0.is_preset);
    assert_eq!(history[0].1[0].run.state, "cancelled");
}

#[test]
fn node_circuit_rejects_other_mesh_and_nonmanual_blueprints() {
    init_temp_db("node-circuit-validation");
    let mesh = create_mesh("node-workflow-mesh", "/tmp/node-workflow").unwrap();
    let other = create_mesh("node-other-mesh", "/tmp/node-other").unwrap();
    let source = create_agent_node(mesh.id, "Source", &mesh.path, "main", EnvType::Windows,
        "claude", None, None, None, None, false, None, None, None).unwrap();
    update_agent_node_status(source.id, SessionStatus::Ready).unwrap();
    let foreign = create_autopilot_circuit(other.id, "foreign", "", 1, &sample_graph_json()).unwrap();
    assert!(create_node_circuit_run(source.id, Some(foreign.id), 3).unwrap_err().contains("manual Circuit"));
    let interval = CircuitGraph::triggered_skeleton("task", crate::autopilot::circuit::model::CircuitNodeKind::Interval { interval_seconds: 60 });
    let timed = create_autopilot_circuit(mesh.id, "timed", "", 1, &interval.to_json().unwrap()).unwrap();
    assert!(create_node_circuit_run(source.id, Some(timed.id), 3).is_err());
    let manual = create_autopilot_circuit(mesh.id, "manual", "", 1, &sample_graph_json()).unwrap();
    let run = create_node_circuit_run(source.id, Some(manual.id), 3).unwrap();
    assert_eq!(get_circuit_run(run).unwrap().unwrap().circuit_id, manual.id);
    cancel_circuit_run(run).unwrap();
    assert_ne!(create_node_circuit_run(source.id, Some(manual.id), 3).unwrap(), run);
}

// ---------------------------------------------------------------------------
// Circuit CRUD + persistence across "restarts" (re-open reads).
// ---------------------------------------------------------------------------

#[test]
fn circuit_crud_round_trips_all_fields() {
    let path = init_temp_db("crud");
    let mesh = create_mesh("circuit-crud-mesh", "/tmp/circuit-crud").unwrap();

    let created =
        create_autopilot_circuit(mesh.id, "nightly-sweep", "desc", 3, &sample_graph_json()).unwrap();
    assert_eq!(created.mesh_id, mesh.id);
    assert_eq!(created.name, "nightly-sweep");
    assert_eq!(created.description, "desc");
    assert_eq!(created.concurrency_limit, 3);
    assert!(!created.enabled, "circuits default to disabled (draft-first, issue #1356)");
    // graph_json round-trips back into the parsed AST.
    let parsed = CircuitGraph::from_json(&created.graph_json).unwrap();
    assert_eq!(parsed.nodes.len(), 4);

    let listed = list_autopilot_circuits(mesh.id).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);

    // Enable/disable round-trips (the Probe tab's toggle).
    assert!(!listed[0].enabled);
    set_autopilot_circuit_enabled(created.id, true).unwrap();
    let enabled = get_autopilot_circuit(created.id).unwrap().unwrap();
    assert!(enabled.enabled);
    set_autopilot_circuit_enabled(created.id, false).unwrap();
    let disabled = get_autopilot_circuit(created.id).unwrap().unwrap();
    assert!(!disabled.enabled);

    delete_autopilot_circuit(created.id).unwrap();
    assert!(get_autopilot_circuit(created.id).unwrap().is_none());
    assert!(list_autopilot_circuits(mesh.id).unwrap().is_empty());

    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn create_autopilot_circuit_is_draft_first_disabled() {
    let path = init_temp_db("draft_first");
    let mesh = create_mesh("circuit-draft-mesh", "/tmp/circuit-draft").unwrap();
    let created =
        create_autopilot_circuit(mesh.id, "still-authoring", "", 1, &sample_graph_json()).unwrap();
    assert!(!created.enabled);

    // Fresh-DB column default matches the INSERT (issue #1356).
    let dflt: Option<String> = {
        let db = write_conn();
        db.query_row(
            "SELECT dflt_value FROM pragma_table_info('autopilot_circuits') WHERE name = 'enabled'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(dflt.as_deref(), Some("0"));

    // Trigger Now is the dry-run seam and must mint a run while disabled.
    let run_id = crate::commands::circuit::trigger_circuit_now(created.id).unwrap();
    let run = get_circuit_run(run_id).unwrap().unwrap();
    assert_eq!(run.circuit_id, created.id);
    assert_eq!(run.state, "pending");
    assert!(
        run.trigger_identity.starts_with("manual:"),
        "manual identity, got {}",
        run.trigger_identity
    );

    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn update_autopilot_circuit_graph_persists_a_new_blueprint() {
    // Issue #1209: the canvas editor's save seam. The whole graph_json
    // is replaced and round-trips back into the AST; other columns
    // (name, enabled, concurrency_limit) are untouched.
    let path = init_temp_db("update_graph");
    let mesh = create_mesh("circuit-update-graph-mesh", "/tmp/circuit-update-graph").unwrap();
    let created =
        create_autopilot_circuit(mesh.id, "editable", "desc", 2, &sample_graph_json()).unwrap();

    let new_graph = CircuitGraph {
        version: 1,
        blueprint: None,
        nodes: vec![
            crate::autopilot::circuit::model::CircuitNode {
                id: "trigger".into(),
                kind: crate::autopilot::circuit::model::CircuitNodeKind::Manual,
            },
            crate::autopilot::circuit::model::CircuitNode {
                id: "verify".into(),
                kind: crate::autopilot::circuit::model::CircuitNodeKind::DeterministicVerification {
                    command: "cargo test".into(),
                },
            },
        ],
        edges: vec![crate::autopilot::circuit::model::CircuitEdge {
            from: "trigger".into(),
            to: "verify".into(),
            condition: crate::autopilot::circuit::model::EdgeCondition::OnOutcome(
                crate::autopilot::circuit::model::StepOutcome::Green,
            ),
        }],
    };
    update_autopilot_circuit_graph(created.id, &new_graph.to_json().unwrap()).unwrap();

    let reloaded = get_autopilot_circuit(created.id).unwrap().unwrap();
    assert_eq!(reloaded.name, "editable");
    assert_eq!(reloaded.concurrency_limit, 2);
    assert!(!reloaded.enabled, "graph save must not flip the draft-first enabled flag");
    let parsed = CircuitGraph::from_json(&reloaded.graph_json).unwrap();
    assert_eq!(parsed, new_graph);

    // A stale editor saving against a deleted circuit must error, not
    // silently no-op (review finding: rows_affected == 0 returned Ok).
    let missing = update_autopilot_circuit_graph(999_999, &sample_graph_json());
    assert!(missing.is_err());

    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn circuits_persist_across_a_restart_equivalent_evolution_rerun() {
    let path = init_temp_db("persist");
    // Every app start runs `db::init` → `evolve_to` against the existing
    // file. Simulate that on the live global DB: the migration rerun
    // must be a no-op for circuit rows (no drops, no resets), so an
    // enabled circuit survives restarts.
    let mesh = create_mesh("circuit-persist-mesh", "/tmp/circuit-persist").unwrap();
    let created =
        create_autopilot_circuit(mesh.id, "survivor", "", 1, &sample_graph_json()).unwrap();
    set_autopilot_circuit_enabled(created.id, true).unwrap();

    {
        let conn = write_conn();
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
    }

    let after = get_autopilot_circuit(created.id).unwrap().unwrap();
    assert_eq!(after.name, "survivor");
    assert!(after.enabled, "an explicitly enabled circuit must survive evolve_to");

    let _ = get();
    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------------------
// Runs + steps ledger.
// ---------------------------------------------------------------------------

#[test]
fn run_and_step_ledger_records_status_outcome_and_timestamps() {
    let path = init_temp_db("ledger");
    let mesh = create_mesh("circuit-ledger-mesh", "/tmp/circuit-ledger").unwrap();
    let circuit =
        create_autopilot_circuit(mesh.id, "ledgered", "", 2, &sample_graph_json()).unwrap();

    let run_id =
        create_circuit_run(circuit.id, mesh.id, "manual:1234", r#"{"circuit.name":"ledgered"}"#)
            .unwrap();
    let runs = list_circuit_runs(circuit.id, 10).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, "pending");
    assert_eq!(runs[0].trigger_identity, "manual:1234");
    // The run's seeded template context round-trips (Trigger Now writes
    // `circuit.*` before the worker ever sees the row).
    assert_eq!(runs[0].context_json, r#"{"circuit.name":"ledgered"}"#);

// One atomic advance: trigger completes, spawn starts.
    commit_circuit_advance(
        run_id,
        Some("running"),
        Some(r#"{"circuit.name":"ledgered"}"#),
        &[
            CircuitStepOp {
                node_id: "trigger".into(),
                status: "completed".into(),
                outcome: Some(Some("completed".into())),
                error: None,
                agent_node_id: None,
                attempt: 1,
                fresh_attempt: false,
            },
            CircuitStepOp {
                node_id: "spawn".into(),
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

    let steps = list_circuit_run_steps(run_id).unwrap();
    assert_eq!(steps.len(), 2);
    let spawn_step = steps.iter().find(|s| s.node_id == "spawn").unwrap();
    assert_eq!(spawn_step.status, "running");
    assert!(spawn_step.outcome.is_none());
    assert!(spawn_step.started_at.is_some(), "insert stamps started_at");
    assert!(spawn_step.completed_at.is_none(), "non-terminal step has no completed_at");
    assert_eq!(spawn_step.attempt, 1);

    // A stale worker association is a harmless no-op, not a rusqlite
    // QueryReturnedNoRows sentinel from an UPDATE with zero matches.
    assert!(!set_circuit_step_agent_node(run_id, "missing", 899).unwrap());
    // Attach the spawned agent node, then finish the step.
    set_circuit_step_agent_node(run_id, "spawn", 900).unwrap();
    commit_circuit_advance(
        run_id,
        Some("completed"),
        None,
        &[CircuitStepOp {
            node_id: "spawn".into(),
            status: "completed".into(),
            outcome: Some(Some("completed".into())),
            error: None,
            agent_node_id: None,
                attempt: 1,
        fresh_attempt: false,
        }],
    )
    .unwrap();

    let steps = list_circuit_run_steps(run_id).unwrap();
    let spawn_step = steps.iter().find(|s| s.node_id == "spawn").unwrap();
    assert_eq!(spawn_step.agent_node_id, Some(900));
    assert_eq!(spawn_step.outcome.as_deref(), Some("completed"));
    assert!(
        spawn_step.completed_at.is_some(),
        "terminal status stamps completed_at"
    );
    let runs = list_circuit_runs(circuit.id, 10).unwrap();
    assert_eq!(runs[0].state, "completed");

    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn pending_run_queue_is_oldest_first_and_can_be_reordered() {
    let path = init_temp_db("queue_order");
    let mesh = create_mesh("circuit-queue-mesh", "/tmp/circuit-queue").unwrap();
    let circuit =
        create_autopilot_circuit(mesh.id, "queue", "", 2, &sample_graph_json()).unwrap();

    let first = create_circuit_run(circuit.id, mesh.id, "manual:1", "{}").unwrap();
    let second = create_circuit_run(circuit.id, mesh.id, "manual:2", "{}").unwrap();
    let third = create_circuit_run(circuit.id, mesh.id, "manual:3", "{}").unwrap();

    let ids = list_queued_circuit_runs(mesh.id)
        .unwrap()
        .into_iter()
        .map(|(run, _)| run.id)
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![first, second, third]);

    move_queued_circuit_run(third, true).unwrap();
    let ids = list_queued_circuit_runs(mesh.id)
        .unwrap()
        .into_iter()
        .map(|(run, _)| run.id)
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![first, third, second]);

    move_queued_circuit_run(third, true).unwrap();
    let ids = list_queued_circuit_runs(mesh.id)
        .unwrap()
        .into_iter()
        .map(|(run, _)| run.id)
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![third, first, second]);

    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn circuit_ledger_keeps_older_active_runs_outside_the_history_limit() {
    let path = init_temp_db("ledger_active_outside_history");
    let mesh = create_mesh("circuit-active-ledger", "/tmp/circuit-active-ledger").unwrap();
    let circuit =
        create_autopilot_circuit(mesh.id, "active ledger", "", 2, &sample_graph_json()).unwrap();

    let older_active = create_circuit_run(circuit.id, mesh.id, "active", "{}").unwrap();
    set_circuit_run_state(older_active, "running").unwrap();
    let mut terminal_ids = Vec::new();
    for index in 0..11 {
        let run_id = create_circuit_run(
            circuit.id,
            mesh.id,
            &format!("terminal:{}", index),
            "{}",
        )
        .unwrap();
        commit_circuit_advance(run_id, Some("completed"), None, &[]).unwrap();
        terminal_ids.push(run_id);
    }

    let rows = list_circuits_with_recent_runs(mesh.id, 10).unwrap();
    let visible_ids = rows[0]
        .1
        .iter()
        .map(|ledger| ledger.run.id)
        .collect::<Vec<_>>();
    assert_eq!(visible_ids.len(), 11, "one active plus ten terminal rows");
    assert!(visible_ids.contains(&older_active));
    assert!(!visible_ids.contains(&terminal_ids[0]), "oldest terminal row is bounded out");

    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn cancelling_a_run_is_terminal_and_returns_attached_agents_for_cleanup() {
    let path = init_temp_db("cancel_run");
    let mesh = create_mesh("circuit-cancel-mesh", "/tmp/circuit-cancel").unwrap();
    let circuit =
        create_autopilot_circuit(mesh.id, "cancel", "", 2, &sample_graph_json()).unwrap();
    let run_id = create_circuit_run(circuit.id, mesh.id, "manual:cancel", "{}").unwrap();
    set_circuit_run_state(run_id, "running").unwrap();
    commit_circuit_advance(
        run_id,
        None,
        None,
        &[CircuitStepOp {
            node_id: "spawn".into(),
            status: "running".into(),
            outcome: None,
            error: None,
            agent_node_id: Some(991),
            attempt: 1,
            fresh_attempt: false,
        }],
    )
    .unwrap();

    assert!(reserve_circuit_agent_slots(run_id, 2).unwrap());
    assert_eq!(circuit_agent_slots_reserved(run_id).unwrap(), 2);

    let agents = cancel_circuit_run(run_id).unwrap();
    assert_eq!(agents, vec![991]);
    assert_eq!(get_circuit_run(run_id).unwrap().unwrap().state, "cancelled");
    let cancelled_step = list_circuit_run_steps(run_id).unwrap();
    assert_eq!(cancelled_step[0].status, "cancelled");
    assert_eq!(cancelled_step[0].outcome.as_deref(), Some("cancelled"));
    assert_eq!(count_active_circuit_runs(mesh.id).unwrap(), 0);
    assert_eq!(circuit_agent_slots_reserved(run_id).unwrap(), 0);
    assert_eq!(
        cancel_circuit_run(run_id).unwrap(),
        vec![991],
        "cleanup retries must retain the ledger's attached agents"
    );
    assert_eq!(
        list_circuit_run_ids_for_cleanup(circuit.id).unwrap(),
        vec![run_id]
    );

    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn stale_worker_and_pause_writes_cannot_resurrect_a_cancelled_run() {
    let path = init_temp_db("cancel_run_stale_commit");
    let mesh = create_mesh("circuit-cancel-race", "/tmp/circuit-cancel-race").unwrap();
    let circuit =
        create_autopilot_circuit(mesh.id, "cancel race", "", 2, &sample_graph_json()).unwrap();
    let run_id = create_circuit_run(circuit.id, mesh.id, "manual:cancel-race", "{}").unwrap();

    cancel_circuit_run(run_id).unwrap();
    commit_circuit_advance(
        run_id,
        Some("running"),
        Some(r#"{"stale":true}"#),
        &[CircuitStepOp {
            node_id: "spawn".into(),
            status: "running".into(),
            outcome: None,
            error: None,
            agent_node_id: None,
            attempt: 1,
            fresh_attempt: false,
        }],
    )
    .unwrap();
    set_circuit_run_state(run_id, "paused").unwrap();

    let run = get_circuit_run(run_id).unwrap().unwrap();
    assert_eq!(run.state, "cancelled");
    assert_eq!(run.context_json, "{}");
    assert!(list_circuit_run_steps(run_id).unwrap().is_empty());
    assert!(!transition_circuit_run_state(run_id, "running", "paused").unwrap());

    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn circuit_agent_ownership_comes_from_the_step_ledger() {
    let path = init_temp_db("agent_ownership");
    let mesh = create_mesh("circuit-owner-mesh", "/tmp/circuit-owner").unwrap();
    let circuit =
        create_autopilot_circuit(mesh.id, "issue autopilot", "", 2, &sample_graph_json())
            .unwrap();
    let run_id = create_circuit_run(circuit.id, mesh.id, "issue:42:run", "{}").unwrap();
    commit_circuit_advance(
        run_id,
        Some("running"),
        None,
        &[CircuitStepOp {
            node_id: "spawn".into(),
            status: "running".into(),
            outcome: None,
            error: None,
            agent_node_id: None,
            attempt: 1,
            fresh_attempt: false,
        }],
    )
    .unwrap();
    let agent = create_agent_node(
        mesh.id,
        "implementer",
        "/tmp/circuit-owner-node",
        "main",
        EnvType::Windows,
        "claude",
        None,
        Some(42),
        None,
        None,
        true,
        None,
        None,
        None,
    )
    .unwrap();
    set_circuit_step_agent_node(run_id, "spawn", agent.id).unwrap();

    assert_eq!(count_active_circuit_agent_nodes(mesh.id).unwrap(), 1);
    assert_eq!(count_active_circuit_agent_nodes_total().unwrap(), 1);
    commit_circuit_advance(run_id, Some("completed"), None, &[]).unwrap();
    assert_eq!(count_active_circuit_agent_nodes(mesh.id).unwrap(), 0);
    assert_eq!(
        count_retained_circuit_agent_nodes(mesh.id).unwrap(),
        1,
        "terminal runs still account for retained agent processes"
    );
    assert_eq!(count_retained_circuit_agent_nodes_total().unwrap(), 1);

    assert_eq!(
        list_circuit_agent_ownerships().unwrap(),
        vec![(agent.id, run_id, circuit.id, "issue autopilot".to_string(), "completed".to_string())]
    );

    clear_circuit_step_agent_node(run_id, "spawn").unwrap();
    assert!(
        list_circuit_agent_ownerships()
            .unwrap()
            .iter()
            .all(|(owned_agent_id, ..)| *owned_agent_id != agent.id),
        "clearing this step removes this agent's ownership without assuming other parallel tests are idle"
    );
    assert_eq!(count_retained_circuit_agent_nodes(mesh.id).unwrap(), 0);

    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn step_upsert_never_duplicates_a_node_row() {
    let path = init_temp_db("upsert");
    let mesh = create_mesh("circuit-upsert-mesh", "/tmp/circuit-upsert").unwrap();
    let circuit = create_autopilot_circuit(mesh.id, "dup", "", 1, "{}").unwrap();
    let run_id = create_circuit_run(circuit.id, mesh.id, "", "{}").unwrap();

for _ in 0..3 {
        commit_circuit_advance(
            run_id,
            None,
            None,
            &[CircuitStepOp {
                node_id: "spawn".into(),
                status: "running".into(),
                outcome: None,
                error: None,
                agent_node_id: Some(7),
                attempt: 1,
                fresh_attempt: false,
            }],
        )
        .unwrap();
    }
    let steps = list_circuit_run_steps(run_id).unwrap();
    assert_eq!(steps.len(), 1, "UNIQUE(run_id, node_id) must dedupe");
    assert_eq!(steps[0].agent_node_id, Some(7));

    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn deleting_a_circuit_explicitly_removes_runs_and_steps() {
    // The schema declares ON DELETE CASCADE, but enforcement depends on
    // the connection's foreign_keys pragma (on for bundled SQLite,
    // off for a system-libsqlite link) — so delete_autopilot_circuit
    // removes descendants explicitly. This test must stay honest about
    // WHICH mechanism it pins: the explicit deletes.
    let path = init_temp_db("cascade");
    let mesh = create_mesh("circuit-cascade-mesh", "/tmp/circuit-cascade").unwrap();
    let circuit = create_autopilot_circuit(mesh.id, "doomed", "", 1, "{}").unwrap();
    let run_id = create_circuit_run(circuit.id, mesh.id, "", "{}").unwrap();
    commit_circuit_advance(
        run_id,
        None,
        None,
        &[CircuitStepOp {
            node_id: "trigger".into(),
            status: "completed".into(),
            outcome: Some(Some("completed".into())),
            error: None,
            agent_node_id: None,
                attempt: 1,
        fresh_attempt: false,
        }],
    )
    .unwrap();

    delete_autopilot_circuit(circuit.id).unwrap();
    // The shared process-global DB holds other tests' rows too — count
    // only this circuit's descendants.
    let remaining_runs: i64 = {
        let conn = write_conn();
        conn.query_row(
            "SELECT COUNT(*) FROM autopilot_circuit_runs WHERE circuit_id = ?1",
            params![circuit.id],
            |row| row.get(0),
        )
        .unwrap()
    };
    let remaining_steps: i64 = {
        let conn = write_conn();
        conn.query_row(
            "SELECT COUNT(*) FROM autopilot_circuit_run_steps s \
             JOIN autopilot_circuit_runs r ON r.id = s.run_id \
             WHERE r.circuit_id = ?1",
            params![circuit.id],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(remaining_runs, 0, "runs must cascade with their circuit");
    assert_eq!(remaining_steps, 0, "steps must cascade with their run");

    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn deleting_a_mesh_removes_its_circuits_runs_and_steps() {
    // delete_mesh must not leave circuit-ledger orphans behind.
    let path = init_temp_db("mesh-cascade");
    let mesh = create_mesh("circuit-mesh-cascade", "/tmp/circuit-mesh-cascade").unwrap();
    let circuit =
        create_autopilot_circuit(mesh.id, "doomed-with-mesh", "", 1, "{}").unwrap();
    let run_id = create_circuit_run(circuit.id, mesh.id, "", "{}").unwrap();
    commit_circuit_advance(
        run_id,
        None,
        None,
        &[CircuitStepOp {
            node_id: "trigger".into(),
            status: "completed".into(),
            outcome: Some(Some("completed".into())),
            error: None,
            agent_node_id: None,
                attempt: 1,
        fresh_attempt: false,
        }],
    )
    .unwrap();

    delete_mesh(mesh.id).unwrap();

    assert!(list_autopilot_circuits(mesh.id).unwrap().is_empty());
    let remaining_runs: i64 = {
        let conn = write_conn();
        conn.query_row(
            "SELECT COUNT(*) FROM autopilot_circuit_runs WHERE mesh_id = ?1",
            params![mesh.id],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(remaining_runs, 0);

    let _ = get();
    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------------------
// Concurrency counters.
// ---------------------------------------------------------------------------

#[test]
fn concurrency_counters_count_only_running_work() {
    let path = init_temp_db("counters");
    let mesh_a = create_mesh("circuit-count-a", "/tmp/circuit-count-a").unwrap();
    let mesh_b = create_mesh("circuit-count-b", "/tmp/circuit-count-b").unwrap();
    let c1 = create_autopilot_circuit(mesh_a.id, "one", "", 4, "{}").unwrap();
    let c2 = create_autopilot_circuit(mesh_a.id, "two", "", 4, "{}").unwrap();
    let cb = create_autopilot_circuit(mesh_b.id, "bee", "", 4, "{}").unwrap();

    // Circuit one: one completed step + one running step piloting agent
    // 101 + one queued step (must not count).
    let r1 = create_circuit_run(c1.id, mesh_a.id, "", "{}").unwrap();
    commit_circuit_advance(
        r1,
        Some("running"),
        None,
        &[
            CircuitStepOp {
                node_id: "trigger".into(),
                status: "completed".into(),
                outcome: Some(Some("completed".into())),
                error: None,
                agent_node_id: None,
                attempt: 1,
            fresh_attempt: false,
            },
            CircuitStepOp {
                node_id: "spawn".into(),
                status: "running".into(),
                outcome: None,
                error: None,
                agent_node_id: Some(101),
                attempt: 1,
                fresh_attempt: false,
            },
            CircuitStepOp {
                node_id: "second-spawn".into(),
                status: "pending_slot".into(),
                outcome: None,
                error: None,
                agent_node_id: None,
                attempt: 1,
            fresh_attempt: false,
            },
        ],
    )
    .unwrap();

    // Circuit two (same mesh): running step piloting agent 102.
let r2 = create_circuit_run(c2.id, mesh_a.id, "", "{}").unwrap();
    commit_circuit_advance(
        r2,
        Some("running"),
        None,
        &[CircuitStepOp {
            node_id: "spawn".into(),
            status: "running".into(),
            outcome: None,
            error: None,
            agent_node_id: Some(102),
            attempt: 1,
            fresh_attempt: false,
        }],
    )
    .unwrap();

    // Mesh B: its own piloted agent must not leak into mesh A's count.
    let rb = create_circuit_run(cb.id, mesh_b.id, "", "{}").unwrap();
    commit_circuit_advance(
        rb,
        Some("running"),
        None,
        &[CircuitStepOp {
            node_id: "spawn".into(),
            status: "running".into(),
            outcome: None,
            error: None,
            agent_node_id: Some(999),
            attempt: 1,
            fresh_attempt: false,
        }],
    )
    .unwrap();

    assert_eq!(count_running_circuit_steps(c1.id).unwrap(), 1);
    assert_eq!(count_running_circuit_steps(c2.id).unwrap(), 1);
    assert_eq!(
        count_active_circuit_agent_nodes(mesh_a.id).unwrap(),
        2,
        "distinct piloted agents across circuits on mesh A"
    );
    assert_eq!(count_active_circuit_agent_nodes(mesh_b.id).unwrap(), 1);

    let _ = get();
    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------------------
// Run-level admission gate (issue #1467).
// ---------------------------------------------------------------------------

/// `running`/`paused` runs count as admitted; `pending` runs do NOT
/// (the gate's job is exactly to decide whether a pending run gets to
/// become running — if it counted, every pending run would see itself
/// + peers and the gate would deadlock). This is the contract that
/// lets the worker treat admission as a per-run counter, distinct
/// from the per-agent-node counter.
#[test]
fn count_active_circuit_runs_includes_running_paused_only() {
    let path = init_temp_db("run-count-active");
    let mesh = create_mesh("circuit-run-count", "/tmp/circuit-run-count").unwrap();
    let c1 = create_autopilot_circuit(mesh.id, "c1", "", 4, "{}").unwrap();
    let c2 = create_autopilot_circuit(mesh.id, "c2", "", 4, "{}").unwrap();
    let c3 = create_autopilot_circuit(mesh.id, "c3", "", 4, "{}").unwrap();

    // One pending, one running, one paused — only the latter two count.
    let r_pending = create_circuit_run(c1.id, mesh.id, "", "{}").unwrap();
    let r_running = create_circuit_run(c2.id, mesh.id, "", "{}").unwrap();
    let r_paused = create_circuit_run(c3.id, mesh.id, "", "{}").unwrap();
    set_circuit_run_state(r_running, "running").unwrap();
    set_circuit_run_state(r_paused, "running").unwrap();
    set_circuit_run_state(r_paused, "paused").unwrap();
    // r_pending stays at default "pending" — must NOT count.

    assert_eq!(
        count_active_circuit_runs(mesh.id).unwrap(),
        2,
        "admitted runs = running + paused; pending does not count toward the gate's input"
    );

    // Sanity: the pending run still exists in the run ledger
    // (gated, not deleted — the next pass re-evaluates it).
    let still_there = get_circuit_run(r_pending).unwrap().unwrap();
    assert_eq!(still_there.state, "pending");

    let _ = get();
    std::fs::remove_file(&path).ok();
}

/// Terminal runs (`completed`, `failed`) do NOT count toward the
/// run-admission gate. Mirrors the per-step `paused_runs_stay_active_and_counters_count_them`
/// test for the new run-level seam. Note: `pending` runs are also
/// excluded (see `count_active_circuit_runs_includes_running_paused_only`
/// for why — counting `pending` would self-deadlock at admission time).
#[test]
fn count_active_circuit_runs_excludes_terminal_states() {
    let path = init_temp_db("run-count-terminal");
    let mesh = create_mesh("circuit-run-count-terminal", "/tmp/circuit-run-count-terminal")
        .unwrap();
    let c1 = create_autopilot_circuit(mesh.id, "c1", "", 4, "{}").unwrap();
    let c2 = create_autopilot_circuit(mesh.id, "c2", "", 4, "{}").unwrap();
    let c3 = create_autopilot_circuit(mesh.id, "c3", "", 4, "{}").unwrap();

    let r_done = create_circuit_run(c1.id, mesh.id, "", "{}").unwrap();
    commit_circuit_advance(r_done, Some("completed"), None, &[]).unwrap();
    let r_dead = create_circuit_run(c2.id, mesh.id, "", "{}").unwrap();
    commit_circuit_advance(r_dead, Some("failed"), None, &[]).unwrap();
    // r_pending stays non-terminal; per the FIFO gate shape, pending
    // does NOT count toward admission input either.
    let r_pending = create_circuit_run(c3.id, mesh.id, "", "{}").unwrap();

    assert_eq!(
        count_active_circuit_runs(mesh.id).unwrap(),
        0,
        "only running/paused count; pending and terminal both excluded"
    );

    let _ = get();
    std::fs::remove_file(&path).ok();

    // Sanity: the terminal commits didn't wipe rows. r_pending is
    // still there pending (will be re-evaluated next worker pass);
    // r_done and r_dead are terminal in DB.
    assert_eq!(get_circuit_run(r_done).unwrap().unwrap().state, "completed");
    assert_eq!(get_circuit_run(r_dead).unwrap().unwrap().state, "failed");
    assert_eq!(get_circuit_run(r_pending).unwrap().unwrap().state, "pending");
}

/// Single-release idempotency lives inside `commit_circuit_advance`'s
/// terminal-state branch (issue #1467, ADR-0028). A second commit
/// against an already-terminal row is a no-op (the `WHERE state IN
/// ('pending','running','paused')` filter matches zero rows), so the
/// capacity count is never double-decremented.
#[test]
fn commit_circuit_advance_terminal_state_is_idempotent_under_double_signal() {
    let path = init_temp_db("commit-term-idem");
    let mesh = create_mesh("circuit-commit-term-idem", "/tmp/circuit-commit-term-idem").unwrap();
    let c1 = create_autopilot_circuit(mesh.id, "c1", "", 4, "{}").unwrap();
    let r1 = create_circuit_run(c1.id, mesh.id, "", "{}").unwrap();
    set_circuit_run_state(r1, "running").unwrap();

    // First terminal commit: 1 → 0 admitted runs on the mesh.
    commit_circuit_advance(r1, Some("completed"), None, &[]).unwrap();
    assert_eq!(count_active_circuit_runs(mesh.id).unwrap(), 0);
    let row = get_circuit_run(r1).unwrap().unwrap();
    assert_eq!(row.state, "completed");

    // Second terminal commit (a different terminal value): the WHERE
    // filter excludes the already-terminal row, so the state stays at
    // "completed" — the first commit's outcome wins.
    commit_circuit_advance(r1, Some("failed"), None, &[]).unwrap();
    assert_eq!(count_active_circuit_runs(mesh.id).unwrap(), 0);
    let row = get_circuit_run(r1).unwrap().unwrap();
    assert_eq!(
        row.state, "completed",
        "second terminal commit must not overwrite the first"
    );

    let _ = get();
    std::fs::remove_file(&path).ok();
}

/// A commit against a row that's already in a terminal state (e.g.
/// crash-recovery picked it up as `failed` already) must not flip the
/// state — same idempotency key as the previous test, pinned
/// separately so the test failure message points at the right case.
#[test]
fn commit_circuit_advance_terminal_state_skips_already_terminal_runs() {
    let path = init_temp_db("commit-term-skip");
    let mesh = create_mesh("circuit-commit-term-skip", "/tmp/circuit-commit-term-skip").unwrap();
    let c1 = create_autopilot_circuit(mesh.id, "c1", "", 4, "{}").unwrap();
    let r1 = create_circuit_run(c1.id, mesh.id, "", "{}").unwrap();
    set_circuit_run_state(r1, "failed").unwrap();

    // Attempt to commit `completed` against an already-failed row:
    // the WHERE filter blocks the row, no write happens.
    commit_circuit_advance(r1, Some("completed"), None, &[]).unwrap();
    let row = get_circuit_run(r1).unwrap().unwrap();
    assert_eq!(
        row.state, "failed",
        "must not overwrite an existing terminal state"
    );

    let _ = get();
    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------------------
// Active-run listing (the worker pass's input).
// ---------------------------------------------------------------------------

#[test]
fn active_run_listing_joins_circuit_fields_and_skips_terminal_runs() {
    let path = init_temp_db("active");
    let mesh = create_mesh("circuit-active-mesh", "/tmp/circuit-active").unwrap();
    let circuit =
        create_autopilot_circuit(mesh.id, "watched", "", 3, &sample_graph_json()).unwrap();
    set_autopilot_circuit_enabled(circuit.id, true).unwrap();

    let done = create_circuit_run(circuit.id, mesh.id, "manual:1", "{}").unwrap();
    commit_circuit_advance(done, Some("completed"), Some("{}"), &[]).unwrap();
    let live = create_circuit_run(circuit.id, mesh.id, "manual:2", "{}").unwrap();

    // The shared process-global DB holds other tests' runs too — scope
    // the assertion to this circuit.
    let active = list_active_circuit_runs()
        .unwrap()
        .into_iter()
        .filter(|r| r.run.circuit_id == circuit.id)
        .collect::<Vec<_>>();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].run.id, live);
    assert_eq!(active[0].run.state, "pending");
    assert_eq!(active[0].circuit_name, "watched");
    assert_eq!(active[0].circuit_concurrency_limit, 3);
    assert!(active[0].circuit_enabled);
    assert_eq!(
        CircuitGraph::from_json(&active[0].circuit_graph_json).unwrap().nodes.len(),
        4
    );

    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn duplicate_trigger_identity_replays_the_existing_run() {
    // The spec's dedupe: re-reporting the same (circuit, identity)
    // replays the original run instead of minting a duplicate — but a
    // DIFFERENT circuit reacting to the same identity is independent.
    let path = init_temp_db("dedupe");
    let mesh = create_mesh("circuit-dedupe-mesh", "/tmp/circuit-dedupe").unwrap();
    let c1 = create_autopilot_circuit(mesh.id, "one", "", 1, "{}").unwrap();
    let c2 = create_autopilot_circuit(mesh.id, "two", "", 1, "{}").unwrap();

    let first = create_circuit_run(c1.id, mesh.id, "issue:42:buildmesh:run", "{}").unwrap();
    let replay =
        create_circuit_run(c1.id, mesh.id, "issue:42:buildmesh:run", "{}").unwrap();
    assert_eq!(first, replay, "same circuit + identity must dedupe to one run");
    let independent =
        create_circuit_run(c2.id, mesh.id, "issue:42:buildmesh:run", "{}").unwrap();
    assert_ne!(
        first, independent,
        "a second circuit may process the same source independently"
    );

    let _ = get();
    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------------------
// Trigger-pass inputs (issue #1208): enabled circuits across meshes and
// the interval cooldown anchor.
// ---------------------------------------------------------------------------

#[test]
fn enabled_circuits_listing_spans_meshes_and_skips_disabled() {
    let path = init_temp_db("enabled");
    let mesh_a = create_mesh("circuit-enabled-a", "/tmp/circuit-enabled-a").unwrap();
    let mesh_b = create_mesh("circuit-enabled-b", "/tmp/circuit-enabled-b").unwrap();
    let on_a = create_autopilot_circuit(mesh_a.id, "on-a", "", 1, "{}").unwrap();
    let on_b = create_autopilot_circuit(mesh_b.id, "on-b", "", 1, "{}").unwrap();
    let off = create_autopilot_circuit(mesh_a.id, "off", "", 1, "{}").unwrap();
    set_autopilot_circuit_enabled(on_a.id, true).unwrap();
    set_autopilot_circuit_enabled(on_b.id, true).unwrap();

    // The shared process-global DB holds other tests' circuits — scope
    // to the ids this test created.
    let listed: Vec<i64> = list_enabled_circuits()
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .filter(|id| [on_a.id, on_b.id, off.id].contains(id))
        .collect();
    assert_eq!(listed.len(), 2, "disabled circuits must not appear");
    assert!(listed.contains(&on_a.id) && listed.contains(&on_b.id), "listing spans meshes");

    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn latest_run_created_at_tracks_the_newest_run_and_none_before_any() {
    let path = init_temp_db("cooldown");
    let mesh = create_mesh("circuit-cooldown-mesh", "/tmp/circuit-cooldown").unwrap();
    let circuit = create_autopilot_circuit(mesh.id, "paced", "", 1, "{}").unwrap();

    assert!(
        latest_circuit_run_created_at(circuit.id).unwrap().is_none(),
        "a never-fired circuit has no cooldown anchor"
    );

    create_circuit_run(circuit.id, mesh.id, "interval:1000", "{}").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    create_circuit_run(circuit.id, mesh.id, "interval:2500", "{}").unwrap();

    let latest = latest_circuit_run_created_at(circuit.id).unwrap().unwrap();
    // MAX(created_at) must be the SECOND run's timestamp — the sleep
    // guarantees SQLite's datetime strings differ and sort correctly.
    let runs = list_circuit_runs(circuit.id, 10).unwrap();
    let newest = runs.iter().map(|r| r.created_at.as_str()).max().unwrap().to_string();
    assert_eq!(latest, newest);

    // The identity set the GitHub poll pass pre-filters against.
    let identities = list_circuit_trigger_identities(circuit.id).unwrap();
    assert_eq!(identities.len(), 2);
    assert!(identities.contains(&"interval:1000".to_string()));
    assert!(identities.contains(&"interval:2500".to_string()));

    let _ = get();
    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------------------
// Migration pin: a v33 database receives the circuit tables during the
// migration runner's baseline-table phase.
// ---------------------------------------------------------------------------

#[test]
fn evolve_to_v34_creates_circuit_tables_and_queue_index_from_a_v33_db() {
    let conn = Connection::open_in_memory().unwrap();
    // Hand-rolled v33-shape DB: app_settings pinned at version 33 plus
    // just enough of meshes for the column walk to find its tables. No
    // circuit tables — a v33 build predates them.
    conn.execute_batch(
        "
        CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE meshes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            layout TEXT NOT NULL DEFAULT 'grid',
            position INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        INSERT OR REPLACE INTO app_settings (key, value) VALUES ('schema_version', '33');
        ",
    )
    .unwrap();

    crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();

    for table in [
        "autopilot_circuits",
        "autopilot_circuit_runs",
        "autopilot_circuit_run_steps",
        "autopilot_circuit_run_agent_leases",
    ] {
        let present: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get::<_, i64>(0).map(|c| c > 0),
            )
            .unwrap();
        assert!(present, "table {table} must exist after schema evolution");
    }

    // The UNIQUE(run_id, node_id) constraint is load-bearing for the
    // engine's upsert commit — pin it directly.
    let dupes_rejected = conn
        .execute_batch(
            "
            INSERT INTO autopilot_circuits (mesh_id, name) VALUES (1, 'pin');
            INSERT INTO autopilot_circuit_runs (circuit_id, mesh_id) VALUES (1, 1);
            INSERT INTO autopilot_circuit_run_steps (run_id, node_id, status)
                VALUES (1, 'spawn', 'running');
            ",
        )
        .and_then(|_| {
            conn.execute(
                "INSERT INTO autopilot_circuit_run_steps (run_id, node_id, status) \
                 VALUES (1, 'spawn', 'completed')",
                [],
            )
        })
        .is_err();
    assert!(
        dupes_rejected,
        "a second (run_id, node_id) row must violate the UNIQUE constraint"
    );

    let queue_index: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_circuit_runs_mesh_queue'",
            [],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        )
        .unwrap();
    assert!(queue_index, "queue ordering must have a composite mesh/state/position index");

    // Idempotent: re-running the migration must not error or duplicate.
    crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
}

#[test]
fn review_preset_migration_collapses_duplicates_and_backfills_source_binding() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;
        CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        INSERT INTO app_settings (key, value) VALUES ('schema_version', '39');
        CREATE TABLE meshes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            layout TEXT NOT NULL DEFAULT 'grid',
            position INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        INSERT INTO meshes (name, path) VALUES ('mesh', '/tmp/review-migration');
        CREATE TABLE agent_nodes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            mesh_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            path TEXT NOT NULL,
            branch TEXT NOT NULL DEFAULT 'main',
            env TEXT NOT NULL DEFAULT 'windows',
            provider TEXT NOT NULL DEFAULT 'anthropic',
            status TEXT NOT NULL DEFAULT 'idle',
            cli_session_id TEXT,
            worktree_name TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        INSERT INTO agent_nodes (id, mesh_id, name, path) VALUES (7, 1, 'source', '/tmp/source');
        CREATE TABLE autopilot_circuits (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            mesh_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            enabled INTEGER NOT NULL DEFAULT 0,
            concurrency_limit INTEGER NOT NULL DEFAULT 1,
            graph_json TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE autopilot_circuit_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            circuit_id INTEGER NOT NULL,
            mesh_id INTEGER NOT NULL,
            trigger_identity TEXT NOT NULL DEFAULT '',
            state TEXT NOT NULL DEFAULT 'pending',
            context_json TEXT NOT NULL DEFAULT '{}',
            queue_position INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE (circuit_id, trigger_identity)
        );
        INSERT INTO autopilot_circuits (mesh_id, name, description, graph_json)
            VALUES (1, 'Review agent 7', 'Review an existing agent and return findings until approved', '{}');
        INSERT INTO autopilot_circuits (mesh_id, name, description, graph_json)
            VALUES (1, 'Review agent 7', 'Review an existing agent and return findings until approved', '{}');
        INSERT INTO autopilot_circuit_runs (circuit_id, mesh_id, trigger_identity, context_json)
            VALUES (2, 1, 'manual:agent:7:old', '{\"source.agent_id\":\"7\"}');
        INSERT INTO autopilot_circuit_runs (circuit_id, mesh_id, trigger_identity, context_json)
            VALUES (1, 1, 'manual:agent:999:old', '{\"source.agent_id\":\"999\"}');
        ",
    )
    .unwrap();

    crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();

    let preset_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM autopilot_circuits WHERE mesh_id = 1 AND is_preset = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(preset_count, 1);
    let (circuit_id, source_id): (i64, Option<i64>) = conn
        .query_row(
            "SELECT circuit_id, source_agent_node_id FROM autopilot_circuit_runs WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(circuit_id, 1, "run history moves to the canonical preset row");
    assert_eq!(source_id, Some(7));
    let missing_source: Option<i64> = conn
        .query_row(
            "SELECT source_agent_node_id FROM autopilot_circuit_runs WHERE id = 2",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(missing_source, None, "deleted historical sources remain nullable during migration");
    let unique_preset_index: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'uq_autopilot_circuits_preset_mesh'",
            [],
            |row| row.get::<_, i64>(0).map(|count| count > 0),
        )
        .unwrap();
    assert!(unique_preset_index, "each mesh can have only one review preset row");
}

// ---------------------------------------------------------------------------
// Milestone 2 (#1207): pause/resume, retry attempts, gate outcomes.
// ---------------------------------------------------------------------------

#[test]
fn paused_runs_stay_active_and_counters_count_them() {
    let path = init_temp_db("paused");
    let mesh = create_mesh("circuit-pause-mesh", "/tmp/circuit-pause").unwrap();
    let circuit =
        create_autopilot_circuit(mesh.id, "pausable", "", 4, &sample_graph_json()).unwrap();

    // A running step on a RUNNING run occupies one slot.
    let r1 = create_circuit_run(circuit.id, mesh.id, "manual:1", "{}").unwrap();
    commit_circuit_advance(
        r1,
        Some("running"),
        None,
        &[CircuitStepOp {
            node_id: "spawn".into(),
            status: "running".into(),
            outcome: None,
            error: None,
            agent_node_id: Some(900),
            attempt: 1,
            fresh_attempt: false,
        }],
    )
    .unwrap();

    // Pause the run: it must stay in the active list and keep counting.
    // (The ledger DB is process-global across this module's tests, so
    // scope the assertion to this circuit rather than the whole table.)
    set_circuit_run_state(r1, "paused").unwrap();
    let active = list_active_circuit_runs().unwrap();
    let mine: Vec<_> = active.iter().filter(|a| a.run.circuit_id == circuit.id).collect();
    assert_eq!(mine.len(), 1, "a paused run stays active (it resumes later)");
    assert_eq!(mine[0].run.state, "paused");
    assert_eq!(count_running_circuit_steps(circuit.id).unwrap(), 1);
    assert_eq!(count_active_circuit_agent_nodes(mesh.id).unwrap(), 1);

    // Resume flips the state back through the same setter.
    set_circuit_run_state(r1, "running").unwrap();
    let active = list_active_circuit_runs().unwrap();
    let mine: Vec<_> = active.iter().filter(|a| a.run.circuit_id == circuit.id).collect();
    assert_eq!(mine[0].run.state, "running");

    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn fresh_attempt_ops_clear_the_previous_round_and_bump_attempt() {
    let path = init_temp_db("retry-attempt");
    let mesh = create_mesh("circuit-retry-mesh", "/tmp/circuit-retry").unwrap();
    let circuit = create_autopilot_circuit(mesh.id, "flaky", "", 2, &sample_graph_json()).unwrap();
    let run_id = create_circuit_run(circuit.id, mesh.id, "manual:1", "{}").unwrap();

    // Round 1 fails with an error.
    commit_circuit_advance(
        run_id,
        None,
        None,
        &[CircuitStepOp {
            node_id: "work".into(),
            status: "failed".into(),
            outcome: Some(Some("failed".into())),
            error: Some(Some("boom".into())),
            agent_node_id: None,
            attempt: 1,
            fresh_attempt: false,
        }],
    )
    .unwrap();

    // Retry reset: fresh_attempt clears outcome/error and stamps attempt 2.
    commit_circuit_advance(
        run_id,
        None,
        None,
        &[CircuitStepOp {
            node_id: "work".into(),
            status: "pending_slot".into(),
            outcome: Some(None),
            error: None,
            agent_node_id: None,
            attempt: 2,
            fresh_attempt: true,
        }],
    )
    .unwrap();

    let steps = list_circuit_run_steps(run_id).unwrap();
    let work = steps.iter().find(|s| s.node_id == "work").unwrap();
    assert_eq!(work.status, "pending_slot");
    assert_eq!(work.attempt, 2, "the retry's execution count persists");
    assert_eq!(work.outcome, None, "fresh attempt clears the stale outcome");
    assert_eq!(work.error_message, None, "fresh attempt clears the stale error");
    assert!(work.started_at.is_some());
    assert!(work.completed_at.is_none(), "a reset step is back in flight");

    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn explicit_error_clear_works_for_running_and_completed_steps() {
    let path = init_temp_db("classifier-recovery-error");
    let mesh = create_mesh("classifier recovery", "/tmp/classifier-recovery").unwrap();
    let circuit = create_autopilot_circuit(mesh.id, "gated", "", 2, &sample_graph_json()).unwrap();
    let run_id = create_circuit_run(circuit.id, mesh.id, "manual:1", "{}").unwrap();
    let mut op = CircuitStepOp {
        node_id: "classifier".into(), status: "running".into(), outcome: None,
        error: Some(Some("Classifier unavailable; retrying".into())), agent_node_id: None,
        attempt: 2, fresh_attempt: false,
    };
    commit_circuit_advance(run_id, None, None, &[op.clone()]).unwrap();
    assert!(list_circuit_run_steps(run_id).unwrap()[0].error_message.is_some());
    op.error = Some(None);
    commit_circuit_advance(run_id, None, None, &[op.clone()]).unwrap();
    assert_eq!(list_circuit_run_steps(run_id).unwrap()[0].error_message, None);
    op.error = Some(Some("stale again".into()));
    commit_circuit_advance(run_id, None, None, &[op.clone()]).unwrap();
    op.status = "completed".into();
    op.outcome = Some(Some("completed".into()));
    op.error = Some(None);
    commit_circuit_advance(run_id, None, None, &[op]).unwrap();
    let step = list_circuit_run_steps(run_id).unwrap().remove(0);
    assert_eq!(step.error_message, None);
    assert_eq!(step.attempt, 2);
    assert!(step.completed_at.is_some());
    let _ = get();
    std::fs::remove_file(&path).ok();
}

#[test]
fn gate_outcomes_stamp_completed_at_and_round_trip() {
    let path = init_temp_db("gate-outcomes");
    let mesh = create_mesh("circuit-gate-mesh", "/tmp/circuit-gate").unwrap();
    let circuit = create_autopilot_circuit(mesh.id, "gated", "", 2, &sample_graph_json()).unwrap();
    let run_id = create_circuit_run(circuit.id, mesh.id, "manual:1", "{}").unwrap();

    for outcome in ["blocked", "working", "green", "red"] {
        commit_circuit_advance(
            run_id,
            None,
            None,
            &[CircuitStepOp {
                node_id: format!("gate-{outcome}"),
                status: "completed".into(),
                outcome: Some(Some(outcome.into())),
                error: None,
                agent_node_id: None,
                attempt: 1,
                fresh_attempt: false,
            }],
        )
        .unwrap();
    }

    // A blocked STATUS (collaborator approval parking) is not terminal.
    commit_circuit_advance(
        run_id,
        None,
        None,
        &[CircuitStepOp {
            node_id: "approval".into(),
            status: "blocked".into(),
            outcome: None,
            error: None,
            agent_node_id: None,
            attempt: 1,
            fresh_attempt: false,
        }],
    )
    .unwrap();

    let steps = list_circuit_run_steps(run_id).unwrap();
    for outcome in ["blocked", "working", "green", "red"] {
        let step = steps.iter().find(|s| s.node_id == format!("gate-{outcome}")).unwrap();
        assert_eq!(step.outcome.as_deref(), Some(outcome));
        assert!(
            step.completed_at.is_some(),
            "gate outcome {outcome} is terminal and stamps completed_at"
        );
    }
    let parked = steps.iter().find(|s| s.node_id == "approval").unwrap();
    assert!(
        parked.completed_at.is_none(),
        "a blocked status parks without completing"
    );

    let _ = get();
    std::fs::remove_file(&path).ok();
}
