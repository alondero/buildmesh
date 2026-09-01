//! Integration tests for the schema-evolution runner (`db::migrations`).
//!
//! These tests exercise the ACTUAL [`migrations::evolve_to`] function from
//! `db::migrations` against in-memory connections with various pre-current
//! schemas to verify the runner handles v6+ upgrades correctly. The
//! pre-#249 tests that simulated v2 → current upgrades were removed
//! with the v6 `migrate_projects_*` dead code; the new
//! `evolve_to_handles_v6_to_current` test pins the post-v6 migration
//! path the issue body specifically asks for.
//!
//! Run with: cargo test --package buildmesh --lib db::tests

use std::collections::HashSet;

#[test]
fn hook_session_capture_only_fills_a_missing_cli_session_id() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE agent_nodes (
            id INTEGER PRIMARY KEY,
            cli_session_id TEXT
         );
         INSERT INTO agent_nodes (id, cli_session_id) VALUES
            (1, NULL),
            (2, ''),
            (3, 'already-captured');",
    )
    .unwrap();

    assert!(super::set_cli_session_id_if_missing_inner(&conn, 1, "hook-one").unwrap());
    assert!(super::set_cli_session_id_if_missing_inner(&conn, 2, "hook-two").unwrap());
    assert!(!super::set_cli_session_id_if_missing_inner(&conn, 3, "stale-hook").unwrap());
    assert!(!super::set_cli_session_id_if_missing_inner(&conn, 999, "missing-node").unwrap());

    let ids: Vec<Option<String>> = (1..=3)
        .map(|id| {
            conn.query_row(
                "SELECT cli_session_id FROM agent_nodes WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .unwrap()
        })
        .collect();
    assert_eq!(
        ids,
        vec![
            Some("hook-one".to_string()),
            Some("hook-two".to_string()),
            Some("already-captured".to_string()),
        ]
    );
}

// File-level `#[cfg(test)]` is applied by the parent module's
// `#[cfg(test)] mod tests;` declaration — no inner `mod tests {}` wrapper
// needed (clippy::module_inception otherwise fires because the file is
// already called `tests.rs`).

/// Regression: upgrading a v8 DB (post-rename, has `meshes` not `projects`)
/// to current must add the `source_issue` column to agent_nodes. Before
/// the fix, the migration was gated on the absence of the renamed-away
/// `projects` table and silently skipped, leaving the column missing
/// while still bumping schema_version → "no such column: source_issue"
/// at runtime. After #249 the runner's always-pass column walk
/// (`migrations::evolve_to`) is structurally responsible for the
/// same outcome — a v8 DB at the current version gains every column
/// the registry knows about regardless of the version-bump gate.
#[test]
fn test_v8_to_v9_adds_source_issue_via_safety_net() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        INSERT INTO app_settings (key, value) VALUES ('schema_version', '9');
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
            worktree_name TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        "
    ).unwrap();

    // Precondition: schema_version is already at 9 (bug state), so the
    // version-gated pass in `evolve_to` sees no work to do. The
    // always-pass column walk must still add the missing column.
    let has_col_before: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'source_issue'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert!(!has_col_before, "source_issue must be missing before fix");

    // Bug state: bump schema_version to current so the runner's
    // version-gated pass short-circuits (current >= 9). The
    // always-pass column walk is what should pick the column up.
    conn.execute(
        "UPDATE app_settings SET value = ?1 WHERE key = 'schema_version'",
        rusqlite::params![crate::db::migrations::SCHEMA_VERSION.to_string()],
    ).unwrap();

    crate::db::migrations::evolve_to(
        crate::db::migrations::SCHEMA_VERSION,
        &conn,
    ).unwrap();

    let has_col_after: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'source_issue'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert!(has_col_after, "source_issue must exist after evolve_to's always-pass column walk");

    // Idempotent: running evolve_to again must be a no-op (every ALTER
    // is gated on the pragma_table_info skip, every backfill on its
    // app_settings flag, every AlwaysStep is naturally idempotent).
    crate::db::migrations::evolve_to(
        crate::db::migrations::SCHEMA_VERSION,
        &conn,
    ).unwrap();
}

/// Regression guard for the v18 sandbox column (issue #497): a pre-v18 `meshes`
/// table (no `sandbox` column) must gain it via the registry's always-pass
/// column walk, and a second call must be a no-op. Mirrors the source-issue
/// (v9) regression above — the same migration-gate-skipped bug class the
/// runner closes structurally.
#[test]
fn test_evolve_to_adds_v18_sandbox_column_idempotently() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE meshes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
         );
         CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO app_settings (key, value) VALUES ('schema_version', '17');",
    )
    .unwrap();

    let present = |c: &rusqlite::Connection| -> bool {
        c.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('meshes') WHERE name = 'sandbox'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };

    assert!(!present(&conn), "sandbox must be missing before evolve_to runs");
    crate::db::migrations::evolve_to(
        crate::db::migrations::SCHEMA_VERSION,
        &conn,
    ).unwrap();
    assert!(present(&conn), "sandbox must exist after evolve_to's always-pass column walk");
    // Idempotent: a second call must not error.
    crate::db::migrations::evolve_to(
        crate::db::migrations::SCHEMA_VERSION,
        &conn,
    ).unwrap();
    // Default must be 0 (off) — the feature is opt-in.
    conn.execute("INSERT INTO meshes (name, path) VALUES ('m', '/tmp/m')", []).unwrap();
    let sandbox: i32 = conn
        .query_row("SELECT sandbox FROM meshes WHERE name = 'm'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sandbox, 0, "sandbox must default to 0 (off)");
}

// --- Pending worktree removal queue ---

/// In-memory schema with just the two tables the close path touches.
fn pending_removal_schema() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
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
            source_issue INTEGER,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE pending_worktree_removals (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            worktree_path TEXT NOT NULL UNIQUE,
            node_name TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE app_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        INSERT INTO agent_nodes (id, mesh_id, name, path) VALUES (42, 1, 'bold-keen-brook', '/repo');
        ",
    )
    .unwrap();
    conn
}

/// Closing a node with a worktree must delete the row AND record the pending
/// removal in one go — that durable record is what lets the UI drop the node
/// instantly while the disk cleanup runs later.
#[test]
fn close_deletes_row_and_enqueues_removal() {
    let conn = pending_removal_schema();

    crate::db::delete_agent_node_enqueueing_removal_inner(
        &conn,
        42,
        Some(("/repo/.claude/worktrees/bold-keen-brook", "bold-keen-brook")),
    )
    .unwrap();

    let node_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM agent_nodes WHERE id = 42", [], |r| r.get(0))
        .unwrap();
    assert_eq!(node_count, 0, "node row must be gone immediately");

    let pending = crate::db::list_pending_worktree_removals_inner(&conn).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].worktree_path, "/repo/.claude/worktrees/bold-keen-brook");
    assert_eq!(pending[0].node_name, "bold-keen-brook");
}

/// An in-place node (no worktree) closes without queueing any cleanup.
#[test]
fn close_without_worktree_enqueues_nothing() {
    let conn = pending_removal_schema();

    crate::db::delete_agent_node_enqueueing_removal_inner(&conn, 42, None).unwrap();

    assert!(crate::db::list_pending_worktree_removals_inner(&conn).unwrap().is_empty());
}

/// Re-enqueuing the same path (e.g. a retry after a failed drain) is a no-op,
/// not a duplicate — the queue holds at most one entry per worktree.
#[test]
fn enqueue_is_idempotent_per_path() {
    let conn = pending_removal_schema();

    crate::db::enqueue_worktree_removal_inner(&conn, "/repo/wt", "n").unwrap();
    crate::db::enqueue_worktree_removal_inner(&conn, "/repo/wt", "n").unwrap();

    assert_eq!(crate::db::list_pending_worktree_removals_inner(&conn).unwrap().len(), 1);
}

/// A successful drain dequeues exactly the path it cleaned, leaving others.
#[test]
fn delete_pending_removes_only_named_path() {
    let conn = pending_removal_schema();
    crate::db::enqueue_worktree_removal_inner(&conn, "/repo/a", "a").unwrap();
    crate::db::enqueue_worktree_removal_inner(&conn, "/repo/b", "b").unwrap();

    crate::db::delete_pending_worktree_removal_inner(&conn, "/repo/a").unwrap();

    let remaining = crate::db::list_pending_worktree_removals_inner(&conn).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].worktree_path, "/repo/b");
}

// --- Manual warm-pool slug adoption (#1080) ---

/// A Manual warm-pool spawn runs in a directory the *pool* named, not the
/// throwaway slug stage 1 wrote to the row. Adopting that slug has to land
/// BOTH halves in one write:
///
///   * `name` — the identity the user sees, and
///   * `worktree_name` — the column the close path derives the removal
///     directory from (`env::node_worktree_path`).
///
/// #1080 leaked a worktree on every close precisely because only `name` was
/// written. The close path then queued `<mesh>/.claude/worktrees/<stage-1
/// slug>` — a directory that had never existed — and `remove_worktree_inner`
/// returns `Ok(())` for a missing path, so the bogus removal was dequeued as
/// a success while the real worktree stayed on disk. No error, no retry, no
/// `worktree-cleanup-failed` event.
///
/// One UPDATE rather than two calls is deliberate: these are two halves of a
/// single adoption, and the bug was exactly the two halves drifting apart.
#[test]
fn adopting_manual_pool_slug_sets_both_name_and_worktree_name() {
    let conn = pending_removal_schema();
    conn.execute(
        "INSERT INTO agent_nodes (id, mesh_id, name, path, worktree_name) \
         VALUES (7, 1, 'tidy-sorrowful-nautilus', '/repo', 'tidy-sorrowful-nautilus')",
        [],
    )
    .unwrap();

    crate::db::adopt_manual_pool_slug_inner(&conn, 7, "utopian-repulsive-manatee").unwrap();

    let (name, worktree_name): (String, Option<String>) = conn
        .query_row(
            "SELECT name, worktree_name FROM agent_nodes WHERE id = 7",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();

    assert_eq!(
        name, "utopian-repulsive-manatee",
        "the display name must adopt the pool slug so the row matches the directory"
    );
    assert_eq!(
        worktree_name.as_deref(),
        Some("utopian-repulsive-manatee"),
        "worktree_name must adopt the pool slug too — the close path derives the \
         removal directory from this column, so a stale value silently leaks the \
         real worktree (#1080)"
    );
}

/// Adoption must not touch any row but its own. Asserts on the named
/// row's new state AND on a sibling row's unchanged state — either
/// alone would let a missing `WHERE id` regression pass.
#[test]
fn adopting_manual_pool_slug_touches_only_the_named_row() {
    let conn = pending_removal_schema();
    conn.execute(
        "INSERT INTO agent_nodes (id, mesh_id, name, path, worktree_name)          VALUES (8, 1, 'other-node', '/repo', 'other-slug')",
        [],
    )
    .unwrap();

    // Row 42 is seeded by `pending_removal_schema`.
    crate::db::adopt_manual_pool_slug_inner(&conn, 42, "claimed-slug").unwrap();

    let (other_name, other_worktree_name): (String, Option<String>) = conn
        .query_row(
            "SELECT name, worktree_name FROM agent_nodes WHERE id = 8",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(other_name, "other-node", "sibling name must be untouched");
    assert_eq!(
        other_worktree_name.as_deref(),
        Some("other-slug"),
        "sibling worktree_name must be untouched"
    );

    let (name42, worktree_name42): (String, Option<String>) = conn
        .query_row(
            "SELECT name, worktree_name FROM agent_nodes WHERE id = 42",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(name42, "claimed-slug", "the named row's name must be adopted");
    assert_eq!(
        worktree_name42.as_deref(),
        Some("claimed-slug"),
        "the named row's worktree_name must be adopted"
    );
}

/// Issue #249 regression pin: a v6 DB (post-`projects`→`meshes` rename,
/// has `meshes` + `agent_nodes` but no v8+ columns) must upgrade cleanly
/// to the current schema in one `evolve_to` call. Pre-#249 the v8+
/// column adds lived in the version-gated ladder gated on the pre-v6
/// `projects` table — v6+ upgrades were structurally impossible (the
/// comment at `db/mod.rs:101-106` warned future developers not to
/// "fix" the guard). Post-#249 the runner's version-gated pass handles
/// v6+ upgrades via the column registry, and the always-pass walk
/// catches any column a build might have missed.
///
/// A second `evolve_to` call must be a no-op (every column ALTER is
/// gated on `pragma_table_info`, every backfill on its `app_settings`
/// flag, every AlwaysStep is naturally idempotent). This is the
/// single-test equivalent of every `ensure_*` safety net running
/// concurrently — a regression here means the runner lost either its
/// column-add idempotency or its version-bump gate.
#[test]
fn evolve_to_handles_v6_to_current_upgrade() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();

    // v6-shape schema: `meshes` + `agent_nodes` exist with the v6
    // column set, but every v8+ column is missing. No `app_settings`
    // key — `current_version()` returns 0 so the version-gated pass
    // walks every entry in the registry.
    conn.execute_batch(
        "
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
            mesh_id INTEGER NOT NULL REFERENCES meshes(id),
            name TEXT NOT NULL,
            path TEXT NOT NULL,
            branch TEXT NOT NULL DEFAULT 'main',
            env TEXT NOT NULL DEFAULT 'windows',
            provider TEXT NOT NULL DEFAULT 'anthropic',
            status TEXT NOT NULL DEFAULT 'idle',
            cli_session_id TEXT,
            worktree_name TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        INSERT INTO meshes (id, name, path) VALUES (1, 'legacy', '/tmp/legacy');
        INSERT INTO agent_nodes (id, mesh_id, name, path) VALUES (1, 1, 'a', '/tmp/legacy/a');
        ",
    )
    .unwrap();

    // Sanity: every column the registry knows about for `meshes` and
    // `agent_nodes` is missing BEFORE the upgrade. The projection
    // must be the full registry set, not just a subset — a future
    // column that forgets to land in `SPECS` slips past this pin.
    let missing_meshes_columns: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('meshes')")
            .unwrap();
        let present: HashSet<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        crate::db::migrations::mesh_column_specs()
            .iter()
            .filter(|c| !present.contains(c.column))
            .map(|c| c.column.to_string())
            .collect()
    };
    assert!(
        missing_meshes_columns.len() >= 20,
        "a v6 DB must be missing at least 20 columns from the current registry \
         (this guards against the runner's mesh subset silently regressing); \
         missing so far: {:?}",
        missing_meshes_columns
    );

    // Act: upgrade to current. First call does the work.
    crate::db::migrations::evolve_to(
        crate::db::migrations::SCHEMA_VERSION,
        &conn,
    )
    .unwrap();

    // Assert: every mesh column the registry lists exists on the
    // table. (For agent_nodes the column set is the same; the
    // `mesh_column_specs` subset is the cheaper pin because the
    // projection is built from it.)
    let present_after: HashSet<String> = conn
        .prepare("SELECT name FROM pragma_table_info('meshes')")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let still_missing: Vec<&'static str> = crate::db::migrations::mesh_column_specs()
        .iter()
        .filter(|c| !present_after.contains(c.column))
        .map(|c| c.column)
        .collect();
    assert!(
        still_missing.is_empty(),
        "every registry column must exist after evolve_to; missing: {:?}",
        still_missing
    );

    // Assert: schema_version is now current.
    let v: i32 = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0).map(|s| s.parse().unwrap_or(0)),
        )
        .unwrap_or(0);
    assert_eq!(v, crate::db::migrations::SCHEMA_VERSION as i32);

    // Assert: the v6 mesh + agent_node row survived. (Read via the
    // projection's COALESCE shape so the freshly-added nullable
    // columns don't break the read.)
    let legacy_name: String = conn
        .query_row("SELECT name FROM meshes WHERE id = 1", [], |row| row.get(0))
        .unwrap();
    assert_eq!(legacy_name, "legacy");
    let node_name: String = conn
        .query_row("SELECT name FROM agent_nodes WHERE id = 1", [], |row| row.get(0))
        .unwrap();
    assert_eq!(node_name, "a");

    // Assert: a v24-era mesh (worktree-enabled) had its
    // `pre_spawn_pool_size` flipped from the v22 ALTER-time default
    // (0) to the v24 default (1) by the one-shot backfill — the
    // pre-#249 `ensure_pool_default_backfill` path, now an entry in
    // `migrations::ONE_SHOT_BACKFILLS`.
    let pool_size: i32 = conn
        .query_row(
            "SELECT pre_spawn_pool_size FROM meshes WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    // The test mesh has no explicit `use_worktree`, so the column's
    // inline default (1) applies. (The v24 backfill's worktree-
    // enabled filter `COALESCE(use_worktree, 1) = 1` also flips it
    // to 1 — both paths converge.)
    assert_eq!(pool_size, 1, "v22 ALTER-time default 0 must flip to v24 default 1");

    // Idempotent: a second call must be a no-op (no error, no
    // duplicate-column, no backfill re-flip).
    crate::db::migrations::evolve_to(
        crate::db::migrations::SCHEMA_VERSION,
        &conn,
    )
    .unwrap();
    let pool_size_after: i32 = conn
        .query_row(
            "SELECT pre_spawn_pool_size FROM meshes WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        pool_size_after, 1,
        "second evolve_to must not re-run the backfill and clobber a user's explicit 0"
    );
}

// ----- v19 Spawn Option composite-id migration (issue #575) ----------
//
// The migration is split into two blocks: a first-class block
// (hardcoded SQL for 'minimax'/'kimi') that runs from `db::init` and
// is preferences-independent, and a custom-account block that needs
// the live `Vec<ProviderAccount>` and runs from `lib.rs::setup` after
// `preferences::init`. The tests below pin both blocks against an
// in-memory schema, and the order-of-operations invariant (custom
// account rows that exist *before* the migration must be rewritten
// to `claude:<id>`).

/// Helper: build a v18 schema with `agent_nodes` populated by rows
/// the v19 migration should rewrite. Returns the connection so each
/// test can verify the post-migration state.
fn v19_setup_with_legacy_rows() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS meshes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            layout TEXT NOT NULL DEFAULT 'grid',
            position INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        INSERT INTO meshes (name, path) VALUES ('m', '/tmp/m');

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
        -- Legacy bare ids the v19 migration should rewrite.
        INSERT INTO agent_nodes (mesh_id, name, path, provider) VALUES (1, 'm', '/tmp/m', 'minimax');
        INSERT INTO agent_nodes (mesh_id, name, path, provider) VALUES (1, 'm', '/tmp/m', 'kimi');
        -- Custom bare account id (e.g. user-typed 'deepseek') — rewritten by the
        -- custom-account block when the live preferences list includes it.
        INSERT INTO agent_nodes (mesh_id, name, path, provider) VALUES (1, 'm', '/tmp/m', 'deepseek');
        -- Native harness ids — left alone by the migration.
        INSERT INTO agent_nodes (mesh_id, name, path, provider) VALUES (1, 'm', '/tmp/m', 'claude');
        INSERT INTO agent_nodes (mesh_id, name, path, provider) VALUES (1, 'm', '/tmp/m', 'codex');
        INSERT INTO agent_nodes (mesh_id, name, path, provider) VALUES (1, 'm', '/tmp/m', 'terminal');
        -- Already-migrated composite id — should be left alone.
        INSERT INTO agent_nodes (mesh_id, name, path, provider) VALUES (1, 'm', '/tmp/m', 'claude:minimax');
        ",
    ).unwrap();
    conn
}

/// Helper: read the `provider` column of every `agent_nodes` row in id order.
fn v19_read_providers(conn: &rusqlite::Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT provider FROM agent_nodes ORDER BY id ASC")
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

/// Pin the first-class rewrite block: `minimax` bare ids are rewritten
/// to `claude:minimax`; native harness ids (`claude`, `codex`,
/// `terminal`) and already-migrated composite ids are left untouched.
/// `kimi` was removed from this block by wayfinder #918: Kimi Code is now
/// a native self-auth harness, so bare `kimi` rows resolve to
/// `Provider::Kimi` via `from_db_str` without a rewrite. This block is
/// preferences-independent and safe to run from `db::init` (lives in
/// `db::migrations::AlwaysStep::RewriteAgentNodeProviderId` post-#249).
#[test]
fn v19_first_class_migration_rewrites_minimax_only() {
    let conn = v19_setup_with_legacy_rows();
    // First call to `evolve_to` creates `app_settings` (via
    // `current_version`'s self-sufficient CREATE TABLE IF NOT EXISTS)
    // and brings the schema forward to current, including the
    // version-gated column adds (none of which this v18-shape schema
    // is missing) and the always-pass `RewriteAgentNodeProviderId`
    // rewrite.
    crate::db::migrations::evolve_to(
        crate::db::migrations::SCHEMA_VERSION,
        &conn,
    )
    .unwrap();

    let providers = v19_read_providers(&conn);
    // 7 rows: minimax, kimi, deepseek, claude, codex, terminal, claude:minimax
    // After first-class block: claude:minimax, kimi (native now, #918),
    // deepseek, claude, codex, terminal, claude:minimax
    assert_eq!(
        providers,
        vec![
            "claude:minimax", // minimax → claude:minimax
            "kimi",            // kimi left bare — resolves to native Kimi Code (#918)
            "deepseek",        // custom — NOT rewritten by the first-class block
            "claude",          // native — left alone
            "codex",           // native — left alone
            "terminal",        // native — left alone
            "claude:minimax",  // already composite — left alone
        ]
    );
}

/// Pin the custom-account rewrite block: a bare `claude_compatible`
/// custom account id is rewritten to `claude:<id>`, but only when
/// the live account list includes it AND `enabled == true`.
/// Disabled custom accounts are left bare (intentional — see the
/// migration doc-comment).
#[test]
fn v19_custom_account_migration_rewrites_enabled_custom_ids() {
    use crate::preferences::{BillingMode, ProviderAccount};

    let conn = v19_setup_with_legacy_rows();
    let accounts = vec![
        ProviderAccount {
            id: "deepseek".to_string(),
            name: "DeepSeek".to_string(),
            enabled: true,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-test".to_string()),
        },
        // A disabled custom account: NOT rewritten.
        ProviderAccount {
            id: "disabled-bot".to_string(),
            name: "Disabled Bot".to_string(),
            enabled: false,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-test".to_string()),
        },
    ];
    // Add a row for the disabled custom account to test the filter.
    conn.execute(
        "INSERT INTO agent_nodes (mesh_id, name, path, provider) VALUES (1, 'm', '/tmp/m', 'disabled-bot')",
        [],
    ).unwrap();

    crate::db::migrate_agent_node_provider_id_custom_accounts(&conn, &accounts).unwrap();

    let providers = v19_read_providers(&conn);
    // After custom-account block: the deepseek row is rewritten;
    // disabled-bot stays bare (filtered out by `a.enabled`).
    let deepseek = providers.iter().find(|p| p.contains("deepseek")).unwrap();
    assert_eq!(deepseek, "claude:deepseek");
    let disabled = providers.iter().find(|p| p.contains("disabled")).unwrap();
    assert_eq!(disabled, "disabled-bot", "disabled custom account must stay bare");
}

/// Idempotency: re-running the custom-account block on a v19+ DB
/// is a no-op (the `NOT LIKE '%:%'` guard skips already-migrated
/// rows). This is the contract `lib.rs::setup` relies on when it
/// re-runs the safety net on every launch.
#[test]
fn v19_custom_account_migration_is_idempotent() {
    use crate::preferences::{BillingMode, ProviderAccount};

    let conn = v19_setup_with_legacy_rows();
    let accounts = vec![ProviderAccount {
        id: "deepseek".to_string(),
        name: "DeepSeek".to_string(),
        enabled: true,
        billing_mode: BillingMode::PayAsYouGo,
        claude_compatible: true,
        api_key: Some("sk-test".to_string()),
    }];

    crate::db::migrate_agent_node_provider_id_custom_accounts(&conn, &accounts).unwrap();
    let after_first = v19_read_providers(&conn);
    crate::db::migrate_agent_node_provider_id_custom_accounts(&conn, &accounts).unwrap();
    let after_second = v19_read_providers(&conn);

    assert_eq!(
        after_first, after_second,
        "re-running the migration must not change the providers column"
    );
}

// ----- mesh-default composite-id safety net (issue follow-up to #575) -----
//
// The v19 first-class block rewrote `agent_nodes.provider` from
// bare → composite but never touched `meshes.default_provider`. A
// pre-#575 mesh whose default was set to `"minimax"` keeps the legacy
// bare form after upgrade, which silently routes through
// `resolve_provider_env` to the keyed MiniMax **account** — spawning
// Claude CLI sessions against MiniMax's endpoint even when the user
// never picked MiniMax-via-Claude.
//
// `ensure_mesh_default_provider_normalized` is the safety net that
// closes this gap. The tests below pin its rewrite rules against a
// v18-shape `meshes` schema.

/// Helper: build a `meshes` table populated with the legacy bare
/// forms the safety net should rewrite, plus rows it must NOT touch.
fn mesh_default_setup_with_legacy_rows() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "
        CREATE TABLE meshes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            layout TEXT NOT NULL DEFAULT 'grid',
            position INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            default_provider TEXT
        );
        -- Legacy bare ids the safety net should rewrite.
        INSERT INTO meshes (name, path, default_provider) VALUES ('m1', '/tmp/m1', 'minimax');
        INSERT INTO meshes (name, path, default_provider) VALUES ('m2', '/tmp/m2', 'kimi');
        -- Native harness id — left alone.
        INSERT INTO meshes (name, path, default_provider) VALUES ('m3', '/tmp/m3', 'claude');
        -- Already-migrated composite id — left alone.
        INSERT INTO meshes (name, path, default_provider) VALUES ('m4', '/tmp/m4', 'claude:minimax');
        -- No override — left alone.
        INSERT INTO meshes (name, path) VALUES ('m5', '/tmp/m5');
        ",
    )
    .unwrap();
    conn
}

fn read_mesh_default_providers(conn: &rusqlite::Connection) -> Vec<(String, Option<String>)> {
    let mut stmt = conn
        .prepare("SELECT name, default_provider FROM meshes ORDER BY id ASC")
        .unwrap();
    stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

/// Safety net: bare `minimax` / `kimi` mesh defaults are rewritten to
/// `claude:minimax` / `claude:kimi`. Native harness ids, already-
/// composite ids, and NULL defaults are left untouched.
#[test]
fn ensure_mesh_default_provider_normalized_rewrites_bare_to_composite() {
    let conn = mesh_default_setup_with_legacy_rows();

    crate::db::ensure_mesh_default_provider_normalized(&conn).unwrap();

    let got = read_mesh_default_providers(&conn);
    assert_eq!(
        got,
        vec![
            ("m1".into(), Some("claude:minimax".into())), // bare minimax rewritten
            ("m2".into(), Some("kimi".into())),            // bare kimi left alone (#918)
            ("m3".into(), Some("claude".into())),          // native — left alone
            ("m4".into(), Some("claude:minimax".into())),  // composite — left alone
            ("m5".into(), None),                           // NULL — left alone
        ],
        "ensure_mesh_default_provider_normalized must rewrite bare minimax only; \
         bare kimi left bare so it resolves to the native Kimi Code harness (#918)"
    );
}

/// Idempotency: re-running the safety net is a no-op on a healthy
/// (already-normalized) DB. Mirrors the v19 first-class migration's
/// `WHERE default_provider IN (...)` guard.
#[test]
fn ensure_mesh_default_provider_normalized_is_idempotent() {
    let conn = mesh_default_setup_with_legacy_rows();
    crate::db::ensure_mesh_default_provider_normalized(&conn).unwrap();
    let after_first = read_mesh_default_providers(&conn);

    crate::db::ensure_mesh_default_provider_normalized(&conn).unwrap();
    let after_second = read_mesh_default_providers(&conn);

    assert_eq!(
        after_first, after_second,
        "re-running the safety net must not change any default_provider"
    );
}

/// Issue #1224 — poison-recovery regression. A panic while holding the
/// writer DB `Mutex` used to permanently poison it: every subsequent
/// caller re-panicked with "poisoned lock" and persistence, preferences,
/// and later writes through `write_conn()` all bricked. The
/// helper `write_conn()` now calls `into_inner()` instead, so the DB stays
/// usable across transient panics.
///
/// This test pins that path against the actual singleton. The `DB`
/// OnceCell is process-wide, so the poison sets the global state for
/// every other test in the binary — that's the bug class the issue
/// describes. The recovery helper must hand back a usable guard, and
/// a round-trip read must still see the rows we wrote.
#[test]
fn write_conn_recovers_from_poison() {
    // The global DB must be initialized before the test can lock it.
    // Other db-tests use a per-test temp file; we follow that pattern
    // so this test stays self-contained and cleans up after itself.
    let test_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_path =
        std::env::temp_dir().join(format!("buildmesh_issue_1224_poison_test_{}.db", test_id));
    crate::db::init(&temp_path).expect("test setup: db::init must succeed");

    // Poison the singleton. `catch_unwind` keeps the test binary alive;
    // the panic payload is the assertion that the inner path panicked.
    let poison_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = crate::db::write_conn();
        panic!("intentional db singleton poison for issue #1224 regression test");
    }));
    assert!(
        poison_result.is_err(),
        "test fixture must panic to poison the DB mutex"
    );

    // After the panic, locking the writer with `lock().unwrap()` would now
    // return `Err(Poisoned)` for every subsequent caller until the
    // process restarts. The recovery helper must hand back a usable
    // guard, and the underlying `Connection` must still answer
    // queries correctly. We round-trip through a write+read pair so
    // the test fails loudly if the recovery shape regresses.
    crate::db::write_conn()
        .execute(
            "CREATE TABLE IF NOT EXISTS issue_1224_poison_probe (id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
            [],
        )
        .unwrap();
    crate::db::write_conn()
        .execute(
            "INSERT OR REPLACE INTO issue_1224_poison_probe (id, label) VALUES (1, 'after-poison')",
            [],
        )
        .unwrap();
    let label: String = crate::db::write_conn()
        .query_row(
            "SELECT label FROM issue_1224_poison_probe WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        label, "after-poison",
        "DB writes must survive the poison (issue #1224)"
    );

    // Tidy so the probe table and the temp file do not leak into
    // other tests. The OnceCell is process-wide, so a sibling test
    // that calls `init` against the same path would otherwise race
    // with us — a unique path per run keeps the test re-orderable.
    crate::db::write_conn()
        .execute("DROP TABLE IF EXISTS issue_1224_poison_probe", [])
        .unwrap();
    drop(crate::db::write_conn()); // release the guard before delete
    std::fs::remove_file(&temp_path).ok();
}

/// Issue #1383: hold an uncommitted writer transaction open and require a
/// pooled read to finish before that transaction is released. A regression
/// to one process-wide connection blocks until the timeout.
#[test]
fn reader_pool_reads_while_writer_transaction_is_active() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("reader-writer-concurrency.db");
    let writer = rusqlite::Connection::open(&db_path).unwrap();
    super::apply_connection_pragmas(&writer, false).unwrap();
    writer
        .execute_batch(
            "CREATE TABLE probe (value INTEGER NOT NULL);
             INSERT INTO probe (value) VALUES (1);",
        )
        .unwrap();
    let readers = super::ReaderPool::open(&db_path).unwrap();

    let writer_tx = writer.unchecked_transaction().unwrap();
    writer_tx
        .execute("INSERT INTO probe (value) VALUES (2)", [])
        .unwrap();

    std::thread::scope(|scope| {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let readers = &readers;
        scope.spawn(move || {
            let reader = readers.checkout().unwrap();
            let count: i64 = reader
                .query_row("SELECT COUNT(*) FROM probe", [], |row| row.get(0))
                .unwrap();
            done_tx.send(count).unwrap();
        });

        assert_eq!(
            done_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("pooled read must not wait for the active writer transaction"),
            1,
            "reader must see the last committed WAL snapshot"
        );
    });

    writer_tx.rollback().unwrap();
}

#[test]
fn reader_pool_serves_a_second_reader_while_the_first_is_checked_out() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("reader-reader-concurrency.db");
    let writer = rusqlite::Connection::open(&db_path).unwrap();
    super::apply_connection_pragmas(&writer, false).unwrap();
    writer
        .execute("CREATE TABLE probe (value INTEGER NOT NULL)", [])
        .unwrap();
    let readers = super::ReaderPool::open(&db_path).unwrap();

    let first_reader = readers.checkout().unwrap();
    let mut first_statement = first_reader.prepare("SELECT value FROM probe").unwrap();
    let mut first_rows = first_statement.query([]).unwrap();

    std::thread::scope(|scope| {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let readers = &readers;
        scope.spawn(move || {
            let second_reader = readers.checkout().unwrap();
            let count: i64 = second_reader
                .query_row("SELECT COUNT(*) FROM probe", [], |row| row.get(0))
                .unwrap();
            done_tx.send(count).unwrap();
        });
        assert_eq!(
            done_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("a checked-out reader must not serialize another reader"),
            0
        );
    });

    assert!(first_rows.next().unwrap().is_none());
}

#[test]
fn reader_pool_connections_reject_writes() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("read-only-pool.db");
    let writer = rusqlite::Connection::open(&db_path).unwrap();
    super::apply_connection_pragmas(&writer, false).unwrap();
    writer
        .execute("CREATE TABLE probe (value INTEGER NOT NULL)", [])
        .unwrap();

    let readers = super::ReaderPool::open(&db_path).unwrap();
    let reader = readers.checkout().unwrap();
    assert!(
        reader
            .execute("INSERT INTO probe (value) VALUES (1)", [])
            .is_err(),
        "reader-pool handles must be enforced read-only by SQLite"
    );
}

#[test]
fn reader_pool_times_out_when_exhausted() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("reader-timeout.db");
    let writer = rusqlite::Connection::open(&db_path).unwrap();
    super::apply_connection_pragmas(&writer, false).unwrap();
    writer
        .execute("CREATE TABLE probe (value INTEGER NOT NULL)", [])
        .unwrap();
    let readers = super::ReaderPool::open(&db_path).unwrap();
    let checked_out: Vec<_> = (0..super::READER_POOL_SIZE)
        .map(|_| readers.checkout().unwrap())
        .collect();
    let started = std::time::Instant::now();
    assert!(readers.checkout().is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    drop(checked_out);
}

#[test]
fn reader_pool_recycles_after_panic_unwind() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("reader-panic.db");
    let writer = rusqlite::Connection::open(&db_path).unwrap();
    super::apply_connection_pragmas(&writer, false).unwrap();
    writer
        .execute("CREATE TABLE probe (value INTEGER NOT NULL)", [])
        .unwrap();
    let readers = super::ReaderPool::open(&db_path).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let reader = readers.checkout().unwrap();
        reader.execute_batch("BEGIN").unwrap();
        panic!("intentional reader panic");
    }));
    assert!(result.is_err());
    let reader = readers.checkout().unwrap();
    assert_eq!(reader.query_row("SELECT COUNT(*) FROM probe", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn reader_pool_queues_ninth_reader_until_a_slot_is_returned() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("reader-contention.db");
    let writer = rusqlite::Connection::open(&db_path).unwrap();
    super::apply_connection_pragmas(&writer, false).unwrap();
    writer
        .execute("CREATE TABLE probe (value INTEGER NOT NULL)", [])
        .unwrap();
    let readers = super::ReaderPool::open(&db_path).unwrap();
    let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();

    std::thread::scope(|scope| {
        let mut done = Vec::with_capacity(super::READER_POOL_SIZE + 1);
        let readers = &readers;
        for _ in 0..=super::READER_POOL_SIZE {
            let release = std::sync::Arc::clone(&release);
            let ready_tx = ready_tx.clone();
            done.push(scope.spawn(move || {
                let reader = readers.checkout().unwrap();
                ready_tx.send(()).unwrap();
                while !release.load(std::sync::atomic::Ordering::Acquire) {
                    std::thread::yield_now();
                }
                drop(reader);
            }));
        }
        for _ in 0..super::READER_POOL_SIZE {
            ready_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("all initial reader holders must check out promptly");
        }
        release.store(true, std::sync::atomic::Ordering::Release);
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the queued ninth reader must check out after a slot is returned");
        for handle in done {
            handle.join().unwrap();
        }
    });
}

#[test]
fn reader_pool_shares_named_memory_uri() {
    let uri = std::path::PathBuf::from("file:reader-shared-memory?mode=memory&cache=shared");
    let writer = rusqlite::Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
            | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    super::apply_connection_pragmas(&writer, false).unwrap();
    writer
        .execute("CREATE TABLE probe (value INTEGER NOT NULL)", [])
        .unwrap();
    let readers = super::ReaderPool::open(&uri).unwrap();
    let reader = readers.checkout().unwrap();
    assert_eq!(reader.query_row("SELECT COUNT(*) FROM probe", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
}
