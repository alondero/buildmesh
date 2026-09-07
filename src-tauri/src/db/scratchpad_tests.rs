//! Tests for the scratch pad DB helpers (`get_mesh_scratchpad_inner`,
//! `set_mesh_scratchpad`) and the v17 `ensure_mesh_scratchpad` safety net.
//!
//! Uses an in-memory SQLite connection rather than the global `DB`
//! OnceCell, mirroring `migration_tests` and `tests`. The OnceCell
//! can only be set once per process, so any test file that calls
//! `db::init` (e.g. `mesh_tests`) has to be run in isolation with
//! `--test-threads=1` — this file deliberately doesn't, so the whole
//! suite can run as one `cargo test` invocation.

#[cfg(test)]
mod tests {
    use crate::db;
    use rusqlite::Connection;

    /// Build a fresh in-memory schema with a single test mesh row
    /// already inserted. Returns the open connection and the mesh's
    /// primary key so each test can exercise a specific scenario
    /// without re-walking the schema setup.
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
                build_command TEXT,
                run_command TEXT,
                model TEXT,
                effort TEXT,
                use_worktree INTEGER NOT NULL DEFAULT 1,
                worktree_mode TEXT,
                default_provider TEXT,
                base_ref TEXT NOT NULL DEFAULT 'origin/main',
                scratchpad TEXT NOT NULL DEFAULT ''
             );
             INSERT INTO meshes (name, path) VALUES ('test', '/tmp/scratch-test');",
        )
        .unwrap();
        let id: i64 = conn
            .query_row("SELECT id FROM meshes WHERE name = 'test'", [], |row| {
                row.get(0)
            })
            .unwrap();
        (conn, id)
    }

    #[test]
    fn get_returns_empty_string_for_freshly_created_mesh() {
        // The "no notes yet" case — a fresh mesh must read back as the
        // empty string, not error. The Probe tab mounts into this state
        // on first open.
        let (conn, id) = fresh_db_with_mesh();
        let value = db::get_mesh_scratchpad_inner(&conn, id).unwrap();
        assert_eq!(value, "");
    }

    #[test]
    fn set_then_get_round_trips() {
        let (conn, id) = fresh_db_with_mesh();

        db::set_mesh_scratchpad_inner(&conn, id, "remember to refactor spawn.rs").unwrap();

        assert_eq!(
            db::get_mesh_scratchpad_inner(&conn, id).unwrap(),
            "remember to refactor spawn.rs"
        );
    }

    #[test]
    fn set_overwrites_previous_value() {
        let (conn, id) = fresh_db_with_mesh();

        db::set_mesh_scratchpad_inner(&conn, id, "first").unwrap();
        db::set_mesh_scratchpad_inner(&conn, id, "second").unwrap();

        assert_eq!(db::get_mesh_scratchpad_inner(&conn, id).unwrap(), "second");
    }

    #[test]
    fn set_to_empty_string_is_a_normal_value_not_a_deletion() {
        let (conn, id) = fresh_db_with_mesh();

        // Empty string == cleared notes, not "row missing". The next
        // get must succeed and return `""`.
        db::set_mesh_scratchpad_inner(&conn, id, "something").unwrap();
        db::set_mesh_scratchpad_inner(&conn, id, "").unwrap();

        assert_eq!(db::get_mesh_scratchpad_inner(&conn, id).unwrap(), "");
    }

    #[test]
    fn preserves_multiline_and_unicode_content() {
        // The scratch pad is "type whatever you want" — so newlines,
        // long strings, and non-ASCII must all round-trip verbatim.
        let (conn, id) = fresh_db_with_mesh();

        let payload = "line 1\nline 2\n\nemoji: 🦀\nboxed: ▣ ▤ ▥";
        db::set_mesh_scratchpad_inner(&conn, id, payload).unwrap();

        assert_eq!(db::get_mesh_scratchpad_inner(&conn, id).unwrap(), payload);
    }

    #[test]
    fn get_for_unknown_mesh_id_errors() {
        // The inner helper returns `Err` for a missing id — the public
        // Tauri command translates that into an empty editor state, but
        // the helper itself is honest about the row not existing.
        let (conn, _id) = fresh_db_with_mesh();
        let result = db::get_mesh_scratchpad_inner(&conn, 999_999);
        assert!(
            result.is_err(),
            "unknown mesh id should error, not silently return empty"
        );
    }

    #[test]
    fn set_for_unknown_mesh_id_errors() {
        // The row-count check on `set_mesh_scratchpad_inner` mirrors the
        // production safety net: a write that matches zero rows is an
        // error, so a debounced save that fires after the mesh was
        // deleted doesn't silently report "Saved" to the user.
        let (conn, _id) = fresh_db_with_mesh();
        let result = db::set_mesh_scratchpad_inner(&conn, 999_999, "ghost write");
        assert!(
            matches!(result, Err(rusqlite::Error::QueryReturnedNoRows)),
            "set against unknown mesh id should error, got: {:?}",
            result
        );
    }

    #[test]
    fn evolve_to_adds_scratchpad_column_on_a_pre_v17_schema() {
        // Simulate an upgrade: a v16 schema (no scratchpad column)
        // opened by `evolve_to` should add the column and a second
        // call should be a no-op. Mirrors the "existing DB upgrade"
        // path the app actually takes.
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
             INSERT INTO meshes (name, path) VALUES ('legacy', '/tmp/legacy');",
        )
        .unwrap();

        // First call: column added.
        db::migrations::evolve_to(db::migrations::SCHEMA_VERSION, &conn).unwrap();
        let col_present: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('meshes') WHERE name = 'scratchpad'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            col_present,
            "scratchpad column should be added on first call"
        );

        // Writing into the new column must succeed and round-trip.
        let legacy_id: i64 = conn
            .query_row("SELECT id FROM meshes WHERE name = 'legacy'", [], |row| {
                row.get(0)
            })
            .unwrap();
        conn.execute(
            "UPDATE meshes SET scratchpad = 'upgraded' WHERE id = ?1",
            rusqlite::params![legacy_id],
        )
        .unwrap();
        let stored: String = conn
            .query_row(
                "SELECT scratchpad FROM meshes WHERE id = ?1",
                rusqlite::params![legacy_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "upgraded");

        // Second call must be a no-op (the helper returns Ok even if
        // the column already exists; the boolean just flips false).
        db::migrations::evolve_to(db::migrations::SCHEMA_VERSION, &conn).unwrap();
    }
}
