//! Integration tests for the real migrate_if_needed implementation.
//!
//! These tests call the ACTUAL migrate_if_needed() function from db::mod
//! against an in-memory connection with a v2 schema to verify it handles
//! incremental columns correctly (or expose the current DROP-based bug).
//!
//! Run with: cargo test --package buildmesh --lib db::tests

// File-level `#[cfg(test)]` is applied by the parent module's
// `#[cfg(test)] mod tests;` declaration — no inner `mod tests {}` wrapper
// needed (clippy::module_inception otherwise fires because the file is
// already called `tests.rs`).

use crate::db::test_migrate_if_needed;

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

    // Assert: layout column now exists (table renamed to meshes during migration)
    let has_layout_after: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('meshes') WHERE name = 'layout'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert!(has_layout_after, "layout column must exist after migration");

    // Assert: existing project survived migration with 'grid' default
    let layout: String = conn.query_row(
        "SELECT layout FROM meshes WHERE name = 'existing-project'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(layout, "grid", "existing project should get 'grid' default after migration");
}

/// Regression: upgrading a v8 DB (post-rename, has `meshes` not `projects`)
/// to v9 must add the `source_issue` column to agent_nodes. Before the fix,
/// the migration was gated on the absence of the renamed-away `projects`
/// table and silently skipped, leaving the column missing while still
/// bumping schema_version → "no such column: source_issue" at runtime.
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

    // Precondition: schema_version is already at 9 (bug state), so
    // migrate_if_needed sees no work to do. The safety net must still run.
    let has_col_before: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'source_issue'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert!(!has_col_before, "source_issue must be missing before fix");

    crate::db::ensure_agent_node_source_issue(&conn).unwrap();

    let has_col_after: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'source_issue'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert!(has_col_after, "source_issue must exist after safety-net runs");

    // Idempotent: running it again must be a no-op, not an error.
    crate::db::ensure_agent_node_source_issue(&conn).unwrap();
}

/// Regression guard for the v18 sandbox column (issue #497): a pre-v18 `meshes`
/// table (no `sandbox` column) must gain it via the safety net, and a second
/// call must be a no-op. Mirrors the scratchpad (v17) safety-net test — the
/// same migration-gate-skipped bug class that bit `source_issue`.
#[test]
fn test_ensure_mesh_sandbox_adds_column_and_is_idempotent() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE meshes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
         );",
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

    assert!(!present(&conn), "sandbox must be missing before the safety net runs");
    crate::db::ensure_mesh_sandbox(&conn).unwrap();
    assert!(present(&conn), "sandbox must exist after the safety net runs");
    // Idempotent: a second call must not error.
    crate::db::ensure_mesh_sandbox(&conn).unwrap();
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

    // Table renamed from projects→meshes during migration
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM meshes",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 0, "no meshes should exist after double migration");
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

/// Pin the first-class rewrite block: `minimax` / `kimi` bare ids
/// are rewritten to `claude:minimax` / `claude:kimi`; native harness
/// ids (`claude`, `codex`, `terminal`) and already-migrated
/// composite ids are left untouched. This block is
/// preferences-independent and safe to run from `db::init`.
#[test]
fn v19_first_class_migration_rewrites_minimax_and_kimi() {
    let conn = v19_setup_with_legacy_rows();
    crate::db::migrate_agent_node_provider_id_to_composite(&conn).unwrap();

    let providers = v19_read_providers(&conn);
    // 7 rows: minimax, kimi, deepseek, claude, codex, terminal, claude:minimax
    // After first-class block: claude:minimax, claude:kimi, deepseek, claude, codex, terminal, claude:minimax
    assert_eq!(
        providers,
        vec![
            "claude:minimax", // minimax → claude:minimax
            "claude:kimi",     // kimi → claude:kimi
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
    use crate::preferences::{BillingMode, ModelTiers, ProviderAccount};

    let conn = v19_setup_with_legacy_rows();
    let accounts = vec![
        ProviderAccount {
            id: "deepseek".to_string(),
            name: "DeepSeek".to_string(),
            enabled: true,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-test".to_string()),
            base_url: Some("https://api.deepseek.com/anthropic".to_string()),
            model_tiers: ModelTiers::default(),
            models: Vec::new(),
        },
        // A disabled custom account: NOT rewritten.
        ProviderAccount {
            id: "disabled-bot".to_string(),
            name: "Disabled Bot".to_string(),
            enabled: false,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-test".to_string()),
            base_url: None,
            model_tiers: ModelTiers::default(),
            models: Vec::new(),
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
    use crate::preferences::{BillingMode, ModelTiers, ProviderAccount};

    let conn = v19_setup_with_legacy_rows();
    let accounts = vec![ProviderAccount {
        id: "deepseek".to_string(),
        name: "DeepSeek".to_string(),
        enabled: true,
        billing_mode: BillingMode::PayAsYouGo,
        claude_compatible: true,
        api_key: Some("sk-test".to_string()),
        base_url: None,
        model_tiers: ModelTiers::default(),
        models: Vec::new(),
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
