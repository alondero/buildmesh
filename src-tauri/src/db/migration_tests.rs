//! DB schema migration tests
//!
//! Tests that verify incremental migrations add columns without data loss.
//!
//! Run with: cargo test --package buildmesh --lib db::tests::migration

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, Result as SqlResult};

    /// Creates a v2 schema (before layout column) for migration testing.
    /// This simulates an existing DB that needs migration to v3/v4+.
    fn create_v2_schema(conn: &Connection) -> SqlResult<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS app_settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT INTO app_settings (key, value) VALUES ('schema_version', '2');

            CREATE TABLE IF NOT EXISTS meshes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS agent_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                mesh_id INTEGER NOT NULL REFERENCES meshes(id),
                name TEXT NOT NULL,
                path TEXT NOT NULL,
                branch TEXT NOT NULL DEFAULT 'main',
                env TEXT NOT NULL DEFAULT 'windows',
                provider TEXT NOT NULL DEFAULT 'anthropic',
                status TEXT NOT NULL DEFAULT 'idle',
                cli_session_id TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE INDEX IF NOT EXISTS idx_agent_nodes_mesh ON agent_nodes(mesh_id);
            "
        )?;
        Ok(())
    }

    /// Current schema version — must be updated when schema changes
    const SCHEMA_VERSION: i32 = 6;

    /// Incremental migration: adds layout column to projects if missing.
    /// Returns the number of columns added (0 if already migrated).
    fn migrate_projects_layout(conn: &Connection) -> SqlResult<usize> {
        // Check if layout column exists
        let has_layout: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('meshes') WHERE name = 'layout'",
            [],
            |row| row.get(0),
        ).unwrap_or(false);

        if has_layout {
            return Ok(0);
        }

        // Add layout column with default
        conn.execute(
            "ALTER TABLE meshes ADD COLUMN layout TEXT NOT NULL DEFAULT 'grid'",
            [],
        )?;
        Ok(1)
    }

    /// Migrate app_settings version marker
    fn set_schema_version(conn: &Connection, version: i32) -> SqlResult<()> {
        conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('schema_version', ?1)",
            [version.to_string()],
        )?;
        Ok(())
    }

    #[test]
    fn test_migration_adds_layout_column_incrementally() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join(format!(
            "buildmesh_migration_test_{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));

        let conn = Connection::open(&db_path).unwrap();

        // Arrange: create v2 schema (no layout column)
        create_v2_schema(&conn).unwrap();

        // Verify v2: layout column does NOT exist
        let has_layout_before: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('meshes') WHERE name = 'layout'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert!(!has_layout_before, "layout column should NOT exist in v2 schema");

        // Insert a test project before migration
        conn.execute(
            "INSERT INTO meshes (name, path) VALUES ('test-project', '/tmp/test')",
            [],
        ).unwrap();

        // Act: run incremental migration
        migrate_projects_layout(&conn).unwrap();
        set_schema_version(&conn, SCHEMA_VERSION).unwrap();

        // Assert: layout column now exists
        let has_layout_after: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('meshes') WHERE name = 'layout'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert!(has_layout_after, "layout column MUST exist after migration");

        // Assert: existing project got default 'grid' value
        let layout_value: String = conn.query_row(
            "SELECT layout FROM meshes WHERE name = 'test-project'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(layout_value, "grid", "existing project should get default 'grid' layout");

        // Assert: new projects can override the default
        conn.execute(
            "INSERT INTO meshes (name, path, layout) VALUES ('another', '/tmp/another', 'single')",
            [],
        ).unwrap();
        let single_layout: String = conn.query_row(
            "SELECT layout FROM meshes WHERE name = 'another'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(single_layout, "single", "explicit layout value should be respected");

        // Cleanup
        drop(conn);
        std::fs::remove_file(&db_path).ok();
    }

    /// v12→v13: `ensure_agent_node_position` must add the `position` column and
    /// backfill each node's position as its 0-based rank by `created_at` WITHIN
    /// its own mesh — so existing nodes keep the order they already render in
    /// (lists used to sort purely by `created_at ASC`). Ranks restart per mesh.
    #[test]
    fn ensure_agent_node_position_backfills_per_mesh_rank() {
        let conn = Connection::open_in_memory().unwrap();
        // v12-shape agent_nodes: everything except the new `position` column.
        conn.execute_batch(
            "
            CREATE TABLE agent_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                mesh_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                path TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            -- Interleave meshes and insert out of created_at order so the test
            -- proves we rank by created_at, not by insert/id order.
            INSERT INTO agent_nodes (mesh_id, name, path, created_at) VALUES
                (1, 'm1-b', '/b', '2020-01-02T00:00:00Z'),
                (2, 'm2-a', '/d', '2020-01-01T00:00:00Z'),
                (1, 'm1-a', '/a', '2020-01-01T00:00:00Z'),
                (1, 'm1-c', '/c', '2020-01-03T00:00:00Z'),
                (2, 'm2-b', '/e', '2020-01-02T00:00:00Z');
            "
        ).unwrap();

        // Precondition: no position column yet.
        let has_before: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'position'",
            [], |row| row.get(0),
        ).unwrap();
        assert!(!has_before, "PRECONDITION: position column must not exist in v12 schema");

        // Act
        crate::db::ensure_agent_node_position(&conn).unwrap();

        // Assert: column exists and ranks are per-mesh, ordered by created_at.
        let pos = |name: &str| -> i64 {
            conn.query_row(
                "SELECT position FROM agent_nodes WHERE name = ?1",
                [name], |row| row.get(0),
            ).unwrap()
        };
        assert_eq!(pos("m1-a"), 0, "mesh 1, earliest");
        assert_eq!(pos("m1-b"), 1, "mesh 1, middle");
        assert_eq!(pos("m1-c"), 2, "mesh 1, latest");
        assert_eq!(pos("m2-a"), 0, "mesh 2 ranks restart at 0");
        assert_eq!(pos("m2-b"), 1, "mesh 2, second");

        // Idempotent: a second call (column present) must not renumber anything.
        crate::db::ensure_agent_node_position(&conn).unwrap();
        assert_eq!(pos("m1-c"), 2, "second call must be a no-op");
    }

    #[test]
    fn test_migration_is_idempotent_when_column_exists() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join(format!(
            "buildmesh_migration_idempotent_{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));

        let conn = Connection::open(&db_path).unwrap();

        // Arrange: create schema directly with layout column (already migrated)
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS meshes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                layout TEXT NOT NULL DEFAULT 'grid',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            "
        ).unwrap();

        // Act: run migration twice
        let first = migrate_projects_layout(&conn).unwrap();
        let second = migrate_projects_layout(&conn).unwrap();

        // Assert: both calls report 0 columns added
        assert_eq!(first, 0, "should report 0 on already-migrated schema");
        assert_eq!(second, 0, "second call should also report 0 (idempotent)");

        drop(conn);
        std::fs::remove_file(&db_path).ok();
    }
}
