//! DB schema migration tests
//!
//! Tests that verify incremental migrations add columns without data loss.
//!
//! Run with: cargo test --package buildmesh --lib db::tests::migration

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, Result as SqlResult};
    use std::sync::Mutex;

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

            CREATE TABLE IF NOT EXISTS checkpoints (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id INTEGER NOT NULL REFERENCES agent_nodes(id),
                git_ref TEXT NOT NULL,
                turn_index INTEGER NOT NULL,
                message TEXT,
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
