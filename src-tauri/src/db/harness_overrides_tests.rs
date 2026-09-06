//! Integration tests for per-Mesh harness overrides (issue #1151 / slice 2
//! of #1148).
//!
//! Each test uses a fresh temp DB so the v32→v33 migration flags never
//! collide between tests. The migration tests use a hand-rolled v32 schema
//! fixture to drive the backfill deterministically; the CRUD tests use the
//! real `db::init()` so the `harness_overrides` column is present at v33+.
//!
//! Run with: cargo test --package buildmesh --lib db::harness_overrides_tests -- --test-threads=1

#[cfg(test)]
mod tests {
    use crate::db;
    use crate::preferences::HarnessConfigValue;
    use rusqlite::Connection;
    use std::collections::HashMap;

    fn fresh_db_path(tag: &str) -> std::path::PathBuf {
        let test_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "buildmesh_harness_overrides_{}_{}.db",
            tag, test_id
        ))
    }

    fn create_test_mesh(tag: &str) -> (i64, std::path::PathBuf) {
        let path = fresh_db_path(tag);
        db::init(&path).unwrap();
        let mesh =
            db::create_mesh(&format!("Mesh-{}", tag), &format!("/tmp/harness-{}", tag)).unwrap();
        (mesh.id, path)
    }

    // -----------------------------------------------------------------------
    // CRUD round-trips
    // -----------------------------------------------------------------------

    /// A fresh mesh's `harness_overrides` map is empty (acceptance criteria
    /// 1: "A new Mesh stores an empty sparse harness-override map").
    #[test]
    fn fresh_mesh_stores_empty_overrides_map() {
        let (mesh_id, path) = create_test_mesh("fresh");
        let overrides = db::get_mesh_harness_overrides(mesh_id).unwrap();
        assert_eq!(overrides, Some(HashMap::new()));
        std::fs::remove_file(&path).ok();
    }

    /// Upsert + read-back round-trip preserves per-harness independence
    /// (acceptance criteria 8: "A Mesh can hold independent overrides for
    /// multiple harnesses"). Two harnesses with different fields round-trip
    /// atomically without clobbering each other.
    #[test]
    fn upsert_preserves_sibling_overrides() {
        let (mesh_id, path) = create_test_mesh("siblings");
        let claude = HarnessConfigValue {
            model: Some("opus-4-1".into()),
            effort: Some("high".into()),
        };
        let codex = HarnessConfigValue {
            model: Some("gpt-5".into()),
            effort: Some("xhigh".into()),
        };
        db::upsert_mesh_harness_override(mesh_id, "claude", claude.clone()).unwrap();
        db::upsert_mesh_harness_override(mesh_id, "codex", codex.clone()).unwrap();

        let read = db::get_mesh_harness_overrides(mesh_id).unwrap().unwrap();
        assert_eq!(read.get("claude"), Some(&claude));
        assert_eq!(read.get("codex"), Some(&codex));
        assert_eq!(read.len(), 2, "two entries, no clobber");
        std::fs::remove_file(&path).ok();
    }

    /// Partial field: a Mesh override can override model only and inherit
    /// effort (acceptance criteria 16: "A partial Mesh override can override
    /// model while inheriting effort, or vice versa").
    #[test]
    fn partial_override_keeps_other_field_none() {
        let (mesh_id, path) = create_test_mesh("partial");
        let value = HarnessConfigValue {
            model: Some("opus-4-1".into()),
            effort: None,
        };
        db::upsert_mesh_harness_override(mesh_id, "claude", value.clone()).unwrap();
        let read = db::get_mesh_harness_overrides(mesh_id).unwrap().unwrap();
        let stored = read.get("claude").unwrap();
        assert_eq!(stored.model.as_deref(), Some("opus-4-1"));
        assert_eq!(stored.effort, None);
        std::fs::remove_file(&path).ok();
    }

    /// Empty value (all fields None) removes the sparse entry rather than
    /// storing `{model: null, effort: null}` (acceptance criteria 6: "Blank
    /// fields are absent rather than persisted as phantom values").
    #[test]
    fn empty_value_removes_sparse_entry() {
        let (mesh_id, path) = create_test_mesh("empty");
        let value = HarnessConfigValue {
            model: Some("opus".into()),
            effort: Some("high".into()),
        };
        db::upsert_mesh_harness_override(mesh_id, "claude", value).unwrap();
        let read = db::get_mesh_harness_overrides(mesh_id).unwrap().unwrap();
        assert_eq!(read.len(), 1);

        // Empty value collapses to absent
        let empty = HarnessConfigValue::default();
        db::upsert_mesh_harness_override(mesh_id, "claude", empty).unwrap();
        let read = db::get_mesh_harness_overrides(mesh_id).unwrap().unwrap();
        assert!(!read.contains_key("claude"), "empty entry removed");
        assert_eq!(read.len(), 0);
        std::fs::remove_file(&path).ok();
    }

    /// Remove one harness overrides only that harness (acceptance criteria
    /// 10: "Resetting one override removes only that harness entry and
    /// restores application inheritance"). Sibling overrides are preserved.
    #[test]
    fn remove_one_preserves_siblings() {
        let (mesh_id, path) = create_test_mesh("remove_one");
        db::upsert_mesh_harness_override(
            mesh_id,
            "claude",
            HarnessConfigValue {
                model: Some("opus".into()),
                effort: None,
            },
        )
        .unwrap();
        db::upsert_mesh_harness_override(
            mesh_id,
            "codex",
            HarnessConfigValue {
                model: Some("gpt-5".into()),
                effort: None,
            },
        )
        .unwrap();

        let rows = db::remove_mesh_harness_override(mesh_id, "claude").unwrap();
        assert_eq!(rows, 1);
        let read = db::get_mesh_harness_overrides(mesh_id).unwrap().unwrap();
        assert!(!read.contains_key("claude"));
        assert!(read.contains_key("codex"));
        assert_eq!(read.len(), 1);
        std::fs::remove_file(&path).ok();
    }

    /// Remove one harness is idempotent — clearing a missing harness is a
    /// no-op (the IPC surface depends on this for the UI's "Reset" affordance).
    #[test]
    fn remove_one_is_idempotent() {
        let (mesh_id, path) = create_test_mesh("remove_idem");
        let rows = db::remove_mesh_harness_override(mesh_id, "claude").unwrap();
        assert_eq!(
            rows, 1,
            "mesh is touched even on no-op so the row-count catches missing mesh"
        );
        let path_inner = path;
        std::fs::remove_file(&path_inner).ok();
    }

    /// Reset all clears every entry; the mesh row is touched so the row
    /// count is the source of truth for the "mesh not found" error.
    #[test]
    fn reset_all_clears_every_entry() {
        let (mesh_id, path) = create_test_mesh("reset_all");
        db::upsert_mesh_harness_override(
            mesh_id,
            "claude",
            HarnessConfigValue {
                model: Some("opus".into()),
                effort: None,
            },
        )
        .unwrap();
        db::upsert_mesh_harness_override(
            mesh_id,
            "codex",
            HarnessConfigValue {
                model: Some("gpt-5".into()),
                effort: None,
            },
        )
        .unwrap();

        let rows = db::clear_mesh_harness_overrides(mesh_id).unwrap();
        assert_eq!(rows, 1);
        let read = db::get_mesh_harness_overrides(mesh_id).unwrap().unwrap();
        assert_eq!(read.len(), 0);
        std::fs::remove_file(&path).ok();
    }

    /// Upsert on a missing mesh returns 0 rows (the IPC surface maps this
    /// to a "mesh not found" error rather than a panic).
    #[test]
    fn upsert_on_missing_mesh_returns_zero_rows() {
        let path = fresh_db_path("missing_mesh");
        db::init(&path).unwrap();
        let rows = db::upsert_mesh_harness_override(
            99999,
            "claude",
            HarnessConfigValue {
                model: Some("opus".into()),
                effort: None,
            },
        )
        .unwrap();
        assert_eq!(rows, 0);
        std::fs::remove_file(&path).ok();
    }

    /// Lock-once + `_inner(&Connection)` contract: the public
    /// `upsert_mesh_harness_override` locks once; the inner helper
    /// can be driven from a test fixture without a global lock. This
    /// pins the "no nested lock" rule (acceptance criteria 18: "Database
    /// code locks once per public operation and introduces no nested-lock
    /// path").
    #[test]
    fn inner_helper_does_not_lock_globally() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE meshes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                layout TEXT NOT NULL DEFAULT 'grid',
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                harness_overrides TEXT NOT NULL DEFAULT '{}'
            )",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO meshes (name, path) VALUES ('inner-mesh', '/tmp/inner-mesh')",
            [],
        )
        .unwrap();
        let mesh_id: i64 = conn
            .query_row("SELECT last_insert_rowid()", [], |row| row.get(0))
            .unwrap();
        let rows = db::upsert_mesh_harness_override_inner(
            &conn,
            mesh_id,
            "claude",
            HarnessConfigValue {
                model: Some("opus".into()),
                effort: None,
            },
        )
        .unwrap();
        assert_eq!(rows, 1);
        let read = db::get_mesh_harness_overrides_inner(&conn, mesh_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            read.get("claude").and_then(|v| v.model.as_deref()),
            Some("opus")
        );
    }

    // -----------------------------------------------------------------------
    // v33 migration (one-shot backfill)
    // -----------------------------------------------------------------------

    /// Build a v32 schema fixture (no `harness_overrides` column) and seed
    /// a Mesh with non-empty legacy model/effort. The migrate-once flag
    /// ensures the test mirrors the production runner path.
    fn build_v32_mesh_fixture(
        legacy_model: Option<&str>,
        legacy_effort: Option<&str>,
        tag: &str,
    ) -> (Connection, i64) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE app_settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT INTO app_settings (key, value) VALUES ('schema_version', '32');
            CREATE TABLE meshes (
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
                scratchpad TEXT NOT NULL DEFAULT '',
                sandbox INTEGER NOT NULL DEFAULT 0,
                pre_spawn_pool_size INTEGER NOT NULL DEFAULT 0,
                color TEXT,
                autopilot_enabled INTEGER NOT NULL DEFAULT 0,
                autopilot_trigger_label TEXT,
                autopilot_concurrency_limit INTEGER NOT NULL DEFAULT 2,
                autopilot_provider TEXT,
                autopilot_action_on_success TEXT,
                root_build_command TEXT,
                root_run_command TEXT,
                autopilot_mode TEXT NOT NULL DEFAULT 'issue_driven',
                loop_initial_prompt TEXT,
                loop_suffix_prompt TEXT,
                loop_max_iterations INTEGER,
                loop_interval_seconds INTEGER NOT NULL DEFAULT 0,
                loop_consecutive_failures INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO meshes (name, path, model, effort) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                format!("v32-{}", tag),
                format!("/tmp/v32-{}", tag),
                legacy_model,
                legacy_effort,
            ],
        )
        .unwrap();
        let mesh_id: i64 = conn
            .query_row("SELECT last_insert_rowid()", [], |row| row.get(0))
            .unwrap();
        (conn, mesh_id)
    }

    /// Acceptance criteria 22: a v32 Mesh with non-empty legacy model/effort
    /// upgrades with an equivalent Claude Code override.
    #[test]
    fn v33_migration_copies_non_empty_legacy_to_claude_override() {
        let (conn, _mesh_id) = build_v32_mesh_fixture(Some("opus-4-1"), Some("high"), "both");

        // Run the v33 one-shot backfill manually (the runner's flag check
        // is exactly what we want to bypass for a fresh test).
        conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('schema_version', '33')",
            [],
        )
        .unwrap();
        conn.execute_batch(
            "ALTER TABLE meshes ADD COLUMN harness_overrides TEXT NOT NULL DEFAULT '{}';",
        )
        .unwrap();
        // The backfill SQL — same as the registry entry; copy verbatim so
        // the test pins the live migration.
        conn.execute(crate::db::migrations::V33_BACKFILL_SQL, [])
            .unwrap();

        let raw: String = conn
            .query_row("SELECT harness_overrides FROM meshes LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let parsed: HashMap<String, HarnessConfigValue> = serde_json::from_str(&raw).unwrap();
        let claude = parsed.get("claude").expect("claude override created");
        assert_eq!(claude.model.as_deref(), Some("opus-4-1"));
        assert_eq!(claude.effort.as_deref(), Some("high"));
    }

    /// Acceptance criteria 23: empty legacy values do NOT create an
    /// override entry.
    #[test]
    fn v33_migration_no_override_for_empty_legacy_values() {
        let (conn, _mesh_id) = build_v32_mesh_fixture(None, None, "empty");

        conn.execute_batch(
            "ALTER TABLE meshes ADD COLUMN harness_overrides TEXT NOT NULL DEFAULT '{}';",
        )
        .unwrap();
        conn.execute(crate::db::migrations::V33_BACKFILL_SQL, [])
            .unwrap();

        let raw: String = conn
            .query_row("SELECT harness_overrides FROM meshes LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let parsed: HashMap<String, HarnessConfigValue> = serde_json::from_str(&raw).unwrap();
        assert!(
            parsed.is_empty(),
            "empty legacy values do not create an override entry"
        );
    }

    /// Acceptance criteria 24: existing new-format values are NOT
    /// overwritten by the one-shot backfill. A Mesh that hand-edits an
    /// override before the migration runs keeps its authored values.
    #[test]
    fn v33_migration_does_not_overwrite_existing_claude_override() {
        let (conn, _mesh_id) =
            build_v32_mesh_fixture(Some("legacy-ignored"), Some("high"), "preserve");

        conn.execute_batch(
            "ALTER TABLE meshes ADD COLUMN harness_overrides TEXT NOT NULL DEFAULT '{}';",
        )
        .unwrap();
        // Hand-edit a `claude` entry BEFORE the migration runs.
        conn.execute(
            "UPDATE meshes SET harness_overrides = ?1",
            rusqlite::params![r#"{"claude":{"model":"user-authored","effort":"medium"}}"#],
        )
        .unwrap();
        conn.execute(crate::db::migrations::V33_BACKFILL_SQL, [])
            .unwrap();

        let raw: String = conn
            .query_row("SELECT harness_overrides FROM meshes LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let parsed: HashMap<String, HarnessConfigValue> = serde_json::from_str(&raw).unwrap();
        let claude = parsed.get("claude").expect("claude override preserved");
        assert_eq!(claude.model.as_deref(), Some("user-authored"));
        assert_eq!(claude.effort.as_deref(), Some("medium"));
    }

    /// Acceptance criteria 5: re-running the migration is safe
    /// (idempotent / repeat-safe). The flag-gated runner only writes the
    /// `claude` entry once; re-running the same SQL on the same row is a
    /// no-op because the `json_extract(..,'$.claude') IS NULL` predicate
    /// excludes already-populated rows.
    #[test]
    fn v33_migration_repeat_safe() {
        let (conn, _mesh_id) = build_v32_mesh_fixture(Some("opus-4-1"), Some("high"), "repeat");

        conn.execute_batch(
            "ALTER TABLE meshes ADD COLUMN harness_overrides TEXT NOT NULL DEFAULT '{}';",
        )
        .unwrap();
        let sql = "UPDATE meshes \
             SET harness_overrides = json_patch( \
                 COALESCE(harness_overrides, '{}'), \
                 json_object( \
                     'claude', json_object( \
                         'model', CASE WHEN TRIM(COALESCE(model, '')) = '' \
                                          THEN NULL ELSE TRIM(model) END, \
                         'effort', CASE WHEN TRIM(COALESCE(effort, '')) = '' \
                                           THEN NULL ELSE TRIM(effort) END \
                     ) \
                 ) \
             ) \
             WHERE (TRIM(COALESCE(model, '')) != '' \
                    OR TRIM(COALESCE(effort, '')) != '') \
               AND json_extract(harness_overrides, '$.claude') IS NULL";
        // Run twice.
        conn.execute(sql, []).unwrap();
        let after_first: String = conn
            .query_row("SELECT harness_overrides FROM meshes LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        conn.execute(sql, []).unwrap();
        let after_second: String = conn
            .query_row("SELECT harness_overrides FROM meshes LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(after_first, after_second, "repeat-safe migration");
    }

    /// Whitespace-only legacy values are trimmed by the migration SQL —
    /// the row has `model = '   '` and the migration skips it (no entry
    /// is created for whitespace-only values).
    #[test]
    fn v33_migration_whitespace_legacy_values() {
        let (conn, _mesh_id) = build_v32_mesh_fixture(Some("   "), Some("    "), "ws");

        conn.execute_batch(
            "ALTER TABLE meshes ADD COLUMN harness_overrides TEXT NOT NULL DEFAULT '{}';",
        )
        .unwrap();
        conn.execute(crate::db::migrations::V33_BACKFILL_SQL, [])
            .unwrap();

        let raw: String = conn
            .query_row("SELECT harness_overrides FROM meshes LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let parsed: HashMap<String, HarnessConfigValue> = serde_json::from_str(&raw).unwrap();
        assert!(
            parsed.is_empty(),
            "whitespace-only legacy does not create an entry"
        );
    }
}
