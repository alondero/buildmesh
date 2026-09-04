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
}
