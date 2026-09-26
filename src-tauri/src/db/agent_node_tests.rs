//! Issue #654 — closes the orchestrator↔reader-thread status write race.
//! Tests exercise both primitives at the SQL layer (in-memory fixture,
//! no global DB, no thread timing) so the race is reproducible
//! deterministically.
//!
//! Run with: cargo test --package buildmesh --lib db::agent_node_tests

#[cfg(test)]
mod tests {
    use crate::db::{
        clear_cli_session_id_inner, cli_session_id_present_inner,
        update_agent_node_status_if_inner,
        update_agent_node_status_inner as update_unconditional_inner,
        update_agent_node_status_unless_in_inner,
    };
    use crate::models::SessionStatus;
    use rusqlite::{params, Connection};

    /// Minimal `agent_nodes` schema carrying only the columns the conditional
    /// update touches. The full schema is overkill for a SQL-semantics test.
    fn conn_with_agent_nodes() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                status TEXT NOT NULL DEFAULT 'idle',
                cli_session_id TEXT,
                session_started_at INTEGER,
                status_changed_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE app_settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn recovered_turn_accepts_sqlite_and_rfc3339_lifecycle_stamps() {
        let completed = 1789324053252;
        for stamp in ["1789321859469:2026-09-13T18:25:20.569845100+00:00",
            "1789321859469:2026-09-13 18:25:20",
            "1789321859469:2026-09-13 18:25:20.569"] {
            assert!(crate::db::agent_turn_stamp_precedes(stamp, completed), "{stamp}");
        }
        for stamp in ["1789321859469:invalid", "bad:2026-09-13 18:25:20",
            "1789321859469:2026-09-13 18:27:34"] {
            assert!(!crate::db::agent_turn_stamp_precedes(stamp, completed), "{stamp}");
        }
    }

    fn circuit_recovery_fixture() -> (Connection, crate::db::agent_node::CircuitRecoveryFence) {
        use crate::autopilot::circuit::model::{CircuitGraph, CircuitNode, CircuitNodeKind};
        let conn = conn_with_agent_nodes();
        conn.execute_batch("CREATE TABLE autopilot_circuit_runs(id INTEGER PRIMARY KEY,state TEXT,context_json TEXT);
            CREATE TABLE autopilot_circuit_run_steps(run_id INTEGER,node_id TEXT,status TEXT,attempt INTEGER,agent_node_id INTEGER);
            INSERT INTO agent_nodes(id,status,session_started_at,status_changed_at) VALUES(77,'running',100,'2000-01-01T00:00:00Z');
            INSERT INTO autopilot_circuit_runs VALUES(1,'running','{\"source.agent_id\":\"77\"}');
            INSERT INTO autopilot_circuit_run_steps VALUES(1,'gate','unverified',1,NULL);").unwrap();
        let fence = crate::db::agent_node::CircuitRecoveryFence {
            run_id: 1, step_id: "gate".into(), attempt: 1, agent_node_id: 77,
            graph: CircuitGraph { version: 3, blueprint: None, edges: vec![], nodes: vec![CircuitNode {
                id: "gate".into(), kind: CircuitNodeKind::AwaitAgentTurn { target_node_id: Some("$source".into()) },
            }] },
        };
        (conn, fence)
    }

    #[test]
    fn circuit_recovery_waiting_for_writer_rechecks_paused_and_cancelled_borrowed_run() {
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::Duration;
        for state in ["paused", "cancelled"] {
            for recovery_status in [SessionStatus::Ready, SessionStatus::AwaitingInput] {
                let (conn, fence) = circuit_recovery_fixture();
                let writer = Arc::new(Mutex::new(conn));
                // Recovery already passed its observational Running check.
                let lock = writer.lock().unwrap();
                assert_eq!(lock.query_row("SELECT state FROM autopilot_circuit_runs WHERE id=1", [], |row| row.get::<_, String>(0)).unwrap(), "running");
                let worker_writer = writer.clone();
                let (waiting_tx, waiting_rx) = mpsc::channel();
                let recovery = std::thread::spawn(move || {
                    waiting_tx.send(()).unwrap();
                    let mut conn = worker_writer.lock().unwrap();
                    let transaction = conn.transaction().unwrap();
                    let committed = crate::db::agent_node::recover_circuit_agent_turn_inner(
                        &transaction, &fence, "100:2000-01-01T00:00:00Z", recovery_status).unwrap();
                    transaction.commit().unwrap();
                    committed
                });
                waiting_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                lock.execute("UPDATE autopilot_circuit_runs SET state=?1 WHERE id=1", [state]).unwrap();
                drop(lock);
                assert!(!recovery.join().unwrap(), "{state} must suppress {recovery_status:?} and its lifecycle publication");
                assert_eq!(current_status(&writer.lock().unwrap(), 77), "running", "borrowed source is untouched");
            }
        }
    }

    #[test]
    fn circuit_recovery_atomically_checks_attempt_target_and_lifecycle() {
        for change in ["UPDATE autopilot_circuit_run_steps SET attempt=2",
            "UPDATE autopilot_circuit_runs SET context_json='{\"source.agent_id\":\"88\"}'",
            "UPDATE agent_nodes SET session_started_at=101"] {
            let (mut conn, fence) = circuit_recovery_fixture();
            conn.execute_batch(change).unwrap();
            let transaction = conn.transaction().unwrap();
            assert!(!crate::db::agent_node::recover_circuit_agent_turn_inner(&transaction, &fence,
                "100:2000-01-01T00:00:00Z", SessionStatus::Ready).unwrap(), "{change}");
            transaction.commit().unwrap();
            assert_eq!(current_status(&conn, 77), "running");
        }
        for recovery_status in [SessionStatus::Ready, SessionStatus::AwaitingInput] {
            let (mut conn, fence) = circuit_recovery_fixture();
            let transaction = conn.transaction().unwrap();
            assert!(crate::db::agent_node::recover_circuit_agent_turn_inner(&transaction, &fence,
                "100:2000-01-01T00:00:00Z", recovery_status).unwrap());
            transaction.commit().unwrap();
            assert_eq!(current_status(&conn, 77), recovery_status.to_db_str());
        }
    }

    fn insert_node(conn: &Connection, status: &str) -> i64 {
        conn.execute(
            "INSERT INTO agent_nodes (status) VALUES (?1)",
            params![status],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn current_status(conn: &Connection, id: i64) -> String {
        conn.query_row(
            "SELECT status FROM agent_nodes WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn current_changed_at(conn: &Connection, id: i64) -> String {
        conn.query_row(
            "SELECT status_changed_at FROM agent_nodes WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn clear_cli_session_id_removes_stale_conversation_identity() {
        let conn = conn_with_agent_nodes();
        let id = insert_node(&conn, "idle");
        conn.execute(
            "UPDATE agent_nodes SET cli_session_id = ?1 WHERE id = ?2",
            params!["anthropic-session-id", id],
        )
        .unwrap();

        clear_cli_session_id_inner(&conn, id).unwrap();

        let session_id: Option<String> = conn
            .query_row(
                "SELECT cli_session_id FROM agent_nodes WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(session_id, None);
        let started: i64 = conn.query_row(
            "SELECT session_started_at FROM agent_nodes WHERE id = ?1",
            params![id], |row| row.get(0),
        ).unwrap();
        assert!(started > 0);
    }

    #[test]
    fn fresh_identity_and_recovery_timestamp_are_written_together() {
        let conn = conn_with_agent_nodes();
        let id = insert_node(&conn, "suspended");
        conn.execute("UPDATE agent_nodes SET cli_session_id = 'old' WHERE id = ?1", [id]).unwrap();
        clear_cli_session_id_inner(&conn, id).unwrap();
        let (identity, started): (Option<String>, Option<i64>) = conn.query_row(
            "SELECT cli_session_id, session_started_at FROM agent_nodes WHERE id = ?1",
            [id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(identity, None);
        assert!(started.is_some());
    }

    /// Issue #1499: the poller's lean presence check mirrors
    /// `set_cli_session_id_if_missing_inner`'s write guard — NULL and empty
    /// read as absent (a write would proceed), any stored id reads present.
    #[test]
    fn cli_session_id_present_mirrors_conditional_write_guard() {
        let conn = conn_with_agent_nodes();
        let missing = insert_node(&conn, "idle");
        assert!(!cli_session_id_present_inner(&conn, missing).unwrap());
        conn.execute(
            "UPDATE agent_nodes SET cli_session_id = '' WHERE id = ?1",
            params![missing],
        )
        .unwrap();
        assert!(!cli_session_id_present_inner(&conn, missing).unwrap());
        conn.execute(
            "UPDATE agent_nodes SET cli_session_id = '550e8400-e29b-41d4-a716-446655440000' WHERE id = ?1",
            params![missing],
        )
        .unwrap();
        assert!(cli_session_id_present_inner(&conn, missing).unwrap());
    }

    /// Happy path: when `expected` matches the current row status, the new
    /// status is applied and `status_changed_at` is bumped. This is what the
    /// delayed Running promotion sees for a healthy spawn.
    #[test]
    fn update_status_if_writes_when_expected_matches() {
        let conn = conn_with_agent_nodes();
        let id = insert_node(&conn, "spawning");
        let before = current_changed_at(&conn, id);

        // Tiny sleep so the RFC3339 timestamp differs at the millisecond.
        // The status_changed_at column is bumped on every successful write —
        // a no-op UPDATE must not bump it (see the negative test below).
        std::thread::sleep(std::time::Duration::from_millis(5));

        let applied = update_agent_node_status_if_inner(
            &conn,
            id,
            SessionStatus::Running,
            SessionStatus::Spawning,
        )
        .unwrap();

        assert!(applied, "the update should have applied");
        assert_eq!(current_status(&conn, id), "running");
        assert_ne!(
            current_changed_at(&conn, id),
            before,
            "status_changed_at must be bumped on a real transition",
        );
    }

    /// Reader thread wrote `error` before the orchestrator's delayed Running
    /// promotion fires — promotion must no-op (#654).
    #[test]
    fn update_status_if_noop_when_reader_already_wrote_error() {
        let conn = conn_with_agent_nodes();
        let id = insert_node(&conn, "error");
        let before = current_changed_at(&conn, id);

        std::thread::sleep(std::time::Duration::from_millis(5));

        let applied = update_agent_node_status_if_inner(
            &conn,
            id,
            SessionStatus::Running,
            SessionStatus::Spawning,
        )
        .unwrap();

        assert!(!applied, "the update must be a no-op when expected mismatches");
        assert_eq!(current_status(&conn, id), "error");
        // A no-op UPDATE must not bump status_changed_at — the coordinator's
        // last_activity keeps reporting the real event (the reader's Error).
        assert_eq!(current_changed_at(&conn, id), before);
    }

    /// Full race: Pending → Spawning → reader's early-exit Error → promotion
    /// is a no-op → final: error (#654).
    #[test]
    fn race_reader_wins_error_suppresses_orchestrator_running() {
        let conn = conn_with_agent_nodes();
        let id = insert_node(&conn, "pending");

        let applied = update_agent_node_status_if_inner(
            &conn,
            id,
            SessionStatus::Spawning,
            SessionStatus::Pending,
        )
        .unwrap();
        assert!(applied);
        assert_eq!(current_status(&conn, id), "spawning");

        update_unconditional_inner(&conn, id, SessionStatus::Error).unwrap();
        assert_eq!(current_status(&conn, id), "error");

        let applied = update_agent_node_status_if_inner(
            &conn,
            id,
            SessionStatus::Running,
            SessionStatus::Spawning,
        )
        .unwrap();
        assert!(!applied, "delayed Running promotion must skip");
        assert_eq!(current_status(&conn, id), "error");
    }

    /// Healthy spawn: Pending → Spawning → promotion applies → final: Running.
    #[test]
    fn race_orchestrator_wins_running_promotes_after_window() {
        let conn = conn_with_agent_nodes();
        let id = insert_node(&conn, "pending");

        let applied = update_agent_node_status_if_inner(
            &conn,
            id,
            SessionStatus::Spawning,
            SessionStatus::Pending,
        )
        .unwrap();
        assert!(applied);

        let applied = update_agent_node_status_if_inner(
            &conn,
            id,
            SessionStatus::Running,
            SessionStatus::Spawning,
        )
        .unwrap();
        assert!(applied, "promotion applies when status is still Spawning");
        assert_eq!(current_status(&conn, id), "running");
    }

    /// `archive_agent_node` and the reader's Idle branch legitimately need
    /// to overwrite any status — the unconditional sibling must stay unconditional.
    #[test]
    fn unconditional_update_still_overwrites_any_status() {
        let conn = conn_with_agent_nodes();
        let id = insert_node(&conn, "archived");

        update_unconditional_inner(&conn, id, SessionStatus::Running).unwrap();

        assert_eq!(current_status(&conn, id), "running");
    }

    // Symmetric race (Angle B code-review regression #654): the
    // orchestrator's Spawning write must not resurrect a reader-written Error.

    #[test]
    fn orchestrator_spawning_write_applies_on_fresh_pending() {
        let conn = conn_with_agent_nodes();
        let id = insert_node(&conn, "pending");

        let applied = update_agent_node_status_unless_in_inner(
            &conn,
            id,
            SessionStatus::Spawning,
            &[SessionStatus::Error, SessionStatus::Archived],
        )
        .unwrap();

        assert!(applied);
        assert_eq!(current_status(&conn, id), "spawning");
    }

    #[test]
    fn orchestrator_spawning_write_skipped_when_reader_already_wrote_error() {
        let conn = conn_with_agent_nodes();
        let id = insert_node(&conn, "error");

        let applied = update_agent_node_status_unless_in_inner(
            &conn,
            id,
            SessionStatus::Spawning,
            &[SessionStatus::Error, SessionStatus::Archived],
        )
        .unwrap();

        assert!(!applied);
        assert_eq!(current_status(&conn, id), "error");

        // Promotion also no-ops because status is not Spawning.
        let applied = update_agent_node_status_if_inner(
            &conn,
            id,
            SessionStatus::Running,
            SessionStatus::Spawning,
        )
        .unwrap();
        assert!(!applied, "promotion sees Error (not Spawning) and bails");
        assert_eq!(current_status(&conn, id), "error");
    }

    #[test]
    fn orchestrator_spawning_write_skipped_when_row_is_archived() {
        let conn = conn_with_agent_nodes();
        let id = insert_node(&conn, "archived");

        let applied = update_agent_node_status_unless_in_inner(
            &conn,
            id,
            SessionStatus::Spawning,
            &[SessionStatus::Error, SessionStatus::Archived],
        )
        .unwrap();

        assert!(!applied);
        assert_eq!(current_status(&conn, id), "archived");
    }

    #[test]
    fn empty_forbidden_list_is_rejected() {
        let conn = conn_with_agent_nodes();
        let id = insert_node(&conn, "idle");

        let result = update_agent_node_status_unless_in_inner(
            &conn,
            id,
            SessionStatus::Spawning,
            &[],
        );

        assert!(
            result.is_err(),
            "empty forbidden list must error to keep the surface disjoint from update_agent_node_status_inner",
        );
    }

    // ===== Issue #1746 — `update_agent_node_positions_batch` bulk-shape contract =====
    //
    // These tests mirror the mesh.rs set: 0/1/many rows, per-row baseline
    // agreement, and an `EXPLAIN QUERY PLAN` check that the bulk UPDATE
    // still drives the INTEGER PRIMARY KEY lookup. See
    // `db::mesh::update_mesh_positions_batch_inner` for the rationale.

    fn conn_with_agent_nodes_full() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                mesh_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                path TEXT NOT NULL,
                position INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL DEFAULT 'idle'
            );",
        )
        .unwrap();
        conn
    }

    fn insert_agent_node(conn: &Connection) -> i64 {
        let n = uuid::Uuid::new_v4();
        conn.execute(
            "INSERT INTO agent_nodes (mesh_id, name, path) VALUES (?1, ?2, ?3)",
            rusqlite::params![1_i64, format!("node-{n}"), format!("/tmp/{n}")],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn read_node_position(conn: &Connection, id: i64) -> i64 {
        conn.query_row(
            "SELECT position FROM agent_nodes WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn update_agent_node_positions_batch_inner_zero_rows_is_noop() {
        let conn = conn_with_agent_nodes_full();
        let id = insert_agent_node(&conn);
        assert_eq!(read_node_position(&conn, id), 0);

        crate::db::update_agent_node_positions_batch_inner(&conn, &[]).unwrap();

        assert_eq!(read_node_position(&conn, id), 0);
    }

    #[test]
    fn update_agent_node_positions_batch_inner_single_row() {
        let conn = conn_with_agent_nodes_full();
        let a = insert_agent_node(&conn);
        let b = insert_agent_node(&conn);
        let c = insert_agent_node(&conn);

        crate::db::update_agent_node_positions_batch_inner(&conn, &[(b, 99)]).unwrap();

        assert_eq!(read_node_position(&conn, a), 0);
        assert_eq!(read_node_position(&conn, b), 99);
        assert_eq!(read_node_position(&conn, c), 0);
    }

    #[test]
    fn update_agent_node_positions_batch_inner_six_hundred_rows_spans_chunks() {
        let conn = conn_with_agent_nodes_full();
        let mut ids = Vec::with_capacity(600);
        for _ in 0..600 {
            ids.push(insert_agent_node(&conn));
        }
        let updates: Vec<(i64, i64)> =
            ids.iter().enumerate().map(|(i, id)| (*id, (i + 1) as i64)).collect();

        crate::db::update_agent_node_positions_batch_inner(&conn, &updates).unwrap();

        for (i, id) in ids.iter().enumerate() {
            assert_eq!(read_node_position(&conn, *id), (i + 1) as i64);
        }
    }

    #[test]
    fn update_agent_node_positions_batch_inner_matches_per_row_baseline() {
        let conn_new = conn_with_agent_nodes_full();
        let mut ids_new = Vec::with_capacity(75);
        for _ in 0..75 {
            ids_new.push(insert_agent_node(&conn_new));
        }
        let updates: Vec<(i64, i64)> = ids_new
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, ((i + 3) * 11) as i64))
            .collect();
        crate::db::update_agent_node_positions_batch_inner(&conn_new, &updates).unwrap();

        let conn_old = conn_with_agent_nodes_full();
        let mut ids_old = Vec::with_capacity(75);
        for _ in 0..75 {
            ids_old.push(insert_agent_node(&conn_old));
        }
        let tx = conn_old.unchecked_transaction().unwrap();
        for (id, pos) in &updates {
            tx.execute(
                "UPDATE agent_nodes SET position = ?1 WHERE id = ?2",
                rusqlite::params![pos, id],
            )
            .unwrap();
        }
        tx.commit().unwrap();

        let positions_new: Vec<i64> = ids_new
            .iter()
            .map(|id| read_node_position(&conn_new, *id))
            .collect();
        let positions_old: Vec<i64> = ids_old
            .iter()
            .map(|id| read_node_position(&conn_old, *id))
            .collect();
        assert_eq!(positions_new, positions_old);
    }

    #[test]
    fn update_agent_node_positions_batch_inner_query_plan_uses_primary_key() {
        let conn = conn_with_agent_nodes_full();
        for _ in 0..10 {
            insert_agent_node(&conn);
        }
        let mut stmt = conn
            .prepare("EXPLAIN QUERY PLAN UPDATE agent_nodes SET position = CASE id WHEN ?1 THEN ?2 WHEN ?3 THEN ?4 END WHERE id IN (?5, ?6)")
            .unwrap();
        let plan = stmt
            .query_map(
                rusqlite::params![1_i64, 10_i64, 2_i64, 20_i64, 1_i64, 2_i64],
                |row| row.get::<_, String>(3),
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            plan.iter().any(|line| line.contains("PRIMARY KEY") || line.contains("rowid")),
            "bulk UPDATE must use the INTEGER PRIMARY KEY index, got plan: {plan:?}"
        );
    }

    /// All-or-nothing: same contract as the mesh position batch — see
    /// `update_mesh_positions_batch_inner_rolls_back_on_trigger_error`
    /// for the rationale. Mirrored here because the two helpers are
    /// intentional twins of the same hot path (drag-to-reorder).
    #[test]
    fn update_agent_node_positions_batch_inner_rolls_back_on_trigger_error() {
        let conn = conn_with_agent_nodes_full();
        let keep = insert_agent_node(&conn);
        let sentinel = insert_agent_node(&conn);
        assert_eq!(read_node_position(&conn, keep), 0);
        assert_eq!(read_node_position(&conn, sentinel), 0);

        conn.execute_batch(&format!(
            "CREATE TRIGGER abort_on_sentinel BEFORE UPDATE ON agent_nodes \
             WHEN OLD.id = {sentinel} \
             BEGIN SELECT RAISE(ABORT, 'sentinel'); END;"
        )).unwrap();

        let err = crate::db::update_agent_node_positions_batch_inner(
            &conn,
            &[(keep, 100), (sentinel, 200)],
        )
        .expect_err("trigger must abort the update");
        assert!(
            err.to_string().contains("sentinel"),
            "error must surface the trigger's message, got {err}"
        );

        assert_eq!(read_node_position(&conn, keep), 0);
        assert_eq!(read_node_position(&conn, sentinel), 0);
    }
}
