//! Tests for agent_node lifecycle helpers, specifically the conditional
//! `update_agent_node_status_if` that closes the race window between the
//! orchestrator's late Running write and the PTY reader thread's early-exit
//! Error write (issue #654 — "post-spawn status + early-exit race").
//!
//! Background: the PTY reader thread fires its early-exit Error write within
//! 3 seconds of process creation if the agent CLI exits immediately (typically
//! a stale `--resume <uuid>`). The orchestrator writes `status = Running`
//! after `start_reader` returns. Whichever write lands last wins, so a node
//! can be left in `Running` state with no live process — a "ghost node".
//!
//! The fix introduces a `Spawning` intermediate status and two conditional
//! updates:
//! - `update_agent_node_status_if(Running, Spawning)` — the orchestrator's
//!   delayed promotion. Only fires if the row is still in `Spawning`.
//! - `update_agent_node_status_unless_in(Spawning, [Error, Archived])` —
//!   the orchestrator's transient-state write. Skipped if the reader has
//!   already written `Error` (symmetric race: reader wins → orchestrator
//!   must NOT resurrect Error back to Spawning).
//!
//! Run with: cargo test --package buildmesh --lib db::agent_node_tests

#[cfg(test)]
mod tests {
    use crate::db::{
        update_agent_node_status_if_inner, update_agent_node_status_inner as update_unconditional_inner,
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
                status_changed_at TEXT NOT NULL DEFAULT (datetime('now'))
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

    /// Race-fix core: when the reader thread has already written `error`,
    /// the orchestrator's delayed Running promotion must be a no-op.
    /// Without this guard the orchestrator would clobber the reader's
    /// `error` write with `running`, leaving a "ghost Running" node with
    /// no live process (issue #654).
    #[test]
    fn update_status_if_noop_when_reader_already_wrote_error() {
        let conn = conn_with_agent_nodes();
        // Simulate the race ordering: reader thread won the race.
        let id = insert_node(&conn, "error");
        let before = current_changed_at(&conn, id);

        std::thread::sleep(std::time::Duration::from_millis(5));

        let applied = update_agent_node_status_if_inner(
            &conn,
            id,
            SessionStatus::Running,
            SessionStatus::Spawning, // expected is "still spawning" — it isn't
        )
        .unwrap();

        assert!(
            !applied,
            "the update must be a no-op when expected mismatches",
        );
        assert_eq!(
            current_status(&conn, id),
            "error",
            "the reader's Error write must survive the orchestrator's promotion attempt",
        );
        assert_eq!(
            current_changed_at(&conn, id),
            before,
            "status_changed_at must NOT change when the UPDATE is a no-op \
             (the coordinator digest's last_activity must keep reporting \
             the reader's Error, not a phantom orchestrator activity)",
        );
    }

    /// Full race scenario: orchestrator writes Spawning → reader writes
    /// Error before the 3-second window elapses → delayed Running promotion
    /// fires. Final state must be `error`, not `running` (issue #654).
    ///
    /// We model the reader's Error write as a raw SQL UPDATE because the
    /// unconditional `update_agent_node_status` would write to the global
    /// OnceCell<Mutex<Connection>>, not our in-memory fixture.
    #[test]
    fn race_reader_wins_error_suppresses_orchestrator_running() {
        let conn = conn_with_agent_nodes();
        let id = insert_node(&conn, "pending");

        // Orchestrator stage-2 completed: status flips to Spawning.
        let applied = update_agent_node_status_if_inner(
            &conn,
            id,
            SessionStatus::Spawning,
            SessionStatus::Pending,
        )
        .unwrap();
        assert!(applied);
        assert_eq!(current_status(&conn, id), "spawning");

        // PTY reader's early-exit (< EARLY_EXIT_WINDOW) fires its Error write
        // via the production SQL (unconditional `_inner` form) — keeping the
        // test in lock-step with the reader thread's actual write path.
        update_unconditional_inner(&conn, id, SessionStatus::Error).unwrap();
        assert_eq!(current_status(&conn, id), "error");

        // Delayed Running promotion fires after the 3-second window — must
        // be a no-op because status is no longer Spawning.
        let applied = update_agent_node_status_if_inner(
            &conn,
            id,
            SessionStatus::Running,
            SessionStatus::Spawning,
        )
        .unwrap();
        assert!(
            !applied,
            "delayed Running promotion must skip when status is no longer Spawning",
        );
        assert_eq!(
            current_status(&conn, id),
            "error",
            "issue #654: ghost-Running node must not appear after the reader's early-exit",
        );
    }

    /// Healthy spawn: orchestrator writes Spawning → agent survives past 3s
    /// → delayed Running promotion fires → status becomes Running. The
    /// conditional update applies because the row is still in Spawning.
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

        // No early-exit from the reader. The agent is still alive.
        let applied = update_agent_node_status_if_inner(
            &conn,
            id,
            SessionStatus::Running,
            SessionStatus::Spawning,
        )
        .unwrap();
        assert!(
            applied,
            "the delayed Running promotion must apply when status is still Spawning",
        );
        assert_eq!(current_status(&conn, id), "running");
    }

    /// Existing unconditional `update_agent_node_status` path is unchanged
    /// (still bypasses the conditional check). This pins the contract for
    /// callers like `archive_agent_node` that legitimately need to overwrite
    /// any status.
    #[test]
    fn unconditional_update_still_overwrites_any_status() {
        let conn = conn_with_agent_nodes();
        let id = insert_node(&conn, "archived");

        // Production unconditional write path — exercises the actual SQL +
        // RFC3339 stamp used by the reader thread and `archive_agent_node`.
        update_unconditional_inner(&conn, id, SessionStatus::Running).unwrap();

        assert_eq!(
            current_status(&conn, id),
            "running",
            "unconditional UPDATE overwrites any prior status (archive, error, idle, …)",
        );
    }

    // -----------------------------------------------------------------
    // Symmetric race: the orchestrator's Spawning write must NOT resurrect
    // a reader-written Error (issue #654 code review). If the reader
    // thread's early-exit Error write lands BEFORE the orchestrator's
    // Spawning write, the orchestrator must be a no-op — otherwise the
    // delayed Running promotion would later see Spawning and write
    // Running, recreating the original ghost-Running bug.
    // -----------------------------------------------------------------

    /// Happy-path: orchestrator writes Spawning on a fresh Pending row.
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

        assert!(applied, "Pending is not in the forbidden set — write applies");
        assert_eq!(current_status(&conn, id), "spawning");
    }

    /// Reader wins: orchestrator's Spawning write is suppressed because
    /// the row is already `error`. The promotion that fires 3s later sees
    /// `status != Spawning` and is also a no-op, so the node correctly
    /// stays `error` end-to-end.
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

        assert!(
            !applied,
            "Error IS in the forbidden set — write must be a no-op",
        );
        assert_eq!(
            current_status(&conn, id),
            "error",
            "the reader's Error must survive the orchestrator's Spawning attempt",
        );

        // Promotion also no-ops because status is not Spawning.
        let applied = update_agent_node_status_if_inner(
            &conn,
            id,
            SessionStatus::Running,
            SessionStatus::Spawning,
        )
        .unwrap();
        assert!(!applied, "promotion sees Error (not Spawning) and bails");
        assert_eq!(
            current_status(&conn, id),
            "error",
            "issue #654 symmetric race: ghost-Running node must not appear",
        );
    }

    /// Archived is also a terminal state — no automatic transition should
    /// resurrect it. (User explicitly archived the node; spawning it
    /// back is its own deliberate command, not a side effect of the race
    /// fix.)
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

        assert!(!applied, "Archived IS in the forbidden set");
        assert_eq!(current_status(&conn, id), "archived");
    }

    /// Empty forbidden set is rejected at the call site (would otherwise
    /// make the write unconditional, which is exactly what
    /// `update_agent_node_status_inner` already does — keep the two
    /// primitives disjoint).
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
            "calling with an empty forbidden list must error to keep the \
             API surface disjoint from update_agent_node_status_inner",
        );
    }
}