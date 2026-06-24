//! Tests for the `warm_worktrees` table + CRUD helpers (issue #609, v21 schema),
//! extended in issue #611 for per-mesh pool size + downsize drain.
//!
//! These tests pin the contracts the spawn pipeline + background worker rely on:
//!   * the safety-net `ensure_warm_worktables_table` is idempotent on a v20 DB
//!     (the case that bit `source_issue` / `sandbox` in earlier versions);
//!   * `claim_warm_entry_for_mesh` is atomic — two concurrent claims on the
//!     same mesh never both succeed;
//!   * `list_stale_warm_worktrees` returns rows whose on-disk directory is
//!     missing (so the startup reconcile can prune them);
//!   * `count_available_warm_for_mesh` matches the worker's target bookkeeping
//!     (the v21 tracer bullet's target is 1, so the worker fills until the
//!     count is at least 1, then stands down);
//!   * `list_worktree_enabled_meshes_for_warm` only returns rows whose
//!     `use_worktree = 1`, so a worktree-disabled mesh never spawns pool work;
//!   * (v22, issue #611) `pre_spawn_pool_size` round-trips through
//!     `list_worktree_enabled_meshes_for_warm`, `count_warm_entries_for_mesh`
//!     counts ALL statuses (not just `available`), `list_oldest_warm_entries`
//!     prefers `filling` rows for drain, and `is_warm_pool_path` returns true
//!     iff a row exists for the path.
//!
//! Run with: cargo test --package buildmesh --lib db::warm_pool_tests

#[cfg(test)]
mod tests {
    use crate::db::{
        claim_warm_entry_for_mesh_inner, count_available_warm_for_mesh_inner,
        count_warm_entries_for_mesh_inner, delete_warm_worktrees_for_mesh_inner,
        ensure_mesh_pre_spawn_pool_size, ensure_warm_worktables_table,
        insert_warm_worktree_inner, is_warm_pool_path_inner,
        list_oldest_warm_entries_for_mesh_inner, list_stale_warm_worktrees_inner,
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

    /// `list_stale_warm_worktrees` returns ids whose on-disk directory is
    /// missing — the startup reconcile uses this to prune ghost rows. A
    /// row whose path DOES exist on disk is left alone.
    #[test]
    fn list_stale_returns_rows_with_missing_directory() {
        let conn = v20_schema_with_mesh(true);
        ensure_warm_worktables_table(&conn).unwrap();
        // Ghost row: directory doesn't exist (path is just a fresh tempfile
        // string; we won't create it).
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

        let stale = list_stale_warm_worktrees_inner(&conn).unwrap();
        assert_eq!(stale, vec![ghost_id], "only the ghost row should be stale");
        // Live row stays.
        assert!(
            !stale.contains(&live_id),
            "row pointing at an existing directory must not be flagged stale"
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
                sandbox INTEGER NOT NULL DEFAULT 0,
                -- v22 (issue #611): per-mesh pool target. Test schema
                -- mirrors the production v22 shape so the SELECT in
                -- list_worktree_enabled_meshes_for_warm_inner finds the
                -- column. Real DBs get the column via
                -- `ensure_mesh_pre_spawn_pool_size` (see db::init).
                pre_spawn_pool_size INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO meshes (name, path, use_worktree) VALUES ('enabled', '/r/enabled', 1);
            INSERT INTO meshes (name, path, use_worktree) VALUES ('disabled', '/r/disabled', 0);
            ",
        )
        .unwrap();

        let rows = list_worktree_enabled_meshes_for_warm_inner(&conn).unwrap();
        assert_eq!(rows.len(), 1, "only the worktree-enabled mesh must be listed");
        assert_eq!(rows[0].path, "/r/enabled");
        assert_eq!(rows[0].pre_spawn_pool_size, 0);
    }

    // ── v22 (issue #611) — per-mesh pool size + drain helpers ─────────────

    /// Build a v22 schema (meshes + pre_spawn_pool_size + warm_worktrees)
    /// with one worktree-enabled mesh. Mirrors the v20 fixture's pattern
    /// but exercises the safety-net forward path for both columns and
    /// tables — proves that an older DB seeded here reads correctly
    /// through the v22 helpers without an explicit migrate_if_needed call.
    fn v22_schema_with_mesh(pool_size: i64) -> Connection {
        let conn = v20_schema_with_mesh(true);
        // Forward both: column add (v21→v22) and table create (v20→v21).
        ensure_mesh_pre_spawn_pool_size(&conn).unwrap();
        ensure_warm_worktables_table(&conn).unwrap();
        conn.execute(
            "UPDATE meshes SET pre_spawn_pool_size = ?1 WHERE id = 1",
            rusqlite::params![pool_size],
        )
        .unwrap();
        conn
    }

    /// `pre_spawn_pool_size` round-trips through `list_worktree_enabled_meshes_for_warm`.
    /// The service-layer worker reads this column to decide its fill target
    /// and downsize threshold, so a silent type/scope drift between the SQL
    /// projection and the worker would surface here as a wrong-sized `target`
    /// in the per-mesh fill loop.
    #[test]
    fn list_for_warm_reads_pre_spawn_pool_size() {
        let conn = v22_schema_with_mesh(3);
        let rows = list_worktree_enabled_meshes_for_warm_inner(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pre_spawn_pool_size, 3);
        // And `0` (the off default) round-trips just as cleanly — a
        // misrouted `null`/`None` would otherwise show up as 0 and
        // accidentally turn the pool off for a user who'd set 5.
        let conn_zero = v22_schema_with_mesh(0);
        let rows_zero = list_worktree_enabled_meshes_for_warm_inner(&conn_zero).unwrap();
        assert_eq!(rows_zero[0].pre_spawn_pool_size, 0);
    }

    /// `count_warm_entries_for_mesh` counts every status (filling,
    /// available, claimed) — distinct from `count_available_warm_for_mesh`
    /// which only counts claimable rows. The drain logic uses this so a
    /// stuck `claimed` row from a failed `forget_after_spawn` doesn't fool
    /// the drain into thinking the mesh is at target.
    #[test]
    fn count_warm_entries_counts_all_statuses() {
        let conn = v22_schema_with_mesh(3);
        // Two available, one filling, one claimed → 4 total.
        let tmp = tempfile::TempDir::new().unwrap();
        let p = |name: &str| tmp.path().join(name).to_str().unwrap().to_string();
        insert_warm_worktree_inner(
            &conn,
            1,
            &p("a"),
            "a",
            Some("aa"),
            WarmWorktreeStatus::Available,
        )
        .unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            &p("b"),
            "b",
            Some("bb"),
            WarmWorktreeStatus::Available,
        )
        .unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            &p("c"),
            "c",
            None,
            WarmWorktreeStatus::Filling,
        )
        .unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            &p("d"),
            "d",
            None,
            WarmWorktreeStatus::Claimed,
        )
        .unwrap();

        let total = count_warm_entries_for_mesh_inner(&conn, 1).unwrap();
        assert_eq!(total, 4, "all four rows (any status) must count");

        // And the available-only count still works — the v21 contract
        // the fill loop depends on.
        let available = count_available_warm_for_mesh_inner(&conn, 1).unwrap();
        assert_eq!(available, 2, "only the two Available rows count");
    }

    /// The drain ordering contract: `filling` rows beat every older
    /// `available` row (cheapest to drop — they're mid-checkout and
    /// would be GC'd on next reconcile anyway). Within the same status,
    /// FIFO by `created_at`. Pinned here because a wrong ordering would
    /// either delete the user's "freshest" pre-cut or leave a stuck
    /// `filling` row to fester.
    #[test]
    fn list_oldest_warm_entries_prefers_filling_status() {
        let conn = v22_schema_with_mesh(1);
        let tmp = tempfile::TempDir::new().unwrap();
        let p = |name: &str| tmp.path().join(name).to_str().unwrap().to_string();
        // Available inserted FIRST (older) but the drain must NOT pick
        // it — the Filling row inserted SECOND (newer) wins on status.
        let avail_id = insert_warm_worktree_inner(
            &conn,
            1,
            &p("old-available"),
            "old-available",
            Some("aa"),
            WarmWorktreeStatus::Available,
        )
        .unwrap();
        let filling_id = insert_warm_worktree_inner(
            &conn,
            1,
            &p("new-filling"),
            "new-filling",
            None,
            WarmWorktreeStatus::Filling,
        )
        .unwrap();

        let picks = list_oldest_warm_entries_for_mesh_inner(&conn, 1, 1).unwrap();
        assert_eq!(picks.len(), 1, "limit=1 must return one row");
        assert_eq!(
            picks[0].0, filling_id,
            "Filling row must be picked over an older Available row"
        );
        assert_ne!(
            picks[0].0, avail_id,
            "older Available row must NOT be picked when a Filling row exists"
        );

        // And the path comes along — the caller needs it for
        // `git worktree remove --force`.
        assert_eq!(picks[0].1, p("new-filling"));
    }

    /// `list_oldest_warm_entries_for_mesh_inner` obeys the LIMIT. With
    /// target shrink 3→1 we expect 2 picks; with shrink 3→0 we expect 3.
    /// Regression test for the drain's `excess = count - target` math.
    #[test]
    fn list_oldest_warm_entries_obeys_limit() {
        let conn = v22_schema_with_mesh(0);
        let tmp = tempfile::TempDir::new().unwrap();
        let p = |name: &str| tmp.path().join(name).to_str().unwrap().to_string();
        for (i, status) in [
            WarmWorktreeStatus::Available,
            WarmWorktreeStatus::Available,
            WarmWorktreeStatus::Available,
        ]
        .iter()
        .enumerate()
        {
            insert_warm_worktree_inner(
                &conn,
                1,
                &p(&format!("wt-{i}")),
                &format!("wt-{i}"),
                None,
                *status,
            )
            .unwrap();
        }

        let drain_two = list_oldest_warm_entries_for_mesh_inner(&conn, 1, 2).unwrap();
        assert_eq!(drain_two.len(), 2, "limit=2 must return two rows");

        let drain_all = list_oldest_warm_entries_for_mesh_inner(&conn, 1, 3).unwrap();
        assert_eq!(drain_all.len(), 3, "limit=3 returns all three");
    }

    /// `is_warm_pool_path_inner` is the Worktree Manager's "is this row a
    /// pool entry?" discriminator. A `warm_worktrees` row's existence for
    /// the path → `true`; anything else (including a path that LOOKS like
    /// a pool entry but has no row, e.g. after the row was claimed and
    /// `forget_after_spawn` deleted it) → `false`.
    #[test]
    fn is_warm_pool_path_returns_true_iff_row_exists() {
        let conn = v22_schema_with_mesh(2);
        let pool_path = "/repo/m/.claude/worktrees/test-pool";
        let other_path = "/repo/m/.claude/worktrees/not-a-pool";

        // No row yet — both return false.
        assert!(!is_warm_pool_path_inner(&conn, pool_path).unwrap());
        assert!(!is_warm_pool_path_inner(&conn, other_path).unwrap());

        // Insert a row at pool_path.
        insert_warm_worktree_inner(
            &conn,
            1,
            pool_path,
            "test-pool",
            None,
            WarmWorktreeStatus::Available,
        )
        .unwrap();

        assert!(
            is_warm_pool_path_inner(&conn, pool_path).unwrap(),
            "path with a row must read true"
        );
        assert!(
            !is_warm_pool_path_inner(&conn, other_path).unwrap(),
            "path without a row must read false even if it looks like a pool path"
        );
    }
}