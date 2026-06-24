//! Tests for the `warm_worktrees` table + CRUD helpers (issue #609, v21 schema).
//!
//! These tests pin the contracts the spawn pipeline + background worker rely on:
//!   * the safety-net `ensure_warm_worktables_table` is idempotent on a v20 DB
//!     (the case that bit `source_issue` / `sandbox` in earlier versions);
//!   * `claim_warm_entry_for_mesh` is atomic — two concurrent claims on the
//!     same mesh never both succeed;
//!   * `list_warm_worktrees_to_reconcile` returns rows whose on-disk directory
//!     is missing OR that a crash left stuck in `filling`/`refreshing` (so the
//!     startup reconcile can tear down their git metadata + prune them, #610);
//!   * `count_available_warm_for_mesh` matches the worker's target bookkeeping
//!     (the v21 tracer bullet's target is 1, so the worker fills until the
//!     count is at least 1, then stands down);
//!   * `list_worktree_enabled_meshes_for_warm` only returns rows whose
//!     `use_worktree = 1`, so a worktree-disabled mesh never spawns pool work.
//!
//! Run with: cargo test --package buildmesh --lib db::warm_pool_tests

#[cfg(test)]
mod tests {
    use crate::db::{
        claim_warm_entry_for_mesh_inner, count_available_warm_for_mesh_inner,
        delete_warm_worktrees_for_mesh_inner, ensure_warm_worktables_table,
        insert_warm_worktree_inner, list_warm_worktrees_to_reconcile_inner,
        list_worktree_enabled_meshes_for_warm_inner, mark_warm_worktree_available_inner,
        WarmWorktreeStatus,
    };
    use rusqlite::Connection;

    /// Build an in-memory schema that matches v20 (no `warm_worktrees` table)
    /// plus the test fixture's `meshes` rows. The safety-net helper is then
    /// expected to bring the schema forward to v21.
    fn v20_schema_with_mesh(use_worktree: bool) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE app_settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT INTO app_settings (key, value) VALUES ('schema_version', '20');

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
                sandbox INTEGER NOT NULL DEFAULT 0
            );
            ",
        )
        .unwrap();
        let use_wt = if use_worktree { 1 } else { 0 };
        conn.execute(
            "INSERT INTO meshes (name, path, use_worktree) VALUES ('m', '/repo/m', ?1)",
            rusqlite::params![use_wt],
        )
        .unwrap();
        conn
    }

    /// The v20→v21 upgrade must add the table via the safety net (mirrors the
    /// `source_issue` / `sandbox` regression class — a build that bumped the
    /// version without creating the table would silently leave spawn claiming
    /// `None` and never warm anything).
    #[test]
    fn ensure_warm_worktables_table_brings_v20_forward() {
        let conn = v20_schema_with_mesh(true);

        // Precondition: no `warm_worktrees` table on a v20 DB.
        let before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='warm_worktrees'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(before, 0, "v20 DB must not yet have warm_worktrees");

        ensure_warm_worktables_table(&conn).unwrap();

        let after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='warm_worktrees'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, 1, "safety net must materialise the table");
    }

    /// Idempotent: a second call on an already-upgraded DB is a no-op (so
    /// `init` can call it unconditionally every launch without erroring).
    #[test]
    fn ensure_warm_worktables_table_is_idempotent() {
        let conn = v20_schema_with_mesh(true);
        ensure_warm_worktables_table(&conn).unwrap();
        // Second call must succeed without error.
        ensure_warm_worktables_table(&conn).unwrap();
    }

    /// Claim is atomic: two concurrent claims on the same mesh each see
    /// different rows (FIFO order). Without the `AND status = 'available'`
    /// guard on the UPDATE, both would happily return the same id and the
    /// second spawn would overwrite the first node's worktree path.
    #[test]
    fn claim_is_atomic_under_concurrent_call() {
        let conn = v20_schema_with_mesh(true);
        ensure_warm_worktables_table(&conn).unwrap();

        // Insert two available rows for mesh 1.
        let id_a = insert_warm_worktree_inner(
            &conn,
            1,
            "/repo/m/.claude/worktrees/pool-warm-a",
            "pool-warm-a",
            Some("aaaa"),
            WarmWorktreeStatus::Available,
        )
        .unwrap();
        let id_b = insert_warm_worktree_inner(
            &conn,
            1,
            "/repo/m/.claude/worktrees/pool-warm-b",
            "pool-warm-b",
            Some("bbbb"),
            WarmWorktreeStatus::Available,
        )
        .unwrap();

        let first = claim_warm_entry_for_mesh_inner(&conn, 1).unwrap().unwrap();
        let second = claim_warm_entry_for_mesh_inner(&conn, 1).unwrap().unwrap();

        // FIFO by created_at: both rows were inserted sequentially; the
        // older one wins the first claim. The two ids must differ.
        assert_ne!(first.id, second.id, "concurrent claims must hand out distinct rows");
        assert_eq!(
            first.id, id_a,
            "oldest available row must win the first claim (FIFO)"
        );
        assert_eq!(second.id, id_b);

        // Third claim sees nothing — pool drained.
        assert!(
            claim_warm_entry_for_mesh_inner(&conn, 1).unwrap().is_none(),
            "pool must be empty after two claims"
        );
    }

    /// A `filling` row is invisible to claimers — the spawn path never picks
    /// up an in-flight checkout (the directory might not exist yet, so a
    /// claim-then-spawn would race the worker's `git worktree add`).
    #[test]
    fn claim_skips_filling_rows() {
        let conn = v20_schema_with_mesh(true);
        ensure_warm_worktables_table(&conn).unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            "/repo/m/.claude/worktrees/pool-warm-x",
            "pool-warm-x",
            Some("aaaa"),
            WarmWorktreeStatus::Filling,
        )
        .unwrap();

        assert!(
            claim_warm_entry_for_mesh_inner(&conn, 1).unwrap().is_none(),
            "filling rows must not be claimable"
        );
    }

    /// `mark_warm_worktree_available` flips `filling → available` and stamps
    /// the base SHA. The spawn path reads `base_sha` for the freshness check.
    #[test]
    fn mark_available_flips_filling_to_available() {
        let conn = v20_schema_with_mesh(true);
        ensure_warm_worktables_table(&conn).unwrap();
        let id = insert_warm_worktree_inner(
            &conn,
            1,
            "/repo/m/.claude/worktrees/pool-warm-x",
            "pool-warm-x",
            None,
            WarmWorktreeStatus::Filling,
        )
        .unwrap();
        // Precondition: no available row.
        assert_eq!(count_available_warm_for_mesh_inner(&conn, 1).unwrap(), 0);
        mark_warm_worktree_available_inner(&conn, id, Some("deadbeef")).unwrap();
        // Postcondition: one available row.
        assert_eq!(count_available_warm_for_mesh_inner(&conn, 1).unwrap(), 1);
        // The claim surfaces the freshly recorded base SHA.
        let claimed = claim_warm_entry_for_mesh_inner(&conn, 1).unwrap().unwrap();
        assert_eq!(claimed.base_sha.as_deref(), Some("deadbeef"));
    }

    /// Threshold the reconcile tests pass: any `filling`/`refreshing` row older
    /// than this is treated as a crash-orphan; younger ones are assumed to be a
    /// worker filling right now and are left alone (issue #610 race guard).
    const STALE_AFTER_MIN: i64 = 5;

    /// Back-date a row's `created_at` so the age guard treats it as a
    /// crash-orphan from a prior session rather than an in-flight fill.
    fn age_row(conn: &Connection, id: i64) {
        conn.execute(
            "UPDATE warm_worktrees SET created_at = datetime('now', '-10 minutes') WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap();
    }

    /// An `available` row whose on-disk directory is missing is reconciled
    /// regardless of age — a settled `available` row always had its directory
    /// created, so a missing one is unambiguously the manual-delete case (issue
    /// #610 AC3). A healthy `available` row whose path exists is left alone.
    #[test]
    fn reconcile_flags_available_row_with_missing_directory() {
        let conn = v20_schema_with_mesh(true);
        ensure_warm_worktables_table(&conn).unwrap();
        // Ghost row: directory doesn't exist (path is just a fresh tempfile
        // string; we won't create it). Deliberately left at the fresh
        // `created_at` to prove `available` rows ignore the age guard.
        let ghost_id = insert_warm_worktree_inner(
            &conn,
            1,
            "/this/path/does/not/exist/anywhere",
            "pool-warm-ghost",
            Some("aaaa"),
            WarmWorktreeStatus::Available,
        )
        .unwrap();
        // Live row: directory exists.
        let tmp = tempfile::TempDir::new().unwrap();
        let live_id = insert_warm_worktree_inner(
            &conn,
            1,
            tmp.path().to_str().unwrap(),
            "pool-warm-live",
            Some("bbbb"),
            WarmWorktreeStatus::Available,
        )
        .unwrap();

        let entries = list_warm_worktrees_to_reconcile_inner(&conn, STALE_AFTER_MIN).unwrap();
        let ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![ghost_id], "only the ghost row should be reconciled");
        // The entry carries the path + a `dir_present=false` flag so the
        // reconcile skips the (pointless) git teardown and drops the row.
        assert_eq!(entries[0].path, "/this/path/does/not/exist/anywhere");
        assert!(!entries[0].dir_present, "a missing-dir entry must report dir_present=false");
        assert!(
            !ids.contains(&live_id),
            "row pointing at an existing directory must not be flagged stale"
        );
    }

    /// An OLD `filling` row whose directory exists (a crash mid-`git worktree
    /// add` in a prior session) is reconciled with `dir_present=true` so the
    /// caller tears down the partial worktree (issue #610 AC1). The missing-dir
    /// scan alone would miss it (its directory is present).
    #[test]
    fn reconcile_flags_old_filling_row_with_live_directory() {
        let conn = v20_schema_with_mesh(true);
        ensure_warm_worktables_table(&conn).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let filling_id = insert_warm_worktree_inner(
            &conn,
            1,
            tmp.path().to_str().unwrap(),
            "pool-warm-filling",
            None,
            WarmWorktreeStatus::Filling,
        )
        .unwrap();
        age_row(&conn, filling_id);

        let entries = list_warm_worktrees_to_reconcile_inner(&conn, STALE_AFTER_MIN).unwrap();
        let ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
        assert_eq!(
            ids, vec![filling_id],
            "an aged `filling` crash-orphan must be reconciled even when its directory exists"
        );
        assert!(entries[0].dir_present, "a live-directory entry must report dir_present=true");
    }

    /// THE RACE GUARD: a FRESH `filling` row (a worker is mid-checkout right
    /// now) must NOT be reconciled. Without the age guard the reconcile would
    /// tear down a directory another thread's `create_git_worktree` is still
    /// writing (issue #610 review). The row is young (default `created_at`), so
    /// it stays put until it either flips to `available` or genuinely ages out.
    #[test]
    fn reconcile_skips_fresh_filling_row() {
        let conn = v20_schema_with_mesh(true);
        ensure_warm_worktables_table(&conn).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        // Inserted at `datetime('now')` — NOT aged.
        insert_warm_worktree_inner(
            &conn,
            1,
            tmp.path().to_str().unwrap(),
            "pool-warm-inflight",
            None,
            WarmWorktreeStatus::Filling,
        )
        .unwrap();

        let entries = list_warm_worktrees_to_reconcile_inner(&conn, STALE_AFTER_MIN).unwrap();
        assert!(
            entries.is_empty(),
            "a freshly-inserted `filling` row (a worker filling it right now) must NOT be reconciled"
        );
    }

    /// An OLD `refreshing` row is reconciled like a `filling` one (issue #610
    /// AC1). `refreshing` is now a first-class `WarmWorktreeStatus` variant;
    /// here we set it through the enum's `as_str()` to mirror what the future
    /// SHA-refresh worker (PRD #608 §4) will write.
    #[test]
    fn reconcile_flags_old_refreshing_row() {
        let conn = v20_schema_with_mesh(true);
        ensure_warm_worktables_table(&conn).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let id = insert_warm_worktree_inner(
            &conn,
            1,
            tmp.path().to_str().unwrap(),
            "pool-warm-refreshing",
            Some("cccc"),
            WarmWorktreeStatus::Refreshing,
        )
        .unwrap();
        age_row(&conn, id);

        let entries = list_warm_worktrees_to_reconcile_inner(&conn, STALE_AFTER_MIN).unwrap();
        let ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![id], "an aged `refreshing` crash-orphan must be reconciled");
    }

    /// A `claimed` row is NEVER reconciled here — even aged, even dir-present:
    /// its directory may already back a live agent node's worktree, so tearing
    /// it down would destroy a running agent's work. `claimed` orphans are
    /// pruned (row-only) by `delete_orphaned_claimed_warm_worktrees` instead.
    #[test]
    fn reconcile_never_flags_claimed_rows() {
        let conn = v20_schema_with_mesh(true);
        ensure_warm_worktables_table(&conn).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let id = insert_warm_worktree_inner(
            &conn,
            1,
            tmp.path().to_str().unwrap(),
            "pool-warm-claimed",
            Some("dddd"),
            WarmWorktreeStatus::Claimed,
        )
        .unwrap();
        age_row(&conn, id);

        let entries = list_warm_worktrees_to_reconcile_inner(&conn, STALE_AFTER_MIN).unwrap();
        assert!(
            !entries.iter().any(|e| e.id == id),
            "a `claimed` row (its dir may back a live node) must never be torn down by reconcile"
        );
    }

    /// `delete_warm_worktrees_for_mesh_inner` is the mesh-delete hook (wired
    /// into `delete_mesh`): when a mesh is deleted, every pool row pointing at
    /// its worktrees must go too — the FK cascade is off, so without this the
    /// rows outlive the mesh as orphans.
    #[test]
    fn delete_for_mesh_removes_every_row_for_that_mesh() {
        let conn = v20_schema_with_mesh(true);
        ensure_warm_worktables_table(&conn).unwrap();
        // Mesh 1 owns two rows; mesh 2 (we add manually) owns one. Only the
        // mesh-1 rows must be deleted.
        let tmp = tempfile::TempDir::new().unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            tmp.path().join("a").to_str().unwrap(),
            "pool-warm-a",
            Some("a"),
            WarmWorktreeStatus::Available,
        )
        .unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            tmp.path().join("b").to_str().unwrap(),
            "pool-warm-b",
            Some("b"),
            WarmWorktreeStatus::Available,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO meshes (name, path) VALUES ('m2', '/repo/m2')",
            [],
        )
        .unwrap();
        let m2_id: i64 = conn.last_insert_rowid();
        insert_warm_worktree_inner(
            &conn,
            m2_id,
            tmp.path().join("c").to_str().unwrap(),
            "pool-warm-c",
            Some("c"),
            WarmWorktreeStatus::Available,
        )
        .unwrap();

        let deleted = delete_warm_worktrees_for_mesh_inner(&conn, 1).unwrap();
        assert_eq!(deleted, 2, "two mesh-1 rows must be deleted");
        // Mesh-2 row survives.
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM warm_worktrees WHERE mesh_id = ?1",
                rusqlite::params![m2_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 1, "mesh-2 row must not be touched");
    }

    /// `list_worktree_enabled_meshes_for_warm` only returns meshes whose
    /// `use_worktree = 1`. A worktree-disabled mesh must not spawn pool
    /// work (the spawn path itself short-circuits when `use_worktree = 0`,
    /// so warming it would be pure waste).
    #[test]
    fn list_for_warm_filters_out_worktree_disabled_meshes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
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
                sandbox INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO meshes (name, path, use_worktree) VALUES ('enabled', '/r/enabled', 1);
            INSERT INTO meshes (name, path, use_worktree) VALUES ('disabled', '/r/disabled', 0);
            ",
        )
        .unwrap();

        let rows = list_worktree_enabled_meshes_for_warm_inner(&conn).unwrap();
        assert_eq!(rows.len(), 1, "only the worktree-enabled mesh must be listed");
        assert_eq!(rows[0].path, "/r/enabled");
    }
}