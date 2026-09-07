//! Tests for the `warm_worktrees` table + CRUD helpers (issue #609, v21 schema),
//! extended in issue #611 for per-mesh pool size + downsize drain.
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
        count_droppable_warm_entries_for_mesh_inner, delete_orphaned_claimed_warm_worktrees_inner,
        delete_warm_worktrees_for_mesh_inner, insert_warm_worktree_inner, is_warm_pool_path_inner,
        list_oldest_warm_entries_for_mesh_inner, list_warm_paths_for_mesh_droppable_inner,
        list_warm_paths_for_mesh_inner, list_warm_worktrees_to_reconcile_inner,
        list_worktree_enabled_meshes_for_warm_inner, mark_warm_worktree_available_inner,
        warm_pool_claims_path_inner, WarmWorktreeStatus,
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

        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();

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
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        // Second call must succeed without error.
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
    }

    /// Claim is atomic: two concurrent claims on the same mesh each see
    /// different rows (FIFO order). Without the `AND status = 'available'`
    /// guard on the UPDATE, both would happily return the same id and the
    /// second spawn would overwrite the first node's worktree path.
    #[test]
    fn claim_is_atomic_under_concurrent_call() {
        let conn = v20_schema_with_mesh(true);
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();

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
        assert_ne!(
            first.id, second.id,
            "concurrent claims must hand out distinct rows"
        );
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
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
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
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
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
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
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
        assert_eq!(
            ids,
            vec![ghost_id],
            "only the ghost row should be reconciled"
        );
        // The entry carries the path + a `dir_present=false` flag so the
        // reconcile skips the (pointless) git teardown and drops the row.
        assert_eq!(entries[0].path, "/this/path/does/not/exist/anywhere");
        assert!(
            !entries[0].dir_present,
            "a missing-dir entry must report dir_present=false"
        );
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
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
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
            ids,
            vec![filling_id],
            "an aged `filling` crash-orphan must be reconciled even when its directory exists"
        );
        assert!(
            entries[0].dir_present,
            "a live-directory entry must report dir_present=true"
        );
    }

    /// THE RACE GUARD: a FRESH `filling` row (a worker is mid-checkout right
    /// now) must NOT be reconciled. Without the age guard the reconcile would
    /// tear down a directory another thread's `create_git_worktree` is still
    /// writing (issue #610 review). The row is young (default `created_at`), so
    /// it stays put until it either flips to `available` or genuinely ages out.
    #[test]
    fn reconcile_skips_fresh_filling_row() {
        let conn = v20_schema_with_mesh(true);
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
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
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
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
        assert_eq!(
            ids,
            vec![id],
            "an aged `refreshing` crash-orphan must be reconciled"
        );
    }

    /// A `claimed` row is NEVER reconciled here — even aged, even dir-present:
    /// its directory may already back a live agent node's worktree, so tearing
    /// it down would destroy a running agent's work. `claimed` orphans are
    /// pruned (row-only) by `delete_orphaned_claimed_warm_worktrees` instead.
    #[test]
    fn reconcile_never_flags_claimed_rows() {
        let conn = v20_schema_with_mesh(true);
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
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

    /// `delete_orphaned_claimed_warm_worktrees_inner` (#639 gap 4, reworked
    /// for #697) is the orphan-row GC. It walks every `claimed` row and
    /// classifies each against the live session snapshot it receives:
    ///   * **Adopted (live agent present on this mesh)** — the row's
    ///     directory MAY back a live agent process, so the GC drops the row
    ///     but leaves the directory alone. Even when
    ///     `set_agent_node_worktree_name` silently failed upstream (the
    ///     #642.2 corner case), the live PROCESS_REGISTRY session alone is
    ///     sufficient evidence to refuse teardown — the spawn that
    ///     produced the session necessarily attached to that mesh.
    ///   * **Crashed-spawn orphan** — no live session on this mesh AND the
    ///     directory exists on disk. Tear down the git worktree metadata
    ///     (`remove_one_worktree`), then drop the row. The slug was baked
    ///     into the directory by the pool's checkout and survives a crash
    ///     that didn't reach `forget_after_spawn`, so this is the channel
    ///     for recovering the leak from a crashed mid-claim spawn.
    ///   * **Issue/PR mid-move / pre-missing** — no live session AND no
    ///     directory on disk. The Issue/PR spawn moves the directory onto a
    ///     `gh{N}-`/`pr{N}-` path (#612) before `forget_after_spawn`
    ///     fires; if the spawn crashes between the move and the row drop,
    ///     the original pool path is empty and the bookkeeping row is safe
    ///     to drop with no fs action.
    ///
    /// Called from `services::warm_pool::reconcile_on_startup` (step 1a).
    /// The live-session snapshot is `PROCESS_REGISTRY.session_ids()` at
    /// call time, snapshotted once so a concurrent spawn-then-die race
    /// can't reach into the GC's view mid-iteration.
    #[test]
    fn delete_orphaned_claimed_prunes_every_claimed_row() {
        let conn = v20_schema_with_mesh(true);
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        // Three claimed rows + two available rows. GC must touch ONLY the
        // claimed rows — the available rows are healthy pool inventory. Three
        // claimed rows (not one) so a future refactor that GC's only the
        // first matching row by index would surface as `deleted != 3`.
        for (name, sha) in [("ca", Some("aa")), ("cb", Some("bb")), ("cc", None)] {
            insert_warm_worktree_inner(
                &conn,
                1,
                tmp.path().join(name).to_str().unwrap(),
                name,
                sha,
                WarmWorktreeStatus::Claimed,
            )
            .unwrap();
        }
        for (name, sha) in [("aa", Some("aaa")), ("ab", Some("bbb"))] {
            insert_warm_worktree_inner(
                &conn,
                1,
                tmp.path().join(name).to_str().unwrap(),
                name,
                sha,
                WarmWorktreeStatus::Available,
            )
            .unwrap();
        }

        let deleted = delete_orphaned_claimed_warm_worktrees_inner(&conn, &[]).unwrap();
        assert_eq!(deleted, 3, "exactly the three claimed rows must be pruned");

        // The claimed rows are gone.
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM warm_worktrees WHERE status = 'claimed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0, "no claimed row may survive the GC");

        // The available rows survive — this is pool inventory the spawn path
        // needs intact.
        let available_remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM warm_worktrees WHERE status = 'available'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            available_remaining, 2,
            "every available row must survive the GC"
        );
    }

    // The `claimed`-row GC's safety contract is **per-row classification**,
    // not "never touches the filesystem" (#697 reworks the design from the
    // pre-#642 row-only GC). A claimed row is:
    //
    //   * **Adopted** (live PROCESS_REGISTRY session on the same mesh):
    //     drop the row, **preserve** the directory. The agent is alive;
    //     the directory is its CWD. Even when
    //     `set_agent_node_worktree_name` silently failed (#642.2 corner
    //     case), the live session alone is sufficient evidence.
    //   * **Orphan** (no live session, directory exists on disk):
    //     **tear down** the directory, drop the row. This is the leak
    //     #697 closes — a crashed mid-claim spawn left the directory
    //     without a corresponding row drop.
    //   * **Mid-move / pre-missing** (no live session, no directory):
    //     drop the row only (no fs action).
    //
    // The four tests below pin each branch and the #642.2 corner case.
    // The old `delete_orphaned_claimed_does_not_touch_directories` test
    // (pre-#697, "never touches the filesystem" — see PR #696) was
    // obsolete by design: that contract is exactly the leak the new
    // PROCESS_REGISTRY-based algorithm exists to close. The replacement
    // tests use the looser "live session on this mesh ⇒ preserve" guard,
    // which is safe against the silent-UPDATE-failure corner case AND
    // sharp enough to recover orphan dirs.

    // -----------------------------------------------------------------------
    // Issue #697 — PROCESS_REGISTRY-based claimed-row GC
    //
    // The original GC (#639 gap 4, hardened in #642.2, reverted for data
    // loss) used a DB-only check that misclassified rows when both
    // `set_agent_node_worktree_name` and `forget_after_spawn` silently
    // failed. The PROCESS_REGISTRY is the source of truth for "is there
    // a live agent on this mesh?" — a live session is sufficient evidence
    // to refuse teardown, no matter what the DB's `agent_nodes.worktree_name`
    // says.
    //
    // The four tests below pin the new contract's three branches
    // (live-agent-preserve, orphan-teardown, mid-move-drop) plus the
    // #642.2 corner case that drove the revert in the first place.
    // -----------------------------------------------------------------------

    /// Build a v22-ish schema that ALSO carries an `agent_nodes` table. The
    /// live-session classification reads `agent_nodes.mesh_id` to decide
    /// whether a session counts as "live on the warm row's mesh", so the
    /// table must exist for the GC's `IN (...)` query to do anything other
    /// than return an empty set.
    fn v22_schema_with_agent_nodes(pool_size: i64) -> Connection {
        let conn = v22_schema_with_mesh(pool_size);
        // Mirror the production CREATE TABLE from `db::init` (just the
        // columns the GC's `SELECT DISTINCT mesh_id WHERE id IN (...)`
        // actually references). The full production schema also gets the
        // safety-net column adds; a test that grew a column add wouldn't
        // trip here, but it would trip the production call sites that
        // SELECT through the column.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_nodes (
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
                 position INTEGER NOT NULL DEFAULT 0,
                 created_at TEXT NOT NULL DEFAULT (datetime('now')),
                 status_changed_at TEXT NOT NULL DEFAULT (datetime('now')),
                 source_pr INTEGER,
                 head_repo_owner TEXT,
                 head_repo_clone_url TEXT,
                 source_pr_pinned_sha TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_agent_nodes_mesh ON agent_nodes(mesh_id);",
        )
        .unwrap();
        conn
    }

    /// Pin the **adopted** branch: a live PROCESS_REGISTRY session on the
    /// warm row's mesh means the directory MAY back a live agent, so the
    /// GC must drop the row but leave the directory INTACT. Without this
    /// guarantee the GC would tear down a live agent's working tree on
    /// the next startup reconcile.
    #[test]
    fn delete_orphaned_claimed_preserves_dir_when_session_live_on_mesh() {
        let conn = v22_schema_with_agent_nodes(0);
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("claimed-live");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("CLAIMED.md"), "live agent's work").unwrap();

        // mesh_id = 1, claimed row at the live dir's path.
        let _row_id = insert_warm_worktree_inner(
            &conn,
            1,
            dir.to_str().unwrap(),
            "claimed-live",
            Some("dd"),
            WarmWorktreeStatus::Claimed,
        )
        .unwrap();

        // A live session on mesh 1: register an agent_nodes row keyed by
        // the session_id. We then pass `[session_id]` as the live snapshot.
        conn.execute(
            "INSERT INTO agent_nodes (mesh_id, name, path, worktree_name) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![1_i64, "live-agent", "/some/host/mesh", "claimed-live"],
        )
        .unwrap();
        let session_id = conn.last_insert_rowid();

        let n =
            delete_orphaned_claimed_warm_worktrees_inner(&conn, std::slice::from_ref(&session_id))
                .unwrap();
        assert_eq!(n, 1, "the claimed row must be pruned");

        // Row gone, directory INTACT.
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM warm_worktrees WHERE status = 'claimed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            remaining, 0,
            "the GC must delete the bookkeeping row even when preserving the directory"
        );
        assert!(
            dir.join("CLAIMED.md").exists(),
            "the GC must NOT tear down a directory when a live PROCESS_REGISTRY \
             session on the same mesh exists — the dir may back the live agent's \
             CWD, even if the DB's `agent_nodes.worktree_name` is stale"
        );
    }

    /// Pin the **crashed-spawn orphan** branch (the hole #697 closes): no
    /// live session on the mesh + directory exists on disk = the spawn
    /// crashed mid-claim without `forget_after_spawn` firing, leaving a
    /// `.claude/worktrees/<slug>/` directory that nobody owns. The GC must
    /// tear it down so the directory doesn't leak across restarts.
    #[test]
    fn delete_orphaned_claimed_tears_down_orphan_dir_when_no_session() {
        let conn = v22_schema_with_agent_nodes(0);
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("claimed-orphan");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ORPHAN.md"), "abandoned by a crashed spawn").unwrap();

        insert_warm_worktree_inner(
            &conn,
            1,
            dir.to_str().unwrap(),
            "claimed-orphan",
            Some("dd"),
            WarmWorktreeStatus::Claimed,
        )
        .unwrap();

        // Empty live-session snapshot = no PROCESS_REGISTRY entry on this mesh.
        let n = delete_orphaned_claimed_warm_worktrees_inner(&conn, &[]).unwrap();
        assert_eq!(n, 1);

        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM warm_worktrees WHERE status = 'claimed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0, "the orphan row must be deleted");

        assert!(
            !dir.exists(),
            "the GC must tear down an orphan dir (no live PROCESS_REGISTRY \
             session on this mesh) — this is the leak #697 closes"
        );
    }

    /// Pin the **mid-move / pre-missing** branch: no live session + no
    /// directory on disk = the spawn is gone AND its directory was already
    /// moved away (Issue/PR #612) or hand-deleted by the user. The row is
    /// a bookkeeping artefact with nothing on disk to clean up; the GC
    /// just drops it.
    #[test]
    fn delete_orphaned_claimed_drops_row_when_dir_missing() {
        let conn = v22_schema_with_agent_nodes(0);
        // Path deliberately under a unique key that does NOT exist on disk.
        // (We don't point at `tmp.path()` because the GC's `remove_one_worktree`
        // would race against the tempdir teardown.)
        let ghost_path = "/this/path/does/not/exist/anywhere/claimed-ghost";

        insert_warm_worktree_inner(
            &conn,
            1,
            ghost_path,
            "claimed-ghost",
            Some("dd"),
            WarmWorktreeStatus::Claimed,
        )
        .unwrap();

        let n = delete_orphaned_claimed_warm_worktrees_inner(&conn, &[]).unwrap();
        assert_eq!(
            n, 1,
            "the missing-dir `claimed` row must still be pruned (the row-only \
             path is the same as it was pre-#697 — the row leak is the part \
             this PR closes; the missing-dir case was never the issue)"
        );

        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM warm_worktrees WHERE status = 'claimed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
    }

    /// Pin the **#642.2 corner case** (the data-loss path that drove the
    /// revert that #697 supersedes):
    ///
    ///   * A spawn claimed this warm pool row → agent_nodes row exists →
    ///     PROCESS_REGISTRY has the session.
    ///   * `set_agent_node_worktree_name(session_id, &preassigned_name)`
    ///     silently failed (DB lock timeout, etc.) — so the agent's
    ///     `worktree_name` STAYS at the stage-1 throwaway slug and no
    ///     longer matches `warm.preassigned_name`.
    ///   * `forget_after_spawn` also silently failed → the warm row is
    ///     still at `claimed`.
    ///
    /// At GC time: a live PROCESS_REGISTRY session exists whose
    /// `agent_nodes.worktree_name` is the throwaway slug (≠ `preassigned_name`).
    /// A path-equality check ("derived worktree path == warm.path") would
    /// misclassify this row as orphan and tear down the live agent's CWD.
    /// The PROCESS_REGISTRY-based check (`any live session on this mesh?`)
    /// correctly refuses to tear down, because the spawn that claimed this
    /// row necessarily attaches to this mesh. **This test pins the
    /// data-loss guard for the issue #697 algorithm.**
    #[test]
    fn delete_orphaned_claimed_preserves_dir_when_worktree_name_stale() {
        let conn = v22_schema_with_agent_nodes(0);
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("claimed-stale");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("STALE.md"), "live agent whose DB row is stale").unwrap();

        // Warm row adopts the pool-preassigned slug `claimed-stale`.
        insert_warm_worktree_inner(
            &conn,
            1,
            dir.to_str().unwrap(),
            "claimed-stale",
            Some("dd"),
            WarmWorktreeStatus::Claimed,
        )
        .unwrap();

        // Live agent nodes row whose `worktree_name` is the THROWAWAY stage-1
        // slug — `set_agent_node_worktree_name` silently failed and left the
        // column at its stage-1 value. The derived worktree path would be
        // `<mesh>/.claude/worktrees/untitled-stage-1/`, NOT
        // `<mesh>/.claude/worktrees/claimed-stale/`. **A path-equality GC
        // would tear this dir down — the data-loss bug.**
        conn.execute(
            "INSERT INTO agent_nodes (mesh_id, name, path, worktree_name) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                1_i64,
                "ghost-name",
                "/some/host/mesh",
                "untitled-stage-1" // NOT "claimed-stale"
            ],
        )
        .unwrap();
        let session_id = conn.last_insert_rowid();

        let n =
            delete_orphaned_claimed_warm_worktrees_inner(&conn, std::slice::from_ref(&session_id))
                .unwrap();
        assert_eq!(n, 1, "the claimed row must be pruned");

        // The dir MUST be preserved — a live process exists on this mesh,
        // even if the DB's `worktree_name` doesn't reflect it.
        assert!(
            dir.join("STALE.md").exists(),
            "the GC must NOT tear down a directory when a live PROCESS_REGISTRY \
             session on the same mesh exists even if `agent_nodes.worktree_name` \
             is stale (the #642.2 data-loss corner case — silent best-effort \
             UPDATE failure upstream of the GC's view must not be misread as 'orphan')"
        );
    }

    /// Pin **multi-row, mixed-classification across two meshes**: mesh 1 has
    /// a live session (preserve its claimed row + dir); mesh 2 has NO live
    /// session and a real orphan dir on disk (tear down). This exercises
    /// the per-`mesh_id` lookup and ensures the GC isn't treating a single
    /// session as a global "preserve everything" override.
    ///
    /// NOTE: the GC's "any live session on this mesh preserves ALL claimed
    /// rows on that mesh" trade-off (vs the issue body's strict
    /// path-equality check) is intentional — it's the looser/safer
    /// classification that closes the #642.2 silent-UPDATE-failure bug.
    /// A live session on mesh 1 means a row on mesh 1 is treated as
    /// adopted; it does NOT mean rows on mesh 2 are adopted. This test
    /// pins both halves of that distinction in a single GC pass.
    #[test]
    fn delete_orphaned_claimed_mixed_adopted_and_orphan_across_meshes() {
        let conn = v22_schema_with_agent_nodes(0);
        // Add a second mesh so we can put the orphan on a different mesh
        // than the live session.
        conn.execute(
            "INSERT INTO meshes (name, path) VALUES ('m2', '/repo/m2')",
            [],
        )
        .unwrap();
        let mesh_2_id: i64 = conn.last_insert_rowid();

        let tmp = tempfile::TempDir::new().unwrap();

        // mesh 1, claimed row backed by a live session.
        let live_dir = tmp.path().join("mesh1-live");
        std::fs::create_dir_all(&live_dir).unwrap();
        std::fs::write(live_dir.join("LIVE.md"), "live agent on mesh 1").unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            live_dir.to_str().unwrap(),
            "mesh1-live",
            Some("ll"),
            WarmWorktreeStatus::Claimed,
        )
        .unwrap();

        // mesh 1, mid-move claimed row (path doesn't exist on disk).
        insert_warm_worktree_inner(
            &conn,
            1,
            "/this/path/does/not/exist/mid-move",
            "mesh1-midmove",
            Some("mm"),
            WarmWorktreeStatus::Claimed,
        )
        .unwrap();

        // mesh 2, claimed row with NO live session + on-disk dir = orphan.
        let orphan_dir = tmp.path().join("mesh2-orphan");
        std::fs::create_dir_all(&orphan_dir).unwrap();
        std::fs::write(orphan_dir.join("ORPHAN.md"), "abandoned on mesh 2").unwrap();
        insert_warm_worktree_inner(
            &conn,
            mesh_2_id,
            orphan_dir.to_str().unwrap(),
            "mesh2-orphan",
            Some("oo"),
            WarmWorktreeStatus::Claimed,
        )
        .unwrap();

        // Live PROCESS_REGISTRY session is on mesh 1 only.
        conn.execute(
            "INSERT INTO agent_nodes (mesh_id, name, path, worktree_name) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![1_i64, "live-agent", "/some/host/mesh1", "mesh1-live"],
        )
        .unwrap();
        let live_session_id = conn.last_insert_rowid();

        let n = delete_orphaned_claimed_warm_worktrees_inner(
            &conn,
            std::slice::from_ref(&live_session_id),
        )
        .unwrap();
        assert_eq!(n, 3, "all three claimed rows must be pruned");

        // mesh 1 dirs survive (live session on this mesh).
        assert!(
            live_dir.join("LIVE.md").exists(),
            "the mesh-1 claimed row's dir must be preserved (live session on mesh 1)"
        );
        // mesh 2 orphan dir GONE (no live session on mesh 2).
        assert!(
            !orphan_dir.exists(),
            "the mesh-2 claimed row's dir must be torn down (no live session on mesh 2)"
        );
        // Mid-move row's path was already absent; nothing to verify on disk.
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM warm_worktrees", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, 0, "every `claimed` row must be pruned");
    }

    /// Pin the **safety fallback** for `live_mesh_ids_for` SQL errors: when
    /// the GC cannot resolve the live-session snapshot to `mesh_id`s
    /// (transient DB error, schema mismatch, dropped `agent_nodes`
    /// table — any reason), it MUST fall back to row-only behaviour and
    /// leave the on-disk directories intact. Treating the snapshot as
    /// "no live sessions anywhere" would let the loop tear down a live
    /// agent's CWD on a transient error — the same data-loss class the
    /// PROCESS_REGISTRY check exists to avoid. The contract is: an error
    /// means "couldn't tell" → err on the side of NOT touching the
    /// filesystem; a snapshot of zero sessions means "no live sessions"
    /// → orphan teardown is correct (separately tested above).
    ///
    /// We trigger the fallback by dropping the `agent_nodes` table (so
    /// the `IN (SELECT ... FROM agent_nodes ...)` query errors out at
    /// prepare), then call the GC with a non-empty live snapshot.
    #[test]
    fn delete_orphaned_claimed_falls_back_to_row_only_on_live_mesh_lookup_error() {
        // Start from the v22-with-agent_nodes fixture so the live snapshot
        // has SOMETHING to look up; then drop `agent_nodes` to force the
        // prepare to error. This mirrors the post-init schema-drift
        // failure mode the safety fallback exists to survive.
        let conn = v22_schema_with_agent_nodes(0);
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("claimed-on-disk");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("MAYBE-LIVE.md"), "don't tear this down").unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            dir.to_str().unwrap(),
            "claimed-on-disk",
            Some("dd"),
            WarmWorktreeStatus::Claimed,
        )
        .unwrap();

        // Drop the `agent_nodes` table — the live_mesh_ids_for query
        // SELECTs from it, so prepare() will fail. This is the only
        // production-realistic way I can simulate "snapshot says non-
        // empty but I couldn't tell which meshes" without re-architecting
        // the helper to take an injected error source. In production the
        // same condition triggers on a transient DB error during startup
        // reconcile.
        conn.execute("DROP TABLE agent_nodes", []).unwrap();
        let live_snapshot = vec![999_i64, 998_i64]; // arbitrary non-empty

        let n = delete_orphaned_claimed_warm_worktrees_inner(&conn, &live_snapshot).unwrap();
        assert_eq!(
            n, 1,
            "the row must still be pruned even when the live lookup errors \
             (row-only behaviour matches the pre-#697 contract)"
        );

        // Row gone, directory INTACT — the safety fallback prevents
        // the GC from destroying a working tree on a transient query error.
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM warm_worktrees WHERE status = 'claimed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
        assert!(
            dir.join("MAYBE-LIVE.md").exists(),
            "the GC must NOT touch the filesystem when the live-lookup query \
             errors — a transient DB failure is not an excuse to destroy a \
             working tree (data-loss guard for the safety fallback)"
        );
    }

    /// Issue #1228 — the `delete_orphaned_claimed_warm_worktrees` GC must
    /// release the global writer mutex during its filesystem teardown so
    /// concurrent DB writes (attention flips, status writes, HTTP auth
    /// token validation) do not queue behind `git worktree remove --force`
    /// + recursive `remove_dir_all` for many seconds during startup
    /// reconcile.
    ///
    /// This test pins the **write-side** invariant directly. Pre-fix, the
    /// GC held `write_conn()` for the entire FS phase — every other thread
    /// that needed the writer mutex queued behind it. Post-fix, the GC
    /// drops the mutex after Phase 1 (snapshot + classify), runs the FS
    /// work lock-free, and re-acquires only for Phase 3 (batch DELETE).
    ///
    /// The probe write we issue concurrently is `delete_warm_worktree(id)`
    /// on an **unrelated** Available row — a row the GC does not touch, so
    /// any latency above normal query time is attributable to writer-mutex
    /// contention, not to a real lock conflict.
    ///
    /// Note: this test calls `db::init` and therefore contends with every
    /// other global-DB test in the binary for the process-wide `DB`
    /// OnceCell (first-init-wins). The `static GFS_LOCK` below mirrors
    /// the `MESH_TESTS_LOCK` pattern in `mesh_tests` — a module-level
    /// mutex keeps our init / seed / GC / probe sequence atomic against
    /// our own re-entry, and unique IDs (`test_id`-based row names) keep
    /// us isolated from whichever other test file happened to init first.
    #[test]
    fn delete_orphaned_claimed_warm_worktrees_releases_writer_mutex_during_fs_teardown() {
        static GFS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _serial = GFS_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let test_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db_path =
            std::env::temp_dir().join(format!("buildmesh_issue_1228_fs_lock_test_{}.db", test_id));
        // First-init-wins: a no-op if another test file in this binary
        // already initialised the global DB with a different path. Either
        // way the `write_conn()` we hold below targets the live writer.
        crate::db::init(&db_path).expect("test setup: db::init must succeed");

        // Use a unique mesh name so we don't collide with rows other
        // global-DB tests may have written into the shared singleton.
        let mesh_name = format!("issue-1228-mesh-{}", test_id);
        let mesh_id: i64 = {
            let conn = crate::db::write_conn();
            conn.execute(
                "INSERT INTO meshes (name, path) VALUES (?1, ?2)",
                rusqlite::params![&mesh_name, &format!("/repo/issue-1228-{}", test_id)],
            )
            .unwrap();
            conn.last_insert_rowid()
        };

        // Seed an "unrelated" Available row the probe write will target.
        // The probe only needs to acquire the writer mutex briefly; using
        // a row the GC doesn't touch means any latency is purely mutex
        // contention, not a real lock conflict.
        let probe_row_path = format!("/repo/probe-row-{}", test_id);
        let probe_row_id: i64 = {
            let conn = crate::db::write_conn();
            conn.execute(
                "INSERT INTO warm_worktrees \
                 (mesh_id, path, preassigned_name, base_sha, status) \
                 VALUES (?1, ?2, ?3, NULL, 'available')",
                rusqlite::params![mesh_id, &probe_row_path, "probe-row"],
            )
            .unwrap();
            conn.last_insert_rowid()
        };

        // Seed N claimed rows pointing at heavy tempdirs so the FS phase
        // takes long enough to observe the lock-free property. The dirs
        // are NOT git worktrees — `remove_one_worktree` returns Err
        // immediately for them and the loop falls through to
        // `remove_dir_all`, which is the realistic production case for
        // orphan dirs that lost their git metadata. 30 dirs * 100 files
        // is enough to reliably push `remove_dir_all` into the 100ms+
        // range on Windows-with-antivirus and is still cheap to set up
        // on fast Linux CI runners.
        const N_ROWS: usize = 30;
        const FILES_PER_DIR: usize = 100;
        let mut dirs: Vec<(tempfile::TempDir, String)> = Vec::with_capacity(N_ROWS);
        for i in 0..N_ROWS {
            let tmp = tempfile::TempDir::new().unwrap();
            for j in 0..FILES_PER_DIR {
                std::fs::write(tmp.path().join(format!("f{}.bin", j)), vec![0xAB_u8; 128])
                    .expect("seed: write fixture file");
            }
            let path = tmp.path().to_str().unwrap().to_string();
            {
                let conn = crate::db::write_conn();
                conn.execute(
                    "INSERT INTO warm_worktrees \
                     (mesh_id, path, preassigned_name, base_sha, status) \
                     VALUES (?1, ?2, ?3, NULL, 'claimed')",
                    rusqlite::params![mesh_id, &path, format!("orphan-{}", i)],
                )
                .unwrap();
            }
            dirs.push((tmp, path));
        }

        // Spawn the GC on a dedicated thread. It runs the production
        // entry point, which is the function the issue targets (and the
        // only one that takes `write_conn()`).
        let (gc_done_tx, gc_done_rx) = std::sync::mpsc::channel::<()>();
        let gc_handle = std::thread::spawn(move || {
            let _ = crate::db::delete_orphaned_claimed_warm_worktrees();
            let _ = gc_done_tx.send(());
        });

        // Wait briefly so the GC reaches its FS phase. Phase 1 (two SELECTs
        // over `warm_worktrees` + `agent_nodes`) takes microseconds;
        // Phase 3 (N DELETEs) takes microseconds. Only the FS phase is
        // slow, so a short sleep after spawn reliably lands us inside it.
        // 100ms is a generous upper bound for "Phase 1 finished".
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Probe: time how long a competing writer takes to acquire
        // `write_conn()` and run a single DELETE. Pre-fix, this blocks
        // for the entire FS phase (could be many seconds on slow disks);
        // post-fix, it completes in normal query time.
        let probe_start = std::time::Instant::now();
        crate::db::delete_warm_worktree(probe_row_id).expect("probe write must succeed");
        let probe_duration = probe_start.elapsed();

        assert!(
            probe_duration < std::time::Duration::from_secs(2),
            "probe write took {:?} while GC's FS phase was running — \
             write_conn() is being held during filesystem teardown, which \
             is exactly what issue #1228 forbids. The mutex must be \
             released between Phase 1 (classify) and Phase 3 (DELETE) so \
             concurrent writes (attention flips, status writes, HTTP auth \
             token validation) do not queue behind remove_one_worktree / \
             remove_dir_all for many seconds during startup reconcile.",
            probe_duration
        );

        // Sanity: the probe DELETE really did remove the row.
        let still_present: i64 = crate::db::read_conn()
            .query_row(
                "SELECT COUNT(*) FROM warm_worktrees WHERE id = ?1",
                rusqlite::params![probe_row_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            still_present, 0,
            "the probe DELETE must remove exactly the unrelated Available row"
        );

        // Wait for the GC to finish so we can clean up the tempdirs and
        // the DB file without racing the FS phase.
        gc_done_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("GC must finish within 30s");
        gc_handle.join().unwrap();

        // Tidy up. Drop the dirs first (so the TempDir destructor doesn't
        // try to re-remove paths the GC already deleted); then drop the
        // write_conn guard before deleting the file.
        drop(dirs);
        drop(crate::db::write_conn());
        std::fs::remove_file(&db_path).ok();
    }

    /// `delete_warm_worktrees_for_mesh_inner` is the mesh-delete hook (wired
    /// into `delete_mesh`): when a mesh is deleted, every pool row pointing at
    /// its worktrees must go too — the FK cascade is off, so without this the
    /// rows outlive the mesh as orphans.
    #[test]
    fn delete_for_mesh_removes_every_row_for_that_mesh() {
        let conn = v20_schema_with_mesh(true);
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
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

    /// `list_warm_paths_for_mesh_inner` (issue #639 gap 3, retained for
    /// diagnostics under #642) returns every pool row's `path` for the
    /// given mesh, regardless of status. The mesh-delete path (`#642.1`)
    /// uses the safer `list_warm_paths_for_mesh_droppable_inner` instead,
    /// but the "everything" view is kept for diagnostic / audit tools.
    ///
    /// Exercises ALL four statuses (`Available`, `Filling`, `Refreshing`,
    /// `Claimed`) so a future refactor that narrows the WHERE clause to a
    /// subset (e.g. mirroring `count_droppable_warm_entries_for_mesh_inner`'s
    /// `status != 'claimed'` filter) fails this test rather than silently
    /// leaking `Refreshing` rows during a mesh delete.
    #[test]
    fn list_paths_returns_every_pool_row_path_for_mesh() {
        let conn = v20_schema_with_mesh(true);
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        // Mesh 1 owns one row per status — every status must come back.
        let path_a = tmp.path().join("a").to_str().unwrap().to_string();
        let path_b = tmp.path().join("b").to_str().unwrap().to_string();
        let path_c = tmp.path().join("c").to_str().unwrap().to_string();
        let path_d = tmp.path().join("d").to_str().unwrap().to_string();
        insert_warm_worktree_inner(
            &conn,
            1,
            &path_a,
            "pool-warm-a",
            Some("a"),
            WarmWorktreeStatus::Available,
        )
        .unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            &path_b,
            "pool-warm-b",
            None,
            WarmWorktreeStatus::Filling,
        )
        .unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            &path_c,
            "pool-warm-c",
            None,
            WarmWorktreeStatus::Refreshing,
        )
        .unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            &path_d,
            "pool-warm-d",
            Some("c"),
            WarmWorktreeStatus::Claimed,
        )
        .unwrap();
        // Mesh 2 (added manually) owns one row that must NOT appear.
        conn.execute(
            "INSERT INTO meshes (name, path) VALUES ('m2', '/repo/m2')",
            [],
        )
        .unwrap();
        let m2_id: i64 = conn.last_insert_rowid();
        let path_m2 = tmp.path().join("m2").to_str().unwrap().to_string();
        insert_warm_worktree_inner(
            &conn,
            m2_id,
            &path_m2,
            "pool-warm-m2",
            Some("z"),
            WarmWorktreeStatus::Available,
        )
        .unwrap();

        let paths = list_warm_paths_for_mesh_inner(&conn, 1).unwrap();
        let mut got = paths.clone();
        got.sort();
        let mut expected = vec![path_a, path_b, path_c, path_d];
        expected.sort();
        assert_eq!(
            got, expected,
            "every pool row path for mesh 1 must come back, regardless of status (Available + Filling + Refreshing + Claimed)"
        );
        // And mesh-2's row is NOT included.
        assert!(
            !paths.contains(&path_m2),
            "a path belonging to a different mesh must not leak into the list"
        );
    }

    /// `list_warm_paths_for_mesh_droppable_inner` (#642.1) is the
    /// force-remove-safe view: every status EXCEPT `claimed`. The mesh-delete
    /// path uses this view because a `claimed` row's directory may back a
    /// live agent process — force-removing it would destroy the agent's
    /// working tree. Mirrors the `status != 'claimed'` filter convention used
    /// by `count_droppable_warm_entries_for_mesh_inner` and
    /// `list_oldest_warm_entries_for_mesh_inner` (#613).
    #[test]
    fn list_paths_droppable_excludes_claimed_rows() {
        let conn = v20_schema_with_mesh(true);
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        // Available + Filling + Refreshing come back; Claimed is dropped.
        let path_a = tmp.path().join("a").to_str().unwrap().to_string();
        let path_b = tmp.path().join("b").to_str().unwrap().to_string();
        let path_c = tmp.path().join("c").to_str().unwrap().to_string();
        let path_d = tmp.path().join("d").to_str().unwrap().to_string();
        insert_warm_worktree_inner(
            &conn,
            1,
            &path_a,
            "pool-warm-a",
            Some("a"),
            WarmWorktreeStatus::Available,
        )
        .unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            &path_b,
            "pool-warm-b",
            None,
            WarmWorktreeStatus::Filling,
        )
        .unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            &path_c,
            "pool-warm-c",
            None,
            WarmWorktreeStatus::Refreshing,
        )
        .unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            &path_d,
            "pool-warm-d",
            Some("d"),
            WarmWorktreeStatus::Claimed,
        )
        .unwrap();

        let paths = list_warm_paths_for_mesh_droppable_inner(&conn, 1).unwrap();
        let mut got = paths.clone();
        got.sort();
        let mut expected = vec![path_a, path_b, path_c];
        expected.sort();
        assert_eq!(
            got, expected,
            "the droppable view must include Available + Filling + Refreshing \
             but exclude Claimed (whose dir may back a live agent)"
        );
        assert!(
            !paths.contains(&path_d),
            "the claimed row's path must not appear in the droppable view"
        );
    }

    /// A mesh with no pool rows returns an empty list (not an error) so the
    /// caller can use the same code path for meshes that never warmed
    /// anything.
    #[test]
    fn list_paths_returns_empty_when_mesh_has_no_pool_rows() {
        let conn = v20_schema_with_mesh(true);
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        let paths = list_warm_paths_for_mesh_inner(&conn, 1).unwrap();
        assert!(
            paths.is_empty(),
            "a mesh with zero pool rows must read as an empty list"
        );
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
                pre_spawn_pool_size INTEGER NOT NULL DEFAULT 0,
                -- v37 (issue #1519): per-mesh worktree container override.
                -- Listed here (not grown via evolve_to) for the same
                -- reason: the projection under test must find it.
                worktree_directory TEXT
            );
            INSERT INTO meshes (name, path, use_worktree) VALUES ('enabled', '/r/enabled', 1);
            INSERT INTO meshes (name, path, use_worktree) VALUES ('disabled', '/r/disabled', 0);
            ",
        )
        .unwrap();

        let rows = list_worktree_enabled_meshes_for_warm_inner(&conn).unwrap();
        assert_eq!(
            rows.len(),
            1,
            "only the worktree-enabled mesh must be listed"
        );
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
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
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

    /// The drain must NOT count a `claimed` row against the target (issue
    /// #613 review): a claimed entry is a worktree in transition to a live
    /// agent node, not pool inventory. `count_droppable_warm_entries_for_mesh`
    /// excludes it, so the idle drain can never see "3 entries, target 2,
    /// excess 1" and go on to force-remove the live worktree.
    #[test]
    fn count_droppable_excludes_claimed_rows() {
        let conn = v22_schema_with_mesh(3);
        // Two available, one filling, one claimed.
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
        insert_warm_worktree_inner(&conn, 1, &p("c"), "c", None, WarmWorktreeStatus::Filling)
            .unwrap();
        insert_warm_worktree_inner(&conn, 1, &p("d"), "d", None, WarmWorktreeStatus::Claimed)
            .unwrap();

        // 2 available + 1 filling = 3 droppable; the claimed row is excluded.
        let droppable = count_droppable_warm_entries_for_mesh_inner(&conn, 1).unwrap();
        assert_eq!(
            droppable, 3,
            "droppable count must exclude the claimed row (it's a live node's worktree)"
        );

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

    /// A `claimed` row must NEVER be a drain candidate (issue #613 review):
    /// its directory is being adopted as a live agent node's worktree, so
    /// force-removing it would delete the agent's work. Even when a claimed
    /// row is the oldest entry and the limit would otherwise reach it, the
    /// candidate scan must skip it and return only the droppable rows.
    #[test]
    fn list_oldest_warm_entries_excludes_claimed() {
        let conn = v22_schema_with_mesh(1);
        let tmp = tempfile::TempDir::new().unwrap();
        let p = |name: &str| tmp.path().join(name).to_str().unwrap().to_string();
        // Claimed inserted FIRST (oldest) — must still be skipped.
        let claimed_id = insert_warm_worktree_inner(
            &conn,
            1,
            &p("old-claimed"),
            "old-claimed",
            Some("cc"),
            WarmWorktreeStatus::Claimed,
        )
        .unwrap();
        let avail_id = insert_warm_worktree_inner(
            &conn,
            1,
            &p("new-available"),
            "new-available",
            Some("aa"),
            WarmWorktreeStatus::Available,
        )
        .unwrap();

        // Ask for up to 5 candidates: only the one available row qualifies.
        let picks = list_oldest_warm_entries_for_mesh_inner(&conn, 1, 5).unwrap();
        let ids: Vec<i64> = picks.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            vec![avail_id],
            "only the available row is a drain candidate; the claimed row must be excluded"
        );
        assert!(
            !ids.contains(&claimed_id),
            "a claimed (live-node) worktree must never be selected for force-removal"
        );
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

    /// `warm_pool_claims_path_inner` is the deleter-side guard for issue #653:
    /// the pending-removal drain calls it before deciding whether a
    /// `pending_worktree_removals` entry is safe to act on. ONLY a row with
    /// `status = 'claimed'` matches — `available`, `filling`, and `refreshing`
    /// rows are pool inventory or in-flight worker state, not a live spawn's
    /// worktree, so they must NOT block the drain.
    ///
    /// Crucially this is NOT a generalised "warm pool row exists?" predicate:
    /// the older `is_warm_pool_path_inner` returns true for every status and is
    /// still used by `collect_prune_info` (which legitimately cares about any
    /// row at the path). The pending-removal drain needs the narrower contract
    /// — "is this path owned by a live spawn right now?" — and a future
    /// refactor that collapsed the two would either (a) block legitimate
    /// `available` row drains or (b) let a `claimed` row's directory be
    /// deleted out from under a live spawn, both regressions.
    #[test]
    fn warm_pool_claims_path_inner_returns_true_only_for_claimed() {
        let conn = v22_schema_with_mesh(2);
        let tmp = tempfile::TempDir::new().unwrap();
        let p = |name: &str| tmp.path().join(name).to_str().unwrap().to_string();

        let avail_path = p("avail");
        let fill_path = p("filling");
        let refr_path = p("refreshing");
        let claim_path = p("claimed");

        insert_warm_worktree_inner(
            &conn,
            1,
            &avail_path,
            "avail",
            Some("aa"),
            WarmWorktreeStatus::Available,
        )
        .unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            &fill_path,
            "filling",
            None,
            WarmWorktreeStatus::Filling,
        )
        .unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            &refr_path,
            "refreshing",
            Some("rr"),
            WarmWorktreeStatus::Refreshing,
        )
        .unwrap();
        insert_warm_worktree_inner(
            &conn,
            1,
            &claim_path,
            "claimed",
            Some("cc"),
            WarmWorktreeStatus::Claimed,
        )
        .unwrap();

        assert!(
            warm_pool_claims_path_inner(&conn, &claim_path).unwrap(),
            "a `claimed` row must read true — the drain must skip the pending removal"
        );
        // Every other status reads false: the drain may proceed to remove
        // these (or, in the case of `filling`/`refreshing`, the worker
        // itself owns the teardown).
        assert!(
            !warm_pool_claims_path_inner(&conn, &avail_path).unwrap(),
            "an `available` row is pool inventory, not a live spawn — drain must proceed"
        );
        assert!(
            !warm_pool_claims_path_inner(&conn, &fill_path).unwrap(),
            "a `filling` row is mid-checkout by the pool worker — drain must proceed"
        );
        assert!(
            !warm_pool_claims_path_inner(&conn, &refr_path).unwrap(),
            "a `refreshing` row is mid-`git reset --hard` by the freshness pass — drain must proceed"
        );
        // And a path with no row at all reads false (defends the drain
        // against a stale-pending-removal that was enqueued for a path
        // whose row was dropped between enqueue and drain).
        assert!(
            !warm_pool_claims_path_inner(&conn, "/no/row/here").unwrap(),
            "a path with no row must read false"
        );
    }

    /// After the spawn completes successfully and `forget_after_spawn` drops
    /// the row, the path is no longer claimed — so the next drain must
    /// proceed to remove it. This is the post-condition that closes the
    /// "skip without dequeuing" loop: claim supersedes tombstone intent,
    /// and once the claim ends (the row is dropped), the tombstone is
    /// consumed and the directory goes.
    #[test]
    fn warm_pool_claims_path_inner_returns_false_after_row_dropped() {
        let conn = v22_schema_with_mesh(1);
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().to_str().unwrap().to_string();
        let id = insert_warm_worktree_inner(
            &conn,
            1,
            &p,
            "claimed",
            Some("cc"),
            WarmWorktreeStatus::Claimed,
        )
        .unwrap();
        assert!(warm_pool_claims_path_inner(&conn, &p).unwrap());
        // Simulate forget_after_spawn's row drop.
        conn.execute(
            "DELETE FROM warm_worktrees WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap();
        assert!(
            !warm_pool_claims_path_inner(&conn, &p).unwrap(),
            "after the row is dropped, the path must no longer be claimed — the next drain proceeds"
        );
    }

    // -------------------------------------------------------------------
    // v24 pool-default-on backfill (ADR 0020). One-time flip of
    // worktree-enabled meshes from the old pool-off default (0) to the new
    // default (1), gated on the `pool_default_backfill_v24` flag.
    // -------------------------------------------------------------------

    /// Build a v22-era DB with three meshes covering the backfill's
    /// decision table: worktree-enabled at 0 (flips), worktree-disabled at
    /// 0 (stays), worktree-enabled with an explicit size (stays).
    ///
    /// Setup order: a v20 schema (no `pre_spawn_pool_size` column) with
    /// one worktree-enabled mesh and `schema_version = 23` (one below
    /// the v24 backfill). The runner's version-gated pass walks every
    /// entry with version > 23, which adds the v22 column AND runs the
    /// v24 OneShotBackfill (flag clear). Mesh 1's
    /// `pre_spawn_pool_size` flips from 0 (the v22 ALTER-time default)
    /// to 1 (the v24 inline default).
    fn v22_schema_for_backfill() -> Connection {
        let conn = v20_schema_with_mesh(true); // mesh 1: use_worktree=1
                                               // Stamp schema_version as 23 — one version below the v24
                                               // backfill, so the version-gated pass walks every entry that
                                               // adds columns AND runs the OneShotBackfill (gated flag clear
                                               // on first call). Mesh 1 ends up at pre_spawn_pool_size = 1.
        conn.execute(
            "UPDATE app_settings SET value = '23' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        // Post-backfill: mesh 1 has pre_spawn_pool_size = 1 (flipped).
        // The non-backfill scenarios below are inserted AFTER the
        // backfill — they verify the WHERE clause is precise (a
        // worktree-disabled mesh stays 0; an explicitly-sized mesh
        // stays at its user-set value).
        conn.execute(
            "INSERT INTO meshes (name, path, use_worktree, pre_spawn_pool_size)
             VALUES ('no-wt', '/repo/no-wt', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO meshes (name, path, use_worktree, pre_spawn_pool_size)
             VALUES ('sized', '/repo/sized', 1, 3)",
            [],
        )
        .unwrap();
        conn
    }

    fn pool_size_for(conn: &Connection, path: &str) -> i64 {
        conn.query_row(
            "SELECT pre_spawn_pool_size FROM meshes WHERE path = ?1",
            rusqlite::params![path],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn backfill_enables_pool_only_for_worktree_enabled_zero_meshes() {
        let conn = v22_schema_for_backfill();
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        assert_eq!(
            pool_size_for(&conn, "/repo/m"),
            1,
            "a worktree-enabled mesh on the old 0 default must flip to 1"
        );
        assert_eq!(
            pool_size_for(&conn, "/repo/no-wt"),
            0,
            "a worktree-disabled mesh stays at 0 — the pool is meaningless for it"
        );
        assert_eq!(
            pool_size_for(&conn, "/repo/sized"),
            3,
            "an explicitly-sized pool must never be touched"
        );
    }

    /// The flag makes the backfill strictly one-time: a user who opts a
    /// mesh back OUT (sets 0) after the flip must never be overridden on a
    /// later startup — that would make the Worktrees Probe's off switch
    /// useless.
    #[test]
    fn backfill_runs_at_most_once_per_db() {
        let conn = v22_schema_for_backfill();
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        // User opts back out.
        conn.execute(
            "UPDATE meshes SET pre_spawn_pool_size = 0 WHERE path = '/repo/m'",
            [],
        )
        .unwrap();
        // Every subsequent init re-calls the ensure — it must be a no-op.
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        assert_eq!(
            pool_size_for(&conn, "/repo/m"),
            0,
            "a post-backfill explicit 0 must survive later startups (flag-gated one-time)"
        );
    }

    /// A brand-new v24 DB gets the column with DEFAULT 1, so a mesh row
    /// inserted without naming the column lands pool-on. Pins the
    /// `ensure_column` default that `create_mesh`'s explicit value mirrors.
    #[test]
    fn fresh_column_default_is_pool_on() {
        let conn = v20_schema_with_mesh(true);
        crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
        // The fixture mesh predates the column add — it reads the ALTER
        // default.
        assert_eq!(
            pool_size_for(&conn, "/repo/m"),
            1,
            "the v24 column default is 1 (pool on); pre-v24 DBs are covered by the backfill"
        );
    }
}
