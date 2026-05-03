//! Integration tests for the real migrate_if_needed implementation.
//!
//! These tests call the ACTUAL migrate_if_needed() function from db::mod
//! against an in-memory connection with a v2 schema to verify it handles
//! incremental columns correctly (or expose the current DROP-based bug).
//!
//! Run with: cargo test --package buildmesh --lib db::tests

#[cfg(test)]
mod tests {
    use crate::db::test_migrate_if_needed;
    use crate::models::Mesh;

    /// Sets up an in-memory v2 schema (before layout column) directly,
    /// then calls the real migrate_if_needed to simulate what happens
    /// when the app starts with an old DB on disk.
    #[test]
    fn test_migrate_if_needed_with_v2_schema_has_no_layout_column() {
        // Arrange: in-memory connection with v2 schema
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO app_settings (key, value) VALUES ('schema_version', '2');
            CREATE TABLE IF NOT EXISTS projects (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            "
        ).unwrap();

        // Verify layout column does NOT exist
        let has_layout_before: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('projects') WHERE name = 'layout'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert!(!has_layout_before, "PRECONDITION: layout column must not exist in v2 schema");

        // Insert a project before migration (should survive)
        conn.execute(
            "INSERT INTO projects (name, path) VALUES ('existing-project', '/tmp/existing')",
            [],
        ).unwrap();

        // Act: call the REAL migrate_if_needed (our exported test helper)
        let result = test_migrate_if_needed(&conn);
        assert!(result.is_ok(), "migrate_if_needed should not error");

        // Assert: layout column now exists
        let has_layout_after: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('projects') WHERE name = 'layout'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert!(has_layout_after, "layout column must exist after migration");

        // Assert: existing project survived migration with 'grid' default
        let layout: String = conn.query_row(
            "SELECT layout FROM projects WHERE name = 'existing-project'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(layout, "grid", "existing project should get 'grid' default after migration");
    }

    /// Verify idempotency: running migration twice does not error
    #[test]
    fn test_migrate_if_needed_is_idempotent() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO app_settings (key, value) VALUES ('schema_version', '2');
            CREATE TABLE IF NOT EXISTS projects (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            "
        ).unwrap();

        test_migrate_if_needed(&conn).unwrap();
        test_migrate_if_needed(&conn).unwrap(); // should not panic or error

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM projects",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 0, "no projects should exist after double migration");
    }
}
