//! Tests for the per-mesh `sandbox` DB helper (`set_mesh_sandbox_inner`) and
//! the v18 `sandbox` column add (the column backs both the macOS Seatbelt
//! #497 and Windows AppContainer #498 toggles). The safety-net side of the
//! migration lives in `db::migrations` (issue #249); the test calls
//! `migrations::evolve_to` against a pre-v18 schema to exercise that path.
//!
//! Uses an in-memory SQLite connection rather than the global `DB` OnceCell,
//! mirroring `scratchpad_tests`, so the whole suite can run as one
//! `cargo test` invocation without `--test-threads=1`.

#[cfg(test)]
mod tests {
    use crate::db;
    use rusqlite::Connection;

    /// Fresh in-memory schema with one mesh row, including the `sandbox`
    /// column with its production default (0 = sandbox off).
    fn fresh_db_with_mesh() -> (Connection, i64) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE meshes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                layout TEXT NOT NULL DEFAULT 'grid',
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                use_worktree INTEGER NOT NULL DEFAULT 1,
                sandbox INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO meshes (name, path) VALUES ('test', '/tmp/sandbox-test');",
        )
        .unwrap();
        let id: i64 = conn
            .query_row("SELECT id FROM meshes WHERE name = 'test'", [], |row| {
                row.get(0)
            })
            .unwrap();
        (conn, id)
    }

    /// Read the persisted flag back. Production reads flow through `MeshRow`
    /// (via `get_mesh_by_id` / `map_mesh_row`), so there's no dedicated getter
    /// to assert against yet — the native spawn path adds one when it consumes
    /// the flag. A local query keeps these tests honest without introducing
    /// unused production code.
    fn read(conn: &Connection, id: i64) -> bool {
        conn.query_row(
            "SELECT COALESCE(sandbox, 0) FROM meshes WHERE id = ?1",
            [id],
            |row| row.get::<_, i32>(0).map(|v| v != 0),
        )
        .unwrap()
    }

    #[test]
    fn defaults_to_false_for_freshly_created_mesh() {
        // Sandbox is off by default until the native spawn path lands and is
        // validated. A fresh mesh must read back `false`.
        let (conn, id) = fresh_db_with_mesh();
        assert!(!read(&conn, id));
    }

    #[test]
    fn set_true_then_get_round_trips() {
        let (conn, id) = fresh_db_with_mesh();
        db::set_mesh_sandbox_inner(&conn, id, true).unwrap();
        assert!(read(&conn, id));
    }

    #[test]
    fn set_false_clears_the_flag() {
        let (conn, id) = fresh_db_with_mesh();
        db::set_mesh_sandbox_inner(&conn, id, true).unwrap();
        db::set_mesh_sandbox_inner(&conn, id, false).unwrap();
        assert!(!read(&conn, id));
    }

    #[test]
    fn set_for_unknown_mesh_id_errors() {
        // A write that matches zero rows is an error, so a save that fires
        // after the mesh was deleted doesn't silently report success.
        let (conn, _id) = fresh_db_with_mesh();
        let result = db::set_mesh_sandbox_inner(&conn, 999_999, true);
        assert!(
            matches!(result, Err(rusqlite::Error::QueryReturnedNoRows)),
            "set against unknown mesh id should error, got: {:?}",
            result
        );
    }

    #[test]
    fn ensure_mesh_sandbox_adds_column_and_defaults_existing_rows_to_false() {
        // Simulate an upgrade from a schema without the column: the safety
        // net must add it and pre-existing rows must read back `false`
        // (deny-by-default would be a surprising flip for existing meshes).
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE meshes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                layout TEXT NOT NULL DEFAULT 'grid',
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
             );
             INSERT INTO meshes (name, path) VALUES ('legacy', '/tmp/legacy-sandbox');",
        )
        .unwrap();

        db::migrations::evolve_to(db::migrations::SCHEMA_VERSION, &conn).unwrap();

        let col_present: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('meshes') WHERE name = 'sandbox'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(col_present, "sandbox column should be added on first call");

        let legacy_id: i64 = conn
            .query_row("SELECT id FROM meshes WHERE name = 'legacy'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(
            !read(&conn, legacy_id),
            "existing rows must default to sandbox-off, not flip on"
        );

        // Second call is a no-op.
        db::migrations::evolve_to(db::migrations::SCHEMA_VERSION, &conn).unwrap();
    }
}
