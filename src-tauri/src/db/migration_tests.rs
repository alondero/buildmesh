//! DB schema migration tests
//!
//! Tests that verify incremental migrations add columns without data loss.
//!
//! Run with: cargo test --package buildmesh --lib db::tests::migration

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, Result as SqlResult};

    #[test]
    fn review_open_pr_policy_migration_is_persistent_and_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::ensure_baseline_tables(&conn).unwrap();
        conn.execute(
            "INSERT INTO meshes (id, name, path) VALUES (1, 'migration-test', 'C:/migration-test')",
            [],
        )
        .unwrap();

        let stale = crate::autopilot::circuit::model::CircuitGraph::issue_driven_autopilot_review(
            "buildmesh:run",
        );
        let mut raw: serde_json::Value = serde_json::from_str(&stale.to_json().unwrap()).unwrap();
        raw["nodes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|node| node["id"] == "open_pr")
            .unwrap()["type"]
            .as_object_mut()
            .unwrap()
            .remove("open_pr_policy");
        let stale_json = serde_json::to_string(&raw).unwrap();

        // An explicitly authored policy must survive the migration unchanged.
        let mut explicit_graph =
            crate::autopilot::circuit::model::CircuitGraph::issue_driven_autopilot_review(
                "buildmesh:other",
            );
        if let Some(node) = explicit_graph
            .nodes
            .iter_mut()
            .find(|node| node.id == "open_pr")
        {
            if let crate::autopilot::circuit::model::CircuitNodeKind::GithubAction {
                open_pr_policy,
                ..
            } = &mut node.kind
            {
                *open_pr_policy =
                    Some(crate::autopilot::circuit::model::OpenPrPolicy::CreateIfMissing);
            }
        }
        let explicit = explicit_graph.to_json().unwrap();
        conn.execute(
            "INSERT INTO autopilot_circuits
             (id, mesh_id, name, graph_json, is_preset)
             VALUES (?1, 1, ?2, ?3, 0)",
            rusqlite::params![1, "stale", stale_json],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO autopilot_circuits
             (id, mesh_id, name, graph_json, is_preset)
             VALUES (?1, 1, ?2, ?3, 0)",
            rusqlite::params![2, "explicit", explicit],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES
             ('schema_version', ?1),
             ('issue_review_first_turn_upgrade_v1', '1')",
            rusqlite::params![crate::db::migrations::SCHEMA_VERSION.to_string()],
        )
        .unwrap();

        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();

        let graph_json = |id: i64| {
            conn.query_row(
                "SELECT graph_json FROM autopilot_circuits WHERE id = ?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
        };
        let migrated =
            crate::autopilot::circuit::model::CircuitGraph::from_json(&graph_json(1)).unwrap();
        assert!(matches!(
            migrated.node("open_pr").map(|node| &node.kind),
            Some(
                crate::autopilot::circuit::model::CircuitNodeKind::GithubAction {
                    open_pr_policy: Some(
                        crate::autopilot::circuit::model::OpenPrPolicy::RequireExisting
                    ),
                    ..
                }
            )
        ));
        let explicit_graph =
            crate::autopilot::circuit::model::CircuitGraph::from_json(&graph_json(2)).unwrap();
        assert!(matches!(
            explicit_graph.node("open_pr").map(|node| &node.kind),
            Some(
                crate::autopilot::circuit::model::CircuitNodeKind::GithubAction {
                    open_pr_policy: Some(
                        crate::autopilot::circuit::model::OpenPrPolicy::CreateIfMissing
                    ),
                    ..
                }
            )
        ));

        let first_json = graph_json(1);
        assert_eq!(
            conn.query_row(
                "SELECT value FROM app_settings WHERE key = 'issue_review_first_turn_upgrade_v2'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "1"
        );
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        assert_eq!(
            graph_json(1),
            first_json,
            "second initializer pass is idempotent"
        );
    }

    fn canonical_index_names(conn: &Connection) -> Vec<String> {
        let mut statement = conn
            .prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'index' AND name GLOB 'idx_*' ORDER BY name",
            )
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<SqlResult<Vec<String>>>()
            .unwrap()
    }

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
            ",
        )?;
        Ok(())
    }

    /// Current schema version — must be updated when schema changes
    const SCHEMA_VERSION: i32 = 6;

    /// Incremental migration: adds layout column to projects if missing.
    /// Returns the number of columns added (0 if already migrated).
    fn migrate_projects_layout(conn: &Connection) -> SqlResult<usize> {
        // Check if layout column exists
        let has_layout: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('meshes') WHERE name = 'layout'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);

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
        let has_layout_before: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('meshes') WHERE name = 'layout'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !has_layout_before,
            "layout column should NOT exist in v2 schema"
        );

        // Insert a test project before migration
        conn.execute(
            "INSERT INTO meshes (name, path) VALUES ('test-project', '/tmp/test')",
            [],
        )
        .unwrap();

        // Act: run incremental migration
        migrate_projects_layout(&conn).unwrap();
        set_schema_version(&conn, SCHEMA_VERSION).unwrap();

        // Assert: layout column now exists
        let has_layout_after: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('meshes') WHERE name = 'layout'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(has_layout_after, "layout column MUST exist after migration");

        // Assert: existing project got default 'grid' value
        let layout_value: String = conn
            .query_row(
                "SELECT layout FROM meshes WHERE name = 'test-project'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            layout_value, "grid",
            "existing project should get default 'grid' layout"
        );

        // Assert: new projects can override the default
        conn.execute(
            "INSERT INTO meshes (name, path, layout) VALUES ('another', '/tmp/another', 'single')",
            [],
        )
        .unwrap();
        let single_layout: String = conn
            .query_row(
                "SELECT layout FROM meshes WHERE name = 'another'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            single_layout, "single",
            "explicit layout value should be respected"
        );

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
            ",
        )
        .unwrap();

        // Precondition: no position column yet.
        let has_before: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'position'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !has_before,
            "PRECONDITION: position column must not exist in v12 schema"
        );

        // Act
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();

        // Assert: column exists and ranks are per-mesh, ordered by created_at.
        let pos = |name: &str| -> i64 {
            conn.query_row(
                "SELECT position FROM agent_nodes WHERE name = ?1",
                [name],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(pos("m1-a"), 0, "mesh 1, earliest");
        assert_eq!(pos("m1-b"), 1, "mesh 1, middle");
        assert_eq!(pos("m1-c"), 2, "mesh 1, latest");
        assert_eq!(pos("m2-a"), 0, "mesh 2 ranks restart at 0");
        assert_eq!(pos("m2-b"), 1, "mesh 2, second");

        // Idempotent: a second call (column present) must not renumber anything.
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        assert_eq!(pos("m1-c"), 2, "second call must be a no-op");
    }

    /// v15→v16: `ensure_agent_node_source_pr_pinned_sha` must add the new
    /// `source_pr_pinned_sha` column to a v15-shape `agent_nodes` table
    /// (one with `source_pr` but no SHA), and a second call must be a no-op
    /// (idempotent). This is the safety-net path that fixes DBs that skipped
    /// the version-gated migration (see `ensure_agent_node_source_issue`
    /// for the same pattern in v9).
    #[test]
    fn ensure_agent_node_source_pr_pinned_sha_adds_column_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        // v15-shape agent_nodes: has `source_pr` (issue #420) but NOT
        // `source_pr_pinned_sha` (issue #444).
        conn.execute_batch(
            "
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
                use_worktree INTEGER NOT NULL DEFAULT 1,
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                source_issue INTEGER,
                source_pr INTEGER
            );
            -- Pre-existing v15 row that the safety net must not break.
            -- Its `source_pr` is set; the new SHA column must default to NULL
            -- and the row must remain queryable.
            INSERT INTO agent_nodes (mesh_id, name, path, source_pr, created_at)
                VALUES (1, 'preexisting', '/p', 420, '2020-01-01T00:00:00Z');
            ",
        )
        .unwrap();

        // Precondition: column does not exist yet.
        let has_before: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'source_pr_pinned_sha'",
            [], |row| row.get(0),
        ).unwrap();
        assert!(
            !has_before,
            "PRECONDITION: source_pr_pinned_sha must not exist in v15 schema"
        );

        // Act — first call adds the column.
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();

        // Assert: column now exists and is nullable (no NOT NULL constraint).
        let has_after: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'source_pr_pinned_sha'",
            [], |row| row.get(0),
        ).unwrap();
        assert!(
            has_after,
            "source_pr_pinned_sha column must exist after safety-net call"
        );

        let notnull: i64 = conn.query_row(
            "SELECT \"notnull\" FROM pragma_table_info('agent_nodes') WHERE name = 'source_pr_pinned_sha'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(
            notnull, 0,
            "source_pr_pinned_sha must be NULLable — backfill of pre-existing rows is impossible"
        );

        // Pre-existing v15 row survives: source_pr is intact, SHA defaults to NULL.
        let (source_pr, source_pr_pinned_sha): (Option<i64>, Option<String>) = conn.query_row(
            "SELECT source_pr, source_pr_pinned_sha FROM agent_nodes WHERE name = 'preexisting'",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(source_pr, Some(420), "pre-existing source_pr must survive");
        assert_eq!(
            source_pr_pinned_sha, None,
            "new column must default to NULL for v15 rows"
        );

        // A new row can store a SHA. Issue #444 — the SHA is the exact-pinning
        // handle that the spawn path verifies against `origin/<head_ref>`.
        conn.execute(
            "INSERT INTO agent_nodes (mesh_id, name, path, source_pr, source_pr_pinned_sha, created_at)
             VALUES (1, 'pinned', '/p2', 421, '0123456789abcdef0123456789abcdef01234567', '2020-01-02T00:00:00Z')",
            [],
        ).unwrap();
        let sha: Option<String> = conn
            .query_row(
                "SELECT source_pr_pinned_sha FROM agent_nodes WHERE name = 'pinned'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            sha.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );

        // Idempotent: a second call must not error (the column already exists).
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();

        // The pinned row is still queryable after the second call.
        let sha_after: Option<String> = conn
            .query_row(
                "SELECT source_pr_pinned_sha FROM agent_nodes WHERE name = 'pinned'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            sha_after.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567"),
            "second safety-net call must not corrupt existing data"
        );
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
            ",
        )
        .unwrap();

        // Act: run migration twice
        let first = migrate_projects_layout(&conn).unwrap();
        let second = migrate_projects_layout(&conn).unwrap();

        // Assert: both calls report 0 columns added
        assert_eq!(first, 0, "should report 0 on already-migrated schema");
        assert_eq!(second, 0, "second call should also report 0 (idempotent)");

        drop(conn);
        std::fs::remove_file(&db_path).ok();
    }

    /// Issue #456 (post-#249 reformulation): the runner's always-pass
    /// column walk must (a) add a missing column with the requested
    /// type/default, (b) be a no-op on a second call (column already
    /// present), and (c) be a no-op when the table itself is missing
    /// (mirrors the table-exists guard the shared `ensure_column` helper
    /// used to provide). The helper is now private to `db::migrations`;
    /// this test pins the runner-level invariant by exercising a fresh
    /// connection against a non-registry table (so the always-pass walk
    /// can't accidentally mutate it) and a registry table (so the walk
    /// adds the column).
    #[test]
    fn evolve_to_column_walk_is_idempotent_and_table_aware() {
        // --- Case 1: a fresh in-memory connection with a non-registry
        // table. The runner's always-pass walk must NOT touch the table
        // (no entry for it in the registry) and must NOT error on a
        // missing-table situation when its own walks hit a registry entry
        // whose table isn't present.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE widgets (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);",
        )
        .unwrap();

        // Pre-state: only the inline CREATE columns are present.
        let before: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('widgets') ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            before,
            vec!["id".to_string(), "name".to_string()],
            "PRECONDITION: widgets has only its inline columns before evolve_to"
        );

        // Act: call evolve_to. The always-pass walk runs every entry in
        // the column registry; none of them target `widgets`, so the
        // table is untouched. The walk also hits registry entries for
        // tables that don't exist (e.g. `autopilot_runs`) — the
        // `table_present` guard makes those a no-op rather than an error.
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();

        // Assert: `widgets` is still untouched (the runner must not
        // mutate tables that aren't in the registry).
        let after_first: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('widgets') ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            after_first,
            vec!["id".to_string(), "name".to_string()],
            "evolve_to must not mutate tables outside its column registry"
        );

        // --- Case 2: idempotent — a second call must not error and must
        // not change anything. (Every column ALTER is gated on
        // `pragma_table_info`; every backfill on its app_settings flag;
        // every AlwaysStep is naturally idempotent.)
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        let after_second: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('widgets') ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            after_second, after_first,
            "second evolve_to call must be a no-op"
        );

        // Pre-existing semantics on the un-touched table: a fresh INSERT
        // reads back the inserted value (the runner's no-mutation
        // guarantee is end-to-end, not just for the column-projection).
        conn.execute("INSERT INTO widgets (name) VALUES ('alpha')", [])
            .unwrap();
        let name: String = conn
            .query_row("SELECT name FROM widgets WHERE name = 'alpha'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(name, "alpha");

        // --- Case 3: a registry target. Stamp `mesh_id` on `widgets`
        // (a registry column for `meshes`) so we can prove the walk
        // adds it when the table exists — closes the loop with the
        // shared `ensure_column` "added on present table" property the
        // pre-#249 helper covered.
        // `evolve_to` now materialises baseline tables first (#1565), so the
        // earlier call already created a full `meshes`. Drop it and
        // recreate the skinny shape the walk is supposed to thicken.
        conn.execute("DROP TABLE IF EXISTS meshes", []).unwrap();
        conn.execute(
            "CREATE TABLE meshes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE
         );",
            [],
        )
        .unwrap();
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        let meshes_cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('meshes') ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        // Every registry entry for `meshes` must now be present (the
        // fresh-table CREATE above deliberately omitted the
        // registry-only columns so the walk has work to do).
        let registry_cols: Vec<String> = crate::db::migrations::mesh_column_specs()
            .iter()
            .map(|c| c.column.to_string())
            .collect();
        for col in &registry_cols {
            assert!(
                meshes_cols.contains(col),
                "registry column {col} must exist on meshes after evolve_to (got: {:?})",
                meshes_cols
            );
        }
    }

    /// Issue #495 (post-#249 reformulation): a DB created before token
    /// hashing stores the coordinator tokens as 32-char cleartext.
    /// The runner's `AlwaysStep::HashCoordinatorTokens` rehashes them
    /// in place so a DB dump no longer exposes the secret, while the
    /// raw token the user already holds keeps validating. The root
    /// token is deliberately left cleartext (Option 3 — the QR
    /// re-reads its raw value). Re-running must be idempotent (never
    /// hash an already-hashed value).
    #[test]
    fn evolve_to_rehashes_cleartext_coordinator_tokens_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        // Pre-hashing shape: 32-char cleartext tokens.
        let raw_read = "0123456789abcdef0123456789abcdef";
        let raw_drive = "fedcba9876543210fedcba9876543210";
        let raw_root = "aaaabbbbccccddddeeeeffff00001111";
        conn.execute(
            "INSERT INTO app_settings (key, value) VALUES
                ('coordinator_read_token', ?1),
                ('coordinator_drive_token', ?2),
                ('remote_access_token', ?3)",
            rusqlite::params![raw_read, raw_drive, raw_root],
        )
        .unwrap();

        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();

        let stored = |key: &str| -> String {
            conn.query_row(
                "SELECT value FROM app_settings WHERE key = ?1",
                [key],
                |r| r.get(0),
            )
            .unwrap()
        };

        // Both coordinator tokens are now their SHA-256 hash.
        assert_eq!(
            stored("coordinator_read_token"),
            crate::db::hash_token(raw_read)
        );
        assert_eq!(
            stored("coordinator_drive_token"),
            crate::db::hash_token(raw_drive)
        );
        assert_ne!(
            stored("coordinator_read_token"),
            raw_read,
            "cleartext must be gone"
        );

        // The root token is intentionally NOT hashed (Option 3, issue #495).
        assert_eq!(
            stored("remote_access_token"),
            raw_root,
            "root token stays cleartext"
        );

        // The raw token the user already configured still validates after migration.
        crate::db::set_coordinator_api_enabled_inner(&conn, true).unwrap();
        assert!(crate::db::validate_coordinator_read_token_inner(&conn, raw_read).unwrap());

        // Idempotent: a second run must not hash the hash.
        let after_first = stored("coordinator_read_token");
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        assert_eq!(
            stored("coordinator_read_token"),
            after_first,
            "must not double-hash"
        );
    }

    /// LAN exposure (issue #496) must default OFF so a fresh install binds only
    /// loopback, and must round-trip once flipped on/off.
    #[test]
    fn lan_exposure_defaults_off_and_round_trips() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();

        // Unset key → loopback-only default.
        assert!(!crate::db::lan_exposure_enabled_inner(&conn).unwrap());

        crate::db::set_lan_exposure_enabled_inner(&conn, true).unwrap();
        assert!(crate::db::lan_exposure_enabled_inner(&conn).unwrap());

        crate::db::set_lan_exposure_enabled_inner(&conn, false).unwrap();
        assert!(!crate::db::lan_exposure_enabled_inner(&conn).unwrap());
    }

    /// Issue #37 retirement — vestigial-column compat + wire-shape pin.
    ///
    /// A user DB created at v28 carries a `pr_url TEXT` column on
    /// `agent_nodes` that the captured-PR feature used to write. The
    /// retirement strategy is "drop the field, keep the column vestigial"
    /// (no destructive migration; the `agent_nodes.pr_url` column is left
    /// physically on disk and ignored), so a v28-shaped DB must remain
    /// queryable through the post-retirement `AgentNode` projection —
    /// AND the `AgentNode` wire shape must omit `pr_url`, so historical
    /// false-positive URLs cannot leak back to the frontend over IPC.
    ///
    /// Before the retirement, this test fails on the wire-shape assertion
    /// (`AgentNode` still serializes `pr_url: null`). After the retirement,
    /// it pins both invariants:
    ///   - The JOIN through `list_coordinator_node_rows_inner` reads the
    ///     row successfully even though the on-disk table carries a column
    ///     not in the projection.
    ///   - Serializing the returned `AgentNode` does NOT include a
    ///     `pr_url` key.
    #[test]
    fn legacy_v28_pr_url_column_is_ignored_by_agent_node_wire_shape() {
        let conn = Connection::open_in_memory().unwrap();
        // v28-shaped schema, post-v29 safety-net (so the AGENT_NODE_COLUMNS
        // projection has the columns it references). A real v28 DB upgrades
        // to v29 via `ensure_agent_node_is_pinned` on next launch; this
        // fixture mirrors the migrated shape directly. The legacy
        // `pr_url TEXT` column that the captured-PR feature used to write
        // is still present. The projection's read code (which omits
        // `pr_url`) must still succeed: the SELECT lists the projection's
        // columns by name, so a table carrying an extra column simply
        // isn't queried for it.
        conn.execute_batch(
            "
            CREATE TABLE meshes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE agent_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                mesh_id INTEGER NOT NULL REFERENCES meshes(id),
                name TEXT NOT NULL,
                path TEXT NOT NULL,
                branch TEXT NOT NULL DEFAULT 'main',
                env TEXT NOT NULL DEFAULT 'windows',
                provider TEXT NOT NULL DEFAULT 'anthropic',
                status TEXT NOT NULL DEFAULT 'idle',
                cli_session_id TEXT,
                worktree_name TEXT,
                use_worktree INTEGER NOT NULL DEFAULT 1,
                is_pinned INTEGER NOT NULL DEFAULT 0,
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                source_issue INTEGER,
                source_pr INTEGER,
                head_repo_owner TEXT,
                head_repo_clone_url TEXT,
                source_pr_pinned_sha TEXT,
                status_changed_at TEXT,
                signal_health TEXT,
                worktree_path TEXT,
                pr_url TEXT
            );
            INSERT INTO meshes (id, name, path) VALUES (1, 'm', '/m');
            -- Pre-existing row carrying a legacy captured URL — exactly the
            -- false-positive pattern the user reported. The assertion below
            -- proves this value cannot leak across the wire boundary.
            INSERT INTO agent_nodes (mesh_id, name, path, status, pr_url)
                VALUES (1, 'legacy', '/m/n', 'idle',
                        'https://github.com/other-org/other-repo/pull/9999');
            ",
        )
        .unwrap();

        let rows = crate::db::list_coordinator_node_rows_inner(&conn).unwrap();
        assert_eq!(rows.len(), 1, "v28-shaped DB must yield exactly one row");
        let (node, mesh_name, _status_changed_at) = rows.into_iter().next().unwrap();

        assert_eq!(mesh_name, "m", "JOIN-read mesh name must survive");
        assert_eq!(
            node.name, "legacy",
            "row must read under post-retirement projection"
        );
        assert_eq!(node.id, 1, "id column must read");

        // The wire shape must omit `pr_url`, even though the underlying row
        // carries one. This is the regression pin: a future PR that
        // re-adds `pr_url` to `AgentNode` (and re-introduces the false-
        // positive chip) trips this assertion.
        let value = serde_json::to_value(&node).unwrap();
        assert!(
            value.get("pr_url").is_none(),
            "AgentNode wire shape must not expose pr_url (got: {value})"
        );
    }

    /// v28→v29: `migrations::evolve_to` must add the new `is_pinned`
    /// column to a v28-shape `agent_nodes` table via its always-pass
    /// column walk, and a second call must be a no-op (idempotent).
    /// Same structural shape as `test_v8_to_v9_adds_source_issue_via_safety_net`
    /// above — the runner's always-pass column walk replaces every
    /// per-column `ensure_*` wrapper. The v15→v16 test for
    /// `source_pr_pinned_sha` (#444) exercises the same path.
    #[test]
    fn evolve_to_adds_v29_is_pinned_column_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        // v28-shape agent_nodes: everything except the new `is_pinned`
        // column. Mirrors the inline CREATE in db::init up to v28.
        conn.execute_batch(
            "
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
                use_worktree INTEGER NOT NULL DEFAULT 1,
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                source_issue INTEGER,
                source_pr INTEGER,
                head_repo_owner TEXT,
                head_repo_clone_url TEXT,
                source_pr_pinned_sha TEXT
            );
            -- Pre-existing v28 row that the safety net must not break. It
            -- reads back as `is_pinned = false` via the ALTER-added default,
            -- and the user can opt-in row-by-row from the UI (#985).
            INSERT INTO agent_nodes (mesh_id, name, path, created_at)
                VALUES (1, 'preexisting', '/p', '2020-01-01T00:00:00Z');
            ",
        )
        .unwrap();

        // Precondition: column does not exist yet.
        let has_before: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'is_pinned'",
            [], |row| row.get(0),
        ).unwrap();
        assert!(
            !has_before,
            "PRECONDITION: is_pinned must not exist in v28 schema"
        );

        // Act — first call adds the column.
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();

        // Assert: column now exists and is NOT NULL with default 0.
        let has_after: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'is_pinned'",
            [], |row| row.get(0),
        ).unwrap();
        assert!(
            has_after,
            "is_pinned column must exist after safety-net call"
        );

        let notnull: i64 = conn
            .query_row(
                "SELECT \"notnull\" FROM pragma_table_info('agent_nodes') WHERE name = 'is_pinned'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            notnull, 1,
            "is_pinned must be NOT NULL — pinning is a known boolean state"
        );

        let default_value: Option<String> = conn.query_row(
            "SELECT \"dflt_value\" FROM pragma_table_info('agent_nodes') WHERE name = 'is_pinned'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(
            default_value.as_deref(),
            Some("0"),
            "is_pinned default must be 0 so pre-v29 rows read back as unpinned"
        );

        // Pre-existing v28 row survives and reads back as unpinned.
        let pinned_existing: i64 = conn
            .query_row(
                "SELECT is_pinned FROM agent_nodes WHERE name = 'preexisting'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pinned_existing, 0, "v28 row must default to is_pinned = 0");

        // A new row can be pinned explicitly via the column writer (ticket
        // #985's UI affordance wires through `set_agent_node_pinned`).
        conn.execute(
            "INSERT INTO agent_nodes (mesh_id, name, path, is_pinned, created_at)
             VALUES (1, 'pinned', '/p2', 1, '2020-01-02T00:00:00Z')",
            [],
        )
        .unwrap();
        let pinned: i64 = conn
            .query_row(
                "SELECT is_pinned FROM agent_nodes WHERE name = 'pinned'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pinned, 1, "explicit is_pinned = 1 must persist");

        // Idempotent: a second call must not error (the column already
        // exists) and must not corrupt existing data.
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        let pinned_after: i64 = conn
            .query_row(
                "SELECT is_pinned FROM agent_nodes WHERE name = 'pinned'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            pinned_after, 1,
            "second safety-net call must not flip existing data"
        );
    }

    /// `set_agent_node_pinned` must (a) flip the column, (b) round-trip
    /// through `get_agent_node_by_id_inner`, (c) be idempotent on a no-op
    /// write, and (d) report zero rows for an unknown node id so the
    /// `#[command]` wrapper can surface "node not found".
    #[test]
    fn set_agent_node_pinned_round_trips_and_reports_unknown_id() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
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
                use_worktree INTEGER NOT NULL DEFAULT 1,
                is_pinned INTEGER NOT NULL DEFAULT 0,
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                source_issue INTEGER,
                source_pr INTEGER,
                head_repo_owner TEXT,
                head_repo_clone_url TEXT,
                source_pr_pinned_sha TEXT,
                signal_health TEXT,
                worktree_path TEXT
            );
            INSERT INTO agent_nodes (mesh_id, name, path, created_at)
                VALUES (1, 'n', '/n', '2020-01-01T00:00:00Z');
            ",
        )
        .unwrap();
        let id: i64 = conn
            .query_row("SELECT id FROM agent_nodes WHERE name = 'n'", [], |row| {
                row.get(0)
            })
            .unwrap();

        // Pin → returns 1, node reads back as pinned.
        let updated = crate::db::set_agent_node_pinned_inner(&conn, id, true).unwrap();
        assert_eq!(
            updated, 1,
            "set_agent_node_pinned must report 1 row updated"
        );
        let node = crate::db::get_agent_node_by_id_inner(&conn, id).unwrap();
        assert!(node.is_pinned, "post-write is_pinned must be true");

        // Unpin → returns 1, node reads back as unpinned.
        let updated = crate::db::set_agent_node_pinned_inner(&conn, id, false).unwrap();
        assert_eq!(updated, 1, "unpin must also report 1 row updated");
        let node = crate::db::get_agent_node_by_id_inner(&conn, id).unwrap();
        assert!(!node.is_pinned, "post-write is_pinned must be false");

        // Idempotent: re-pinning with the same value still succeeds.
        let updated = crate::db::set_agent_node_pinned_inner(&conn, id, false).unwrap();
        assert_eq!(updated, 1, "idempotent no-op write must still report 1 row");

        // Unknown id → 0 rows. The `#[command]` wrapper surfaces this as
        // an error string rather than silently no-op'ing.
        let updated = crate::db::set_agent_node_pinned_inner(&conn, 99999, true).unwrap();
        assert_eq!(updated, 0, "unknown id must report zero rows updated");
    }

    /// `toggle_agent_node_pinned` must (a) flip the column and return the
    /// new value, (b) toggle a second time back to the original, and
    /// (c) report `None` for an unknown id so the `#[command]` wrapper can
    /// surface "node not found" without faking a flipped boolean.
    #[test]
    fn toggle_agent_node_pinned_flips_and_reports_unknown_id() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
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
                use_worktree INTEGER NOT NULL DEFAULT 1,
                is_pinned INTEGER NOT NULL DEFAULT 0,
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                source_issue INTEGER,
                source_pr INTEGER,
                head_repo_owner TEXT,
                head_repo_clone_url TEXT,
                source_pr_pinned_sha TEXT,
                signal_health TEXT,
                worktree_path TEXT
            );
            INSERT INTO agent_nodes (mesh_id, name, path, created_at)
                VALUES (1, 'n', '/n', '2020-01-01T00:00:00Z');
            ",
        )
        .unwrap();
        let id: i64 = conn
            .query_row("SELECT id FROM agent_nodes WHERE name = 'n'", [], |row| {
                row.get(0)
            })
            .unwrap();

        // First toggle: 0 → 1, returns Some(true).
        let after_first = crate::db::toggle_agent_node_pinned_inner(&conn, id).unwrap();
        assert_eq!(after_first, Some(true), "first toggle must flip to true");
        let node = crate::db::get_agent_node_by_id_inner(&conn, id).unwrap();
        assert!(node.is_pinned, "post-toggle row must read as pinned");

        // Second toggle: 1 → 0, returns Some(false).
        let after_second = crate::db::toggle_agent_node_pinned_inner(&conn, id).unwrap();
        assert_eq!(
            after_second,
            Some(false),
            "second toggle must flip back to false"
        );
        let node = crate::db::get_agent_node_by_id_inner(&conn, id).unwrap();
        assert!(
            !node.is_pinned,
            "post-second-toggle row must read as unpinned"
        );

        // Unknown id → None. The `#[command]` wrapper surfaces this as
        // an error string rather than fabricating a flip.
        let after_unknown = crate::db::toggle_agent_node_pinned_inner(&conn, 99999).unwrap();
        assert_eq!(
            after_unknown, None,
            "unknown id must return None, not Some(false)"
        );
    }

    /// v35 — `agent_nodes.signal_health` (issue #1364 §3): a v34-shaped DB
    /// gains the nullable TEXT column via `evolve_to`, existing rows read
    /// back as `None`, and the column writer round-trips through
    /// `get_agent_node_by_id_inner`.
    #[test]
    fn evolve_to_adds_v35_signal_health_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
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
                use_worktree INTEGER NOT NULL DEFAULT 1,
                is_pinned INTEGER NOT NULL DEFAULT 0,
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                source_issue INTEGER,
                source_pr INTEGER,
                head_repo_owner TEXT,
                head_repo_clone_url TEXT,
                source_pr_pinned_sha TEXT
            );
            INSERT INTO agent_nodes (mesh_id, name, path, created_at)
                VALUES (1, 'pre-v35', '/p', '2020-01-01T00:00:00Z');
            ",
        )
        .unwrap();

        // Precondition: column does not exist yet.
        let has_before: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'signal_health'",
            [], |row| row.get(0),
        )
        .unwrap();
        assert!(
            !has_before,
            "PRECONDITION: signal_health must not exist in v34 schema"
        );

        // Act — the always-run column walk adds it.
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();

        let has_after: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'signal_health'",
            [], |row| row.get(0),
        )
        .unwrap();
        assert!(has_after, "signal_health must exist after evolve_to");

        // A pre-v35 row reads back as None (no provisioning outcome yet).
        let node = crate::db::get_agent_node_by_id_inner(&conn, 1).unwrap();
        assert_eq!(
            node.signal_health, None,
            "pre-v35 row must read as None health"
        );

        // The writer round-trips ok/degraded/unavailable and clears to None.
        use crate::agent::session_lifecycle::SignalHealth;
        crate::db::update_agent_node_signal_health_inner(&conn, 1, Some(SignalHealth::Ok)).unwrap();
        assert_eq!(
            crate::db::get_agent_node_by_id_inner(&conn, 1)
                .unwrap()
                .signal_health,
            Some(SignalHealth::Ok)
        );
        crate::db::update_agent_node_signal_health_inner(&conn, 1, Some(SignalHealth::Degraded))
            .unwrap();
        assert_eq!(
            crate::db::get_agent_node_by_id_inner(&conn, 1)
                .unwrap()
                .signal_health,
            Some(SignalHealth::Degraded)
        );
        crate::db::update_agent_node_signal_health_inner(&conn, 1, Some(SignalHealth::Unavailable))
            .unwrap();
        assert_eq!(
            crate::db::get_agent_node_by_id_inner(&conn, 1)
                .unwrap()
                .signal_health,
            Some(SignalHealth::Unavailable)
        );
        crate::db::update_agent_node_signal_health_inner(&conn, 1, None).unwrap();
        assert_eq!(
            crate::db::get_agent_node_by_id_inner(&conn, 1)
                .unwrap()
                .signal_health,
            None,
            "clearing to None must round-trip"
        );

        // Idempotent: a second evolve_to call must not error.
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
    }

    /// v37 — configurable Worktree Node directories (issue #1519): a
    /// pre-v37 DB gains `meshes.worktree_directory` and
    /// `agent_nodes.worktree_path`; existing rows read back as `None`
    /// (legacy `<mesh>/.claude/worktrees/<name>` fallback) and the narrow
    /// writers round-trip.
    /// The same upgrade also adds v38 queue positions and backfills existing
    /// rows in FIFO order.
    #[test]
    fn evolve_v37_to_v38_preserves_worktree_columns_and_backfills_queue() {
        let conn = Connection::open_in_memory().unwrap();
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
                use_worktree INTEGER NOT NULL DEFAULT 1,
                is_pinned INTEGER NOT NULL DEFAULT 0,
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                source_issue INTEGER,
                source_pr INTEGER,
                head_repo_owner TEXT,
                head_repo_clone_url TEXT,
                source_pr_pinned_sha TEXT,
                signal_health TEXT
            );
            INSERT INTO meshes (name, path) VALUES ('m', '/repo/m');
            INSERT INTO agent_nodes (mesh_id, name, path, worktree_name, created_at)
                VALUES (1, 'n', '/repo/m', 'n', '2020-01-01T00:00:00Z');
            INSERT INTO app_settings (key, value) VALUES ('schema_version', '36');

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
                circuit_id INTEGER NOT NULL REFERENCES autopilot_circuits(id) ON DELETE CASCADE,
                mesh_id INTEGER NOT NULL,
                trigger_identity TEXT NOT NULL DEFAULT '',
                state TEXT NOT NULL DEFAULT 'pending',
                context_json TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE (circuit_id, trigger_identity)
            );
            INSERT INTO autopilot_circuits
                (id, mesh_id, name, graph_json)
                VALUES (1, 7, 'legacy queue', '{\"version\":1,\"nodes\":[],\"edges\":[]}');
            INSERT INTO autopilot_circuit_runs
                (id, circuit_id, mesh_id, trigger_identity)
                VALUES (4, 1, 7, 'oldest'), (9, 1, 7, 'newest');
            ",
        )
        .unwrap();

        let has_mesh_before: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('meshes') WHERE name = 'worktree_directory'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let has_node_before: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'worktree_path'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !has_mesh_before,
            "PRECONDITION: meshes.worktree_directory must not exist"
        );
        assert!(
            !has_node_before,
            "PRECONDITION: agent_nodes.worktree_path must not exist"
        );

        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();

        let has_mesh_after: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('meshes') WHERE name = 'worktree_directory'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let has_node_after: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'worktree_path'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            has_mesh_after,
            "meshes.worktree_directory must exist after evolve_to"
        );
        assert!(
            has_node_after,
            "agent_nodes.worktree_path must exist after evolve_to"
        );

        // Pre-v37 rows read back as None (inherit / legacy fallback).
        let mesh = crate::db::get_mesh_by_id_inner(&conn, 1).unwrap();
        assert_eq!(mesh.worktree_directory, None);
        let node = crate::db::get_agent_node_by_id_inner(&conn, 1).unwrap();
        assert_eq!(node.worktree_path, None);

        // Narrow writers round-trip + blank clears to None.
        crate::db::set_mesh_worktree_directory_inner(&conn, 1, Some("custom-wt")).unwrap();
        assert_eq!(
            crate::db::get_mesh_by_id_inner(&conn, 1)
                .unwrap()
                .worktree_directory
                .as_deref(),
            Some("custom-wt")
        );
        crate::db::set_mesh_worktree_directory_inner(&conn, 1, Some("   ")).unwrap();
        assert_eq!(
            crate::db::get_mesh_by_id_inner(&conn, 1)
                .unwrap()
                .worktree_directory,
            None,
            "blank clears to inherit"
        );
        crate::db::adopt_manual_pool_slug_with_path_inner(
            &conn,
            1,
            "n",
            Some("/repo/m/custom-wt/n"),
        )
        .unwrap();
        assert_eq!(
            crate::db::get_agent_node_by_id_inner(&conn, 1)
                .unwrap()
                .worktree_path
                .as_deref(),
            Some("/repo/m/custom-wt/n")
        );

        // Idempotent.
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();

        let mut stmt = conn
            .prepare("SELECT id, queue_position FROM autopilot_circuit_runs ORDER BY id")
            .unwrap();
        let positions = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
            .unwrap()
            .collect::<SqlResult<Vec<_>>>()
            .unwrap();
        assert_eq!(positions, vec![(4, 4), (9, 9)]);
        assert_eq!(
            conn.query_row(
                "SELECT value FROM app_settings WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            crate::db::migrations::SCHEMA_VERSION.to_string()
        );

        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
    }

    #[test]
    fn init_schema_upgrades_legacy_circuit_runs_before_creating_the_queue_index() {
        // This is the production schema sequence without the process-global
        // database lifecycle, so it is safe to exercise in parallel tests.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO app_settings (key, value) VALUES ('schema_version', '37');
            CREATE TABLE meshes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE
            );
            CREATE TABLE agent_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                mesh_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                path TEXT NOT NULL
            );
            CREATE TABLE autopilot_circuit_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                circuit_id INTEGER NOT NULL,
                mesh_id INTEGER NOT NULL,
                trigger_identity TEXT NOT NULL DEFAULT '',
                state TEXT NOT NULL DEFAULT 'pending',
                context_json TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE (circuit_id, trigger_identity)
            );
            INSERT INTO autopilot_circuit_runs (id, circuit_id, mesh_id, trigger_identity)
                VALUES (4, 1, 7, 'legacy-run');
            ",
        )
        .unwrap();
        super::super::init_schema(&conn).unwrap();
        let has_queue_column: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('autopilot_circuit_runs') WHERE name = 'queue_position'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(has_queue_column);
        assert_eq!(
            conn.query_row(
                "SELECT queue_position FROM autopilot_circuit_runs WHERE id = 4",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            4,
            "legacy runs retain FIFO order when the queue column is added"
        );
        let has_queue_index: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'index' AND name = 'idx_circuit_runs_mesh_queue'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(has_queue_index);

        let fresh = Connection::open_in_memory().unwrap();
        super::super::init_schema(&fresh).unwrap();
        assert_eq!(
            canonical_index_names(&conn),
            canonical_index_names(&fresh),
            "legacy and fresh databases must converge on the canonical indexes"
        );
    }

    #[test]
    fn init_schema_creates_canonical_indexes_after_evolution() {
        let conn = Connection::open_in_memory().unwrap();

        super::super::init_schema(&conn).unwrap();

        for index in [
            "idx_coordinator_drive_prompts_created_at",
            "idx_warm_worktrees_mesh",
            "idx_warm_worktrees_status",
            "idx_agent_nodes_mesh",
            "idx_autopilot_runs_mesh",
            "idx_autopilot_circuits_mesh",
            "idx_autopilot_circuit_runs_circuit",
            "idx_autopilot_circuit_runs_state",
            "idx_circuit_runs_mesh_queue",
            "idx_circuit_steps_run",
        ] {
            let present: bool = conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    [index],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(present, "{index} must exist after schema initialization");
        }
    }
}
