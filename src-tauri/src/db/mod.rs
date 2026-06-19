//! Database module using rusqlite for local SQLite storage

#[cfg(test)]
mod migration_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod mesh_tests;

#[cfg(test)]
mod scratchpad_tests;

use rusqlite::{Connection, params};
pub use rusqlite::Result as SqlResult;
use once_cell::sync::OnceCell;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::models::*;

static DB: OnceCell<Mutex<Connection>> = OnceCell::new();

/// Current schema version
const SCHEMA_VERSION: i32 = 18;

/// Initialize the database
pub fn init(db_path: &PathBuf) -> SqlResult<()> {
    let conn = Connection::open(db_path)?;

    // Ensure app_settings exists first (needed by migrate_if_needed to check version)
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS app_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        "
    )?;

    // Run migrations (may add columns to existing tables)
    migrate_if_needed(&conn)?;

    // Create schema (all tables + indexes, IF NOT EXISTS so they're idempotent).
    // For fresh DBs this creates the tables; for existing DBs it's a no-op.
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

        CREATE TABLE IF NOT EXISTS pending_worktree_removals (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            worktree_path TEXT NOT NULL UNIQUE,
            node_name TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_agent_nodes_mesh ON agent_nodes(mesh_id);
        "
    )?;

    // Safety nets: add any columns that may be missing on old or migrated DBs.
    // These are no-ops on fresh DBs (tables just created above have the base schema).
    ensure_mesh_columns(&conn)?;
    ensure_agent_node_source_issue(&conn)?;
    ensure_agent_node_use_worktree(&conn)?;
    ensure_agent_node_position(&conn)?;
    ensure_agent_node_status_changed_at(&conn)?;
    ensure_agent_node_source_pr(&conn)?;
    ensure_agent_node_source_pr_fork_meta(&conn)?;
    ensure_agent_node_source_pr_pinned_sha(&conn)?;
    ensure_checkpoints_dropped(&conn)?;
    ensure_mesh_scratchpad(&conn)?;
    ensure_mesh_sandbox(&conn)?;
    // Data migration (#495): rehash any coordinator token a pre-hashing build
    // left as cleartext. Idempotent, so it's safe to run on every init.
    ensure_coordinator_tokens_hashed(&conn)?;

    DB.set(Mutex::new(conn)).map_err(|_| rusqlite::Error::InvalidParameterName("db already initialized".to_string()))?;
    Ok(())
}

fn migrate_if_needed(conn: &Connection) -> SqlResult<()> {
    let current_version: i32 = conn
        .query_row("SELECT value FROM app_settings WHERE key = 'schema_version'", [], |row| {
            row.get::<_, String>(0).map(|v| v.parse().unwrap_or(0))
        })
        .unwrap_or(0);

    if current_version < SCHEMA_VERSION {
        tracing::info!("Migrating database from version {} to {}", current_version, SCHEMA_VERSION);

        // NOTE: this branch is gated on the pre-v6 `projects` table existing,
        // so it does NOT run for users upgrading from v6+. Those upgrades are
        // handled by the `ensure_*` safety nets in init() — add one per new
        // column. Do not "fix" this guard without first refactoring the inner
        // migrate_projects_* helpers, which still reference the renamed-away
        // `projects` table and would crash on a v6+ schema.
        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='projects'",
                [],
                |row| row.get::<_, i64>(0).map(|c| c > 0),
            )
            .unwrap_or(false);

        if !table_exists {
            // Fresh DB: init() will create the table with layout column.
            // Update version now so we don't re-enter migration on next init().
        } else {
            // Existing DB: run incremental migrations.
            migrate_projects_layout(conn)?;
            migrate_projects_position(conn)?;
            migrate_sessions_worktree_name(conn)?;
            if current_version < 7 {
                migrate_remote_access_token(conn)?;
            }
            if current_version < 8 {
                migrate_mesh_columns(conn)?;
            }
            if current_version < 9 {
                migrate_agent_node_source_issue(conn)?;
            }
            if current_version < 10 {
                migrate_gemini_to_agy(conn)?;
            }
            if current_version < 11 {
                migrate_agent_node_use_worktree(conn)?;
            }
        }

        conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
    }
    Ok(())
}

fn migrate_mesh_columns(conn: &Connection) -> SqlResult<()> {
    let columns = [
        ("build_command", "TEXT"),
        ("run_command", "TEXT"),
        ("model", "TEXT"),
        ("effort", "TEXT"),
        ("use_worktree", "INTEGER NOT NULL DEFAULT 1"),
        ("worktree_mode", "TEXT"),
        ("default_provider", "TEXT"),
        ("base_ref", "TEXT NOT NULL DEFAULT 'origin/main'"),
    ];

    for (name, ty) in columns {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('meshes') WHERE name = ?1",
                [name],
                |row| row.get(0),
            ).unwrap_or(false);
        if !has_col {
            conn.execute(&format!("ALTER TABLE meshes ADD COLUMN {} {}", name, ty), [])?;
            tracing::info!("Added {} column to meshes table", name);
        }
    }
    Ok(())
}

/// Shared safety-net helper (issue #456): add `column` of type
/// `col_type_with_default` to `table` if (and only if) the table exists and
/// the column is missing. The four-step pattern (table-exists guard,
/// `pragma_table_info` check, `ALTER TABLE ADD COLUMN`, `tracing::warn!`) is
/// shared by every `ensure_*` safety net — folding it into one helper means
/// a new column costs a single line, not 25+.
///
/// Returns `Ok(true)` if the column was added by this call, `Ok(false)` if
/// it was already present (or the table doesn't exist). The `bool` is
/// load-bearing for backfill wrappers — `ensure_agent_node_position` and
/// `ensure_agent_node_status_changed_at` only need to backfill existing
/// rows on the *first* add, and re-running the backfill would clobber any
/// positions a user has re-arranged via drag-to-reorder (issue #65).
///
/// The helper does NOT log itself — each `ensure_*` wrapper logs in its own
/// name, so a `tracing` grep lands on the safety-net function that ran
/// (not the generic helper).
fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    col_type_with_default: &str,
) -> SqlResult<bool> {
    let table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        )
        .unwrap_or(false);
    if !table_exists {
        return Ok(false);
    }

    let has_col: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info(?1) WHERE name=?2",
            rusqlite::params![table, column],
            |row| row.get(0),
        )
        .unwrap_or(false);
    if has_col {
        return Ok(false);
    }

    conn.execute(
        &format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, col_type_with_default),
        [],
    )?;
    Ok(true)
}

/// Safety net: ensure the v9 source_issue column exists on agent_nodes.
/// Same shape as ensure_mesh_columns — fixes DBs whose schema_version
/// was bumped past 9 without the column being added because the migration
/// guard skipped them (see ensure_mesh_columns for the same bug class).
pub(crate) fn ensure_agent_node_source_issue(conn: &Connection) -> SqlResult<()> {
    if ensure_column(conn, "agent_nodes", "source_issue", "INTEGER")? {
        tracing::warn!("ensure_agent_node_source_issue: added missing source_issue column");
    }
    Ok(())
}

fn migrate_agent_node_use_worktree(conn: &Connection) -> SqlResult<()> {
    let has_col: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'use_worktree'",
            [],
            |row| row.get(0),
        ).unwrap_or(false);
    if !has_col {
        conn.execute("ALTER TABLE agent_nodes ADD COLUMN use_worktree INTEGER NOT NULL DEFAULT 1", [])?;
        tracing::info!("Added use_worktree column to agent_nodes table");
    }
    Ok(())
}

pub(crate) fn ensure_agent_node_use_worktree(conn: &Connection) -> SqlResult<()> {
    if ensure_column(conn, "agent_nodes", "use_worktree", "INTEGER NOT NULL DEFAULT 1")? {
        tracing::warn!("ensure_agent_node_use_worktree: added missing use_worktree column");
    }
    Ok(())
}

/// Safety net (v13): ensure the `position` column exists on agent_nodes, used
/// for drag-to-reorder grid order within a mesh. On first add, backfill each
/// node's position as its 0-based rank by `created_at` within its own mesh, so
/// existing nodes keep the order they already render in (lists previously
/// sorted by `created_at ASC`). Ties broken by `id` for determinism.
///
/// The backfill is a multi-step migration, not a one-column add, so it stays
/// inline in this wrapper — the helper covers the table-exists + ALTER step,
/// the `if` guards the backfill so a re-run never overwrites a position the
/// user has re-arranged via drag-to-reorder (issue #65).
pub(crate) fn ensure_agent_node_position(conn: &Connection) -> SqlResult<()> {
    if ensure_column(conn, "agent_nodes", "position", "INTEGER NOT NULL DEFAULT 0")? {
        conn.execute(
            "UPDATE agent_nodes SET position = (
                 SELECT COUNT(*) FROM agent_nodes AS earlier
                 WHERE earlier.mesh_id = agent_nodes.mesh_id
                   AND (earlier.created_at < agent_nodes.created_at
                        OR (earlier.created_at = agent_nodes.created_at AND earlier.id < agent_nodes.id))
             )",
            [],
        )?;
        tracing::warn!("ensure_agent_node_position: added missing position column and backfilled per-mesh order");
    }
    Ok(())
}

/// Safety net (v14): ensure the `status_changed_at` column exists on
/// agent_nodes. It records when a node last changed lifecycle status, which
/// the coordinator read API (ADR-0008) turns into the digest's `last_activity`
/// and — when the node is `awaiting_input` — `waiting_since`. On first add,
/// backfill existing rows to `created_at` so a pre-v14 node reports a sane
/// (if coarse) activity time instead of NULL. SQLite forbids a non-constant
/// default on `ALTER TABLE ADD COLUMN`, hence the add-then-backfill two-step.
pub(crate) fn ensure_agent_node_status_changed_at(conn: &Connection) -> SqlResult<()> {
    if ensure_column(conn, "agent_nodes", "status_changed_at", "TEXT")? {
        conn.execute(
            "UPDATE agent_nodes SET status_changed_at = created_at WHERE status_changed_at IS NULL",
            [],
        )?;
        tracing::warn!("ensure_agent_node_status_changed_at: added column and backfilled from created_at");
    }
    Ok(())
}

/// Safety net (v15): ensure the `source_pr` column exists on agent_nodes.
/// Added for issue #420 — PR-spawned nodes record the originating PR number
/// so the spawn path can fetch the head ref and use it as the worktree's
/// `base_ref` (worktree adoption, #36). `None` for every existing row, so no
/// backfill is needed — `#[serde(default)]` on the Rust struct (and the
/// generated TS shape) makes the absence explicit and the spawn path
/// branches on it.
pub(crate) fn ensure_agent_node_source_pr(conn: &Connection) -> SqlResult<()> {
    if ensure_column(conn, "agent_nodes", "source_pr", "INTEGER")? {
        tracing::warn!("ensure_agent_node_source_pr: added missing source_pr column");
    }
    Ok(())
}

/// Safety net (v16): ensure the `head_repo_owner` + `head_repo_clone_url`
/// columns exist on `agent_nodes`. Added for issue #443 — PR-spawned nodes
/// spawned from a fork PR record the fork's owner login and clone URL so
/// `spawn_agent_inner` can add the fork as a remote and fetch the head ref
/// (worktree adoption for fork PRs, #36). Both columns are nullable; only
/// rows spawned from a fork PR set them. Pre-v16 rows stay `NULL` and the
/// spawn path treats that as "same-repo PR" (the #420 path).
pub(crate) fn ensure_agent_node_source_pr_fork_meta(conn: &Connection) -> SqlResult<()> {
    for (name, ty) in [("head_repo_owner", "TEXT"), ("head_repo_clone_url", "TEXT")] {
        if ensure_column(conn, "agent_nodes", name, ty)? {
            tracing::warn!(
                "ensure_agent_node_source_pr_fork_meta: added missing {} column",
                name
            );
        }
    }
    Ok(())
}

/// Safety net (v16): ensure the `source_pr_pinned_sha` column exists on
/// agent_nodes. Added for issue #444 — PR-spawned nodes may store the
/// originating PR's head commit SHA so the spawn path can verify the local
/// `origin/<head_ref>` SHA matches it after `git fetch` and emit a
/// `pr_sha_drift` warning if the PR was force-pushed (or rebased) between
/// the user clicking Spawn and the worktree being cut. The column is
/// nullable: `None` for every existing v15 PR-spawned node (no SHA was
/// known at the time), and `None` for issue-spawned / hand-spawned nodes
/// entirely. The drift-check path branches on `Some(_)` so a `None` skips
/// the comparison rather than failing — same fail-open semantics as the
/// existing `pr_head_unfetchable` fallback.
pub(crate) fn ensure_agent_node_source_pr_pinned_sha(conn: &Connection) -> SqlResult<()> {
    if ensure_column(conn, "agent_nodes", "source_pr_pinned_sha", "TEXT")? {
        tracing::warn!("ensure_agent_node_source_pr_pinned_sha: added missing source_pr_pinned_sha column");
    }
    Ok(())
}

/// Safety net (v12): drop the obsolete `checkpoints` table. The checkpoint
/// feature was removed, so the table is dead. This runs every init() rather
/// than as a version-gated migration because the gated branch only fires for
/// pre-v6 DBs (it's guarded on the legacy `projects` table); v6+ DBs rely on
/// these `ensure_*` nets instead. `DROP TABLE IF EXISTS` is fully idempotent.
pub(crate) fn ensure_checkpoints_dropped(conn: &Connection) -> SqlResult<()> {
    conn.execute("DROP TABLE IF EXISTS checkpoints", [])?;
    Ok(())
}

/// Safety net: ensure all v8 user-tunable columns exist on the meshes table.
/// Called after migrate_if_needed to fix DBs that skipped migration due to
/// the projects-table guard (existing DBs that already had schema_version=8
/// but whose meshes table lacked those columns).
fn ensure_mesh_columns(conn: &Connection) -> SqlResult<()> {
    let columns = [
        ("build_command", "TEXT"),
        ("run_command", "TEXT"),
        ("model", "TEXT"),
        ("effort", "TEXT"),
        ("use_worktree", "INTEGER NOT NULL DEFAULT 1"),
        ("worktree_mode", "TEXT"),
        ("default_provider", "TEXT"),
        ("base_ref", "TEXT NOT NULL DEFAULT 'origin/main'"),
    ];
    for (name, ty) in columns {
        if ensure_column(conn, "meshes", name, ty)? {
            tracing::warn!("ensure_mesh_columns: added missing column {}", name);
        }
    }
    Ok(())
}

/// Safety net (v17): ensure the `scratchpad` column exists on `meshes`.
/// Added for the Scratch Pad Probe tab — a mesh-scoped free-text note field
/// owned entirely by Buildmesh (not visible to agents, not on disk). Always
/// non-null with a `''` default, so the no-mesh-yet case and the
/// already-has-notes case are indistinguishable at the read boundary. No
/// backfill needed — pre-v17 rows just get the empty string.
pub(crate) fn ensure_mesh_scratchpad(conn: &Connection) -> SqlResult<()> {
    if ensure_column(conn, "meshes", "scratchpad", "TEXT NOT NULL DEFAULT ''")? {
        tracing::warn!("ensure_mesh_scratchpad: added missing scratchpad column");
    }
    Ok(())
}

/// Safety net (v18): ensure the `sandbox` column exists on `meshes`.
/// Added for the macOS Seatbelt sandbox toggle (issue #497) — a per-mesh
/// default for whether agent PTY processes are confined via `sandbox-exec`.
/// Off by default (`0`): the feature is macOS-only and opt-in, so pre-v18 rows
/// and non-macOS hosts simply read `false`. No backfill needed.
pub(crate) fn ensure_mesh_sandbox(conn: &Connection) -> SqlResult<()> {
    if ensure_column(conn, "meshes", "sandbox", "INTEGER NOT NULL DEFAULT 0")? {
        tracing::warn!("ensure_mesh_sandbox: added missing sandbox column");
    }
    Ok(())
}

fn migrate_projects_layout(conn: &Connection) -> SqlResult<()> {
    let has_layout: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('projects') WHERE name = 'layout'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if !has_layout {
        conn.execute(
            "ALTER TABLE projects ADD COLUMN layout TEXT NOT NULL DEFAULT 'grid'",
            [],
        )?;
        tracing::info!("Added layout column to projects table");
    }
    Ok(())
}

fn migrate_projects_position(conn: &Connection) -> SqlResult<()> {
    let has_position: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('projects') WHERE name = 'position'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if !has_position {
        conn.execute(
            "ALTER TABLE projects ADD COLUMN position INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
        tracing::info!("Added position column to projects table");
        conn.execute(
            "UPDATE projects SET position = (
                SELECT COUNT(*) FROM projects p2 WHERE p2.created_at < projects.created_at
            )",
            [],
        )?;
    }
    Ok(())
}

fn migrate_sessions_worktree_name(conn: &Connection) -> SqlResult<()> {
    // Guard: sessions table may not exist in very old schemas (v2-v3)
    let table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='sessions'",
            [],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        )
        .unwrap_or(false);

    if !table_exists {
        return Ok(());
    }

    let has_worktree_name: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('sessions') WHERE name = 'worktree_name'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if !has_worktree_name {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN worktree_name TEXT",
            [],
        )?;
        tracing::info!("Added worktree_name column to sessions table");
    }
    Ok(())
}

#[allow(dead_code)]
fn migrate_mesh_rename(conn: &Connection) -> SqlResult<()> {
    // Guard: only rename if old table names exist (upgrade path from v5)
    let projects_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='projects'",
            [],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        )
        .unwrap_or(false);

    if !projects_exists {
        // Already migrated or fresh install — nothing to do
        return Ok(());
    }

    // Also guard on sessions — partial schemas (v2 without sessions) would crash otherwise
    let sessions_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='sessions'",
            [],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        )
        .unwrap_or(false);

    // Always rename projects→meshes; only rename sessions-related tables if they exist.
    // Without this, DBs that have `projects` but no `sessions` (v2 schema) would skip
    // the rename and then crash in migrate_mesh_columns (which references `meshes`).
    if !sessions_exists {
        conn.execute("ALTER TABLE projects RENAME TO meshes", [])?;
        tracing::info!("Migrated projects→meshes (no sessions table present)");
        return Ok(());
    }

    let result: SqlResult<()> = (|| {
        conn.execute("BEGIN TRANSACTION", [])?;
        conn.execute("ALTER TABLE projects RENAME TO meshes", [])?;
        conn.execute("ALTER TABLE sessions RENAME TO agent_nodes", [])?;
        conn.execute("ALTER TABLE agent_nodes RENAME COLUMN project_id TO mesh_id", [])?;
        conn.execute("COMMIT", [])?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            tracing::info!("Migrated to v6: projects→meshes, sessions→agent_nodes, project_id→mesh_id");
        }
        Err(e) => {
            conn.execute("ROLLBACK", [])?;
            return Err(e);
        }
    }
    Ok(())
}

fn migrate_remote_access_token(conn: &Connection) -> SqlResult<()> {
    // Ensure the remote_access_token key exists in app_settings with a generated token
    let has_token: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM app_settings WHERE key = 'remote_access_token'",
            [],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        )
        .unwrap_or(false);

    if !has_token {
        let token = generate_token();
        conn.execute(
            "INSERT INTO app_settings (key, value) VALUES ('remote_access_token', ?1)",
            params![&token],
        )?;
        tracing::info!("Generated remote access root token");
    }
    Ok(())
}

fn migrate_agent_node_source_issue(conn: &Connection) -> SqlResult<()> {
    let table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='agent_nodes'",
            [],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        )
        .unwrap_or(false);

    if !table_exists {
        return Ok(());
    }

    let has_col: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'source_issue'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if !has_col {
        conn.execute("ALTER TABLE agent_nodes ADD COLUMN source_issue INTEGER", [])?;
        tracing::info!("Added source_issue column to agent_nodes table");
    }
    Ok(())
}

fn migrate_gemini_to_agy(conn: &Connection) -> SqlResult<()> {
    let rows_agents = conn.execute(
        "UPDATE agent_nodes SET provider = 'agy' WHERE provider = 'gemini'",
        [],
    )?;
    if rows_agents > 0 {
        tracing::info!("Migrated {} agent_nodes from gemini to agy", rows_agents);
    }
    let rows_meshes = conn.execute(
        "UPDATE meshes SET default_provider = 'agy' WHERE default_provider = 'gemini'",
        [],
    )?;
    if rows_meshes > 0 {
        tracing::info!("Migrated {} meshes default_provider from gemini to agy", rows_meshes);
    }
    Ok(())
}

/// Generate a random 32-character hex token (16 bytes of random data).
fn generate_token() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: [u8; 16] = rng.random();
    hex::encode(bytes)
}

/// Hash a token for at-rest storage (issue #495). Returns the lowercase
/// SHA-256 hex (64 chars). Tokens are high-entropy (128-bit random hex from
/// `generate_token`), so a plain SHA-256 is the right primitive here — no salt
/// or slow KDF, which exist to slow brute force on *low-entropy* passwords.
/// Because a raw token is 32 chars and this output is 64, the length alone
/// distinguishes a pre-hashing cleartext value from an already-hashed one
/// (used by `ensure_coordinator_tokens_hashed` to migrate idempotently).
pub(crate) fn hash_token(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(raw.as_bytes()))
}

/// Get or create the root remote access token (stored in app_settings).
pub fn get_or_create_root_token() -> SqlResult<String> {
    let db = get().lock().unwrap();

    let existing: Option<String> = db.query_row(
        "SELECT value FROM app_settings WHERE key = 'remote_access_token'",
        [],
        |row| row.get(0),
    ).ok();

    if let Some(token) = existing {
        if !token.is_empty() {
            return Ok(token);
        }
    }

    let token = generate_token();
    db.execute(
        "INSERT INTO app_settings (key, value) VALUES ('remote_access_token', ?1)",
        params![&token],
    )?;
    Ok(token)
}

/// Validate the root remote access token.
pub fn validate_root_token(token: &str) -> SqlResult<bool> {
    let db = get().lock().unwrap();
    let stored: Option<String> = db.query_row(
        "SELECT value FROM app_settings WHERE key = 'remote_access_token'",
        [],
        |row| row.get(0),
    ).ok();

    Ok(stored.as_deref().unwrap_or("") == token)
}

// --- Coordinator read API auth (ADR-0008) ---
//
// The coordinator surface is a SEPARATE, capability-scoped credential from the
// mobile root token, gated behind a master enable switch that defaults OFF.
// A read-scoped token can never be used to drive nodes (drive is a future
// slice). All three keys live in app_settings alongside `remote_access_token`.
const COORDINATOR_ENABLED_KEY: &str = "coordinator_api_enabled";
const COORDINATOR_READ_TOKEN_KEY: &str = "coordinator_read_token";
// Drive (write) side (issue #319). The drive scope is a SEPARATE token from the
// read token — a read-scoped credential can never drive a node — behind its own
// enable switch (also defaulting OFF) so drive can be killed independently while
// reads stay up. Both still sit under the coordinator master switch above, so
// disabling the whole surface disables drive too.
const COORDINATOR_DRIVE_ENABLED_KEY: &str = "coordinator_drive_enabled";
const COORDINATOR_DRIVE_TOKEN_KEY: &str = "coordinator_drive_token";

/// Is the coordinator read API enabled? Defaults to `false` (off) for a fresh
/// install, so a naive setup is never an open endpoint.
pub fn coordinator_api_enabled() -> SqlResult<bool> {
    let db = get().lock().unwrap();
    coordinator_api_enabled_inner(&db)
}

pub fn coordinator_api_enabled_inner(conn: &Connection) -> SqlResult<bool> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![COORDINATOR_ENABLED_KEY],
            |row| row.get(0),
        )
        .ok();
    Ok(value.as_deref() == Some("1"))
}

/// Flip the master enable switch for the coordinator read API.
pub fn set_coordinator_api_enabled(enabled: bool) -> SqlResult<()> {
    let db = get().lock().unwrap();
    set_coordinator_api_enabled_inner(&db, enabled)
}

pub fn set_coordinator_api_enabled_inner(conn: &Connection, enabled: bool) -> SqlResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        params![COORDINATOR_ENABLED_KEY, if enabled { "1" } else { "0" }],
    )?;
    Ok(())
}

/// Whether the embedded HTTP/WS server may bind beyond loopback (issue #496).
/// Off by default: a fresh install binds only `127.0.0.1`/`::1`, so external
/// devices on the LAN cannot reach the hub without an explicit opt-in. Enabling
/// it is what lets a phone connect over LAN/VPN (TLS for that path is a later
/// slice). The secure default is enforced by `http::start_http_server` reading
/// this before choosing its bind addresses. The settings toggle that writes
/// this flag arrives with the LAN-exposure UI slice (PRD #494).
const LAN_EXPOSURE_ENABLED_KEY: &str = "lan_exposure_enabled";

/// Is LAN/VPN exposure enabled? Defaults to `false` (loopback-only) so a naive
/// setup is never reachable from another machine.
pub fn lan_exposure_enabled() -> SqlResult<bool> {
    let db = get().lock().unwrap();
    lan_exposure_enabled_inner(&db)
}

pub fn lan_exposure_enabled_inner(conn: &Connection) -> SqlResult<bool> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![LAN_EXPOSURE_ENABLED_KEY],
            |row| row.get(0),
        )
        .ok();
    Ok(value.as_deref() == Some("1"))
}

/// Mint (or replace) the read-scoped coordinator token, returning it. Minting a
/// fresh token invalidates any previously issued one.
pub fn generate_coordinator_read_token() -> SqlResult<String> {
    let db = get().lock().unwrap();
    generate_coordinator_read_token_inner(&db)
}

pub fn generate_coordinator_read_token_inner(conn: &Connection) -> SqlResult<String> {
    // Return the raw token to the caller once; persist only its hash (#495) so a
    // DB dump or rogue agent reading app_settings can't recover the secret.
    let token = generate_token();
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        params![COORDINATOR_READ_TOKEN_KEY, hash_token(&token)],
    )?;
    Ok(token)
}

/// The stored read token *hash*, if one has been minted (and is non-empty).
/// Used by the status command to report `has_token` — presence only; the value
/// is a SHA-256 hash (#495), never the raw token.
pub fn coordinator_read_token() -> SqlResult<Option<String>> {
    let db = get().lock().unwrap();
    coordinator_read_token_inner(&db)
}

pub fn coordinator_read_token_inner(conn: &Connection) -> SqlResult<Option<String>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![COORDINATOR_READ_TOKEN_KEY],
            |row| row.get(0),
        )
        .ok();
    Ok(value.filter(|v| !v.is_empty()))
}

/// Validate a presented token for READ access. Rejects unless the API is
/// enabled AND the token matches the minted read token — so disabling the
/// master switch instantly cuts off all read access even with a valid token.
pub fn validate_coordinator_read_token(token: &str) -> SqlResult<bool> {
    let db = get().lock().unwrap();
    validate_coordinator_read_token_inner(&db, token)
}

pub fn validate_coordinator_read_token_inner(conn: &Connection, token: &str) -> SqlResult<bool> {
    if token.is_empty() || !coordinator_api_enabled_inner(conn)? {
        return Ok(false);
    }
    // The DB holds only the hash (#495), so hash the presented token and compare
    // hashes. The raw token never has to be reconstructed to authenticate.
    match coordinator_read_token_inner(conn)? {
        Some(stored) => Ok(stored == hash_token(token)),
        None => Ok(false),
    }
}

// --- Coordinator drive (write) auth (ADR-0008 §5, issue #319) ---

/// Is the drive side enabled? Defaults to `false` (off), independent of the
/// read side, so a deployment can offer read-only coordination without ever
/// exposing the ability to write to a node's PTY. (No process-global wrapper
/// yet — only `validate_coordinator_drive_token_inner` reads this; a drive
/// Settings slice can add the public getter when it surfaces the state.)
pub fn coordinator_drive_enabled_inner(conn: &Connection) -> SqlResult<bool> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![COORDINATOR_DRIVE_ENABLED_KEY],
            |row| row.get(0),
        )
        .ok();
    Ok(value.as_deref() == Some("1"))
}

/// Flip the drive kill-switch. Off by default; killing it stops all driving
/// while leaving the read surface untouched.
pub fn set_coordinator_drive_enabled(enabled: bool) -> SqlResult<()> {
    let db = get().lock().unwrap();
    set_coordinator_drive_enabled_inner(&db, enabled)
}

pub fn set_coordinator_drive_enabled_inner(conn: &Connection, enabled: bool) -> SqlResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        params![COORDINATOR_DRIVE_ENABLED_KEY, if enabled { "1" } else { "0" }],
    )?;
    Ok(())
}

/// Mint (or replace) the drive-scoped coordinator token. Distinct from the read
/// token, so granting drive is an explicit, separate act from granting read.
pub fn generate_coordinator_drive_token() -> SqlResult<String> {
    let db = get().lock().unwrap();
    generate_coordinator_drive_token_inner(&db)
}

pub fn generate_coordinator_drive_token_inner(conn: &Connection) -> SqlResult<String> {
    // Raw token returned once; only its hash is persisted (#495).
    let token = generate_token();
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        params![COORDINATOR_DRIVE_TOKEN_KEY, hash_token(&token)],
    )?;
    Ok(token)
}

/// The stored drive token *hash*, if one has been minted (and is non-empty).
/// Only the validator reads it today (no process-global getter until a Settings
/// slice reports drive state); kept `_inner` so a future caller can lock once.
/// The value is a SHA-256 hash (#495), never the raw token.
pub fn coordinator_drive_token_inner(conn: &Connection) -> SqlResult<Option<String>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![COORDINATOR_DRIVE_TOKEN_KEY],
            |row| row.get(0),
        )
        .ok();
    Ok(value.filter(|v| !v.is_empty()))
}

/// Validate a presented token for DRIVE access. Rejects unless the coordinator
/// master switch is on AND the drive kill-switch is on AND the token matches the
/// minted drive token. Because the drive token is stored under its own key, a
/// read-scoped token never validates here — drive is its own capability.
pub fn validate_coordinator_drive_token(token: &str) -> SqlResult<bool> {
    let db = get().lock().unwrap();
    validate_coordinator_drive_token_inner(&db, token)
}

pub fn validate_coordinator_drive_token_inner(conn: &Connection, token: &str) -> SqlResult<bool> {
    if token.is_empty()
        || !coordinator_api_enabled_inner(conn)?
        || !coordinator_drive_enabled_inner(conn)?
    {
        return Ok(false);
    }
    // Stored value is the hash (#495); compare against the hashed presentation.
    match coordinator_drive_token_inner(conn)? {
        Some(stored) => Ok(stored == hash_token(token)),
        None => Ok(false),
    }
}

/// Safety net (issue #495): rehash any coordinator token still stored as
/// pre-hashing cleartext. A token minted before token hashing sits in
/// `app_settings` as the 32-char raw hex output of `generate_token`; this
/// rewrites it in place to its SHA-256 so a DB dump can no longer reveal the
/// secret, while the raw token the user already holds keeps validating (the
/// validator hashes the incoming token). Idempotent: a SHA-256 hex is 64 chars,
/// so an already-hashed (or empty) value is left untouched.
///
/// The root token (`remote_access_token`) is deliberately excluded: the Remote
/// Access QR re-reads its *raw* value on every open, so it stays cleartext until
/// the Keychain/device-token slice moves it out of SQLite (PRD #494).
pub(crate) fn ensure_coordinator_tokens_hashed(conn: &Connection) -> SqlResult<()> {
    for key in [COORDINATOR_READ_TOKEN_KEY, COORDINATOR_DRIVE_TOKEN_KEY] {
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM app_settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .ok();
        // Only a raw token (the 32-char `generate_token` output) needs
        // rewriting; a 64-char hash is already migrated and an empty value is
        // nothing to hash.
        if let Some(raw) = value {
            if raw.len() == 32 {
                conn.execute(
                    "UPDATE app_settings SET value = ?2 WHERE key = ?1",
                    params![key, hash_token(&raw)],
                )?;
                tracing::warn!("ensure_coordinator_tokens_hashed: rehashed cleartext {}", key);
            }
        }
    }
    Ok(())
}

/// Exposes migrate_if_needed for integration testing.
/// In tests, call this on an existing Connection to simulate schema upgrade.
#[cfg(test)]
pub(crate) fn test_migrate_if_needed(conn: &Connection) -> SqlResult<()> {
    let current_version: i32 = conn
        .query_row("SELECT value FROM app_settings WHERE key = 'schema_version'", [], |row| {
            row.get::<_, String>(0).map(|v| v.parse().unwrap_or(0))
        })
        .unwrap_or(0);

    if current_version < SCHEMA_VERSION {
        migrate_projects_layout(conn)?;
        migrate_projects_position(conn)?;
        migrate_sessions_worktree_name(conn)?;
        if current_version < 6 {
            migrate_mesh_rename(conn)?;
        }
        if current_version < 7 {
            migrate_remote_access_token(conn)?;
        }
        if current_version < 8 {
            migrate_mesh_columns(conn)?;
        }
        if current_version < 9 {
            migrate_agent_node_source_issue(conn)?;
        }
        conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
    }
    Ok(())
}

pub fn get() -> &'static Mutex<Connection> {
    DB.get().expect("database not initialized")
}

// --- Internal Helpers (no locking) ---

/// Canonical column projection for reading a `Mesh` row. The `COALESCE`
/// defaults must stay in sync with `map_mesh_row`'s positional `row.get`s.
const MESH_COLUMNS: &str =
    "id, name, path, layout, position, created_at, \
     COALESCE(build_command, ''), COALESCE(run_command, ''), \
     COALESCE(model, ''), COALESCE(effort, ''), \
     COALESCE(use_worktree, 1), COALESCE(worktree_mode, ''), \
     COALESCE(default_provider, ''), COALESCE(base_ref, 'origin/main'), \
     scratchpad, COALESCE(sandbox, 0)";

/// Map a row selected with `MESH_COLUMNS` into a `Mesh`. Single place that
/// normalizes empty config strings to `None` (via `parse_str`).
fn map_mesh_row(row: &rusqlite::Row) -> rusqlite::Result<Mesh> {
    Ok(Mesh {
        id: row.get(0)?,
        name: row.get(1)?,
        path: row.get(2)?,
        layout: row.get::<_, String>(3)?,
        position: row.get(4)?,
        created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now()),
        build_command: parse_str(row.get::<_, String>(6)?),
        run_command: parse_str(row.get::<_, String>(7)?),
        model: parse_str(row.get::<_, String>(8)?),
        effort: parse_str(row.get::<_, String>(9)?),
        use_worktree: row.get::<_, i32>(10)? != 0,
        worktree_mode: parse_str(row.get::<_, String>(11)?),
        default_provider: parse_str(row.get::<_, String>(12)?),
        base_ref: row.get::<_, String>(13)?,
        scratchpad: row.get(14)?,
        sandbox: row.get::<_, i32>(15)? != 0,
    })
}

fn get_mesh_by_id_inner(conn: &Connection, id: i64) -> SqlResult<Mesh> {
    let mut stmt = conn.prepare(
        &format!("SELECT {} FROM meshes WHERE id = ?1", MESH_COLUMNS)
    )?;
    stmt.query_row(params![id], map_mesh_row)
}

fn parse_str(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

const AGENT_NODE_COLUMNS: &str =
    "id, mesh_id, name, path, branch, env, provider, status, cli_session_id, worktree_name, created_at, source_issue, use_worktree, position, source_pr, head_repo_owner, head_repo_clone_url, source_pr_pinned_sha";

fn map_agent_node_row(row: &rusqlite::Row) -> rusqlite::Result<AgentNode> {
    Ok(AgentNode {
        id: row.get(0)?,
        mesh_id: row.get(1)?,
        name: row.get(2)?,
        path: row.get(3)?,
        branch: row.get(4)?,
        env: EnvType::from_db_str(&row.get::<_, String>(5)?),
        provider: Provider::from_db_str(&row.get::<_, String>(6)?),
        status: SessionStatus::from_db_str(&row.get::<_, String>(7)?),
        cli_session_id: row.get(8)?,
        worktree_name: row.get(9)?,
        use_worktree: row.get::<_, i32>(12)? != 0,
        source_issue: row.get(11)?,
        position: row.get(13)?,
        // source_pr is at index 14. Read as Option: the safety net adds the
        // column nullable for pre-v15 DBs, and rusqlite's typed read errors
        // the row on NULL otherwise. (v16 added head_repo_owner +
        // head_repo_clone_url at 15/16, and source_pr_pinned_sha at 17 —
        // see AGENT_NODE_COLUMNS.)
        source_pr: row.get(14)?,
        head_repo_owner: row.get(15)?,
        head_repo_clone_url: row.get(16)?,
        // source_pr_pinned_sha is at index 17 (issue #444). Same nullable
        // pattern as `source_pr`: a pre-v16 row that didn't store a SHA
        // reads back as `None`, and the drift-check path treats `None` as
        // "skip the comparison" rather than failing.
        source_pr_pinned_sha: row.get(17)?,
        created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(10)?)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now()),
    })
}

/// Parse a timestamp column that may be either RFC3339 (what Rust writes, e.g.
/// `update_agent_node_status`) or SQLite's `datetime('now')` form
/// (`YYYY-MM-DD HH:MM:SS`, what a column DEFAULT or backfill writes). Falls back
/// to "now" on an unparseable value so a malformed row degrades to a fresh
/// timestamp rather than erroring the whole query.
fn parse_db_timestamp(s: &str) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return dt.with_timezone(&chrono::Utc);
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return chrono::Utc.from_utc_datetime(&naive);
    }
    chrono::Utc::now()
}

/// Coordinator read API (ADR-0008): every non-archived Agent Node across all
/// Meshes, joined with its Mesh name and `status_changed_at`, in the same
/// order the grid renders. The two extra fields aren't on `AgentNode`, so we
/// return them alongside it; `coordinator::node_digest::spine` turns each tuple
/// into a Node Digest. Spine-only — no transcript enrichment in this slice.
pub fn list_coordinator_node_rows()
-> SqlResult<Vec<(AgentNode, String, chrono::DateTime<chrono::Utc>)>> {
    let db = get().lock().unwrap();
    list_coordinator_node_rows_inner(&db)
}

pub fn list_coordinator_node_rows_inner(
    conn: &Connection,
) -> SqlResult<Vec<(AgentNode, String, chrono::DateTime<chrono::Utc>)>> {
    // Qualify AGENT_NODE_COLUMNS with the `a.` alias (derived, never drifts)
    // so the join with `meshes` has no ambiguous `name`/`created_at`/`position`.
    let qualified: String = AGENT_NODE_COLUMNS
        .split(", ")
        .map(|c| format!("a.{}", c))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {qualified}, m.name, a.status_changed_at \
         FROM agent_nodes a JOIN meshes m ON a.mesh_id = m.id \
         WHERE a.status != 'archived' \
         ORDER BY a.mesh_id ASC, a.position ASC, a.created_at ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        // map_agent_node_row reads positional indices 0..17, which match the
        // AGENT_NODE_COLUMNS order we selected first; mesh name and
        // status_changed_at follow at 18 and 19 (v16 added head_repo_owner +
        // head_repo_clone_url at 15/16, and source_pr_pinned_sha at 17).
        let node = map_agent_node_row(row)?;
        let mesh_name: String = row.get(18)?;
        // Read as Option: a DB migrated from a pre-v14 schema added the column
        // nullable, so any row inserted before `create_agent_node` started
        // stamping it (or via some other path) can be NULL. A non-Option read
        // would make rusqlite error the whole query on a single NULL row,
        // blanking the endpoint. Fall back to the node's creation time.
        let status_changed_at: Option<String> = row.get(19)?;
        let status_changed_at = status_changed_at
            .map(|s| parse_db_timestamp(&s))
            .unwrap_or(node.created_at);
        Ok((node, mesh_name, status_changed_at))
    })?;
    rows.collect()
}

pub(crate) fn get_agent_node_by_id_inner(conn: &Connection, id: i64) -> SqlResult<AgentNode> {
    let mut stmt = conn.prepare(
        &format!("SELECT {} FROM agent_nodes WHERE id = ?1", AGENT_NODE_COLUMNS)
    )?;
    stmt.query_row(params![id], map_agent_node_row)
}

// --- Mesh operations ---

pub fn create_mesh(name: &str, path: &str) -> SqlResult<Mesh> {
    let db = get().lock().unwrap();

    // Check if mesh with this path already exists (idempotent upsert)
    let existing: Option<i64> = db.query_row(
        "SELECT id FROM meshes WHERE path = ?1",
        params![path],
        |row| row.get(0),
    ).ok();

    if let Some(id) = existing {
        return get_mesh_by_id_inner(&db, id);
    }

    // Append at end of position list
    let next_position: i64 = db.query_row(
        "SELECT COALESCE(MAX(position), 0) + 1 FROM meshes",
        [],
        |row| row.get(0),
    )?;

    db.execute(
        "INSERT INTO meshes (name, path, layout, position, use_worktree, base_ref)
         VALUES (?1, ?2, 'grid', ?3, 1, 'origin/main')",
        params![name, path, next_position],
    )?;
    let id = db.last_insert_rowid();
    get_mesh_by_id_inner(&db, id)
}

pub fn get_mesh_by_id(id: i64) -> SqlResult<Mesh> {
    let db = get().lock().unwrap();
    get_mesh_by_id_inner(&db, id)
}

pub fn update_mesh_layout(id: i64, layout: &str) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute(
        "UPDATE meshes SET layout = ?1 WHERE id = ?2",
        params![layout, id],
    )?;
    Ok(())
}

/// Read the scratch pad text for a mesh. Returns the empty string (not
/// an error) for an unknown mesh id so the frontend can mount a blank
/// editor without a second round-trip — Scratch Pad is a "type whatever
/// you want" surface and the absence of notes is the common case.
pub fn get_mesh_scratchpad(id: i64) -> SqlResult<String> {
    let db = get().lock().unwrap();
    get_mesh_scratchpad_inner(&db, id)
}

pub(crate) fn get_mesh_scratchpad_inner(conn: &Connection, id: i64) -> SqlResult<String> {
    // COALESCE keeps the contract even on a pre-v17 DB whose safety net
    // hasn't run yet (e.g. unit tests that construct the schema in
    // memory) — empty string instead of NULL.
    conn.query_row(
        "SELECT COALESCE(scratchpad, '') FROM meshes WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )
}

/// Overwrite a mesh's scratch pad text. Empty string is a normal value
/// (cleared notes), not a deletion. Returns an error if the mesh id
/// doesn't exist — the call site surfaces that to the frontend so a
/// debounced save that fires after the mesh was deleted doesn't silently
/// report "Saved" for a write that affected zero rows.
pub fn set_mesh_scratchpad(id: i64, content: &str) -> SqlResult<()> {
    let db = get().lock().unwrap();
    set_mesh_scratchpad_inner(&db, id, content)
}

pub(crate) fn set_mesh_scratchpad_inner(
    conn: &Connection,
    id: i64,
    content: &str,
) -> SqlResult<()> {
    let rows = conn.execute(
        "UPDATE meshes SET scratchpad = ?1 WHERE id = ?2",
        params![content, id],
    )?;
    if rows == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

pub fn update_mesh_positions_batch(updates: &[(i64, i64)]) -> SqlResult<()> {
    if updates.is_empty() { return Ok(()); }
    let db = get().lock().unwrap();
    for (id, pos) in updates {
        db.execute(
            "UPDATE meshes SET position = ?1 WHERE id = ?2",
            params![pos, id],
        )?;
    }
    Ok(())
}

pub fn list_meshes() -> SqlResult<Vec<Mesh>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM meshes ORDER BY position ASC, name ASC", MESH_COLUMNS)
    )?;
    let rows = stmt.query_map([], map_mesh_row)?;
    rows.collect()
}

/// Look up a mesh by its path.
pub fn get_mesh_by_path(path: &str) -> SqlResult<Mesh> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM meshes WHERE path = ?1", MESH_COLUMNS)
    )?;
    stmt.query_row(params![path], map_mesh_row)
}

pub fn delete_mesh(id: i64) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute("DELETE FROM agent_nodes WHERE mesh_id = ?1", params![id])?;
    db.execute("DELETE FROM meshes WHERE id = ?1", params![id])?;
    Ok(())
}

// --- Agent Node operations ---

#[allow(clippy::too_many_arguments)]
pub fn create_agent_node(
    mesh_id: i64,
    name: &str,
    path: &str,
    branch: &str,
    env: EnvType,
    provider: Provider,
    worktree_name: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    use_worktree: bool,
    head_repo_owner: Option<&str>,
    head_repo_clone_url: Option<&str>,
) -> SqlResult<AgentNode> {
    let db = get().lock().unwrap();
    // Append at the end of this mesh's grid order. New nodes land last so an
    // existing arrangement isn't disturbed by a fresh spawn.
    let next_position: i64 = db.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM agent_nodes WHERE mesh_id = ?1",
        params![mesh_id],
        |row| row.get(0),
    )?;
    // Stamp `status_changed_at` explicitly rather than leaning on the column
    // DEFAULT: a DB migrated from pre-v14 added the column nullable with NO
    // default (SQLite can't ALTER-add a non-constant default), so an INSERT that
    // omitted it would store NULL and break the coordinator digest query.
    db.execute(
        "INSERT INTO agent_nodes (mesh_id, name, path, branch, env, provider, status, worktree_name, source_issue, source_pr, source_pr_pinned_sha, use_worktree, position, status_changed_at, head_repo_owner, head_repo_clone_url)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'idle', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            mesh_id,
            name,
            path,
            branch,
            env.to_string(),
            provider.to_string(),
            worktree_name,
            source_issue,
            source_pr,
            source_pr_pinned_sha,
            if use_worktree { 1 } else { 0 },
            next_position,
            chrono::Utc::now().to_rfc3339(),
            head_repo_owner,
            head_repo_clone_url,
        ],
    )?;
    let id = db.last_insert_rowid();
    get_agent_node_by_id_inner(&db, id)
}

/// Persist new grid positions for a batch of agent nodes (drag-to-reorder).
/// Callers send the full new ordering for the affected mesh so the DB stays in
/// sync with the frontend's optimistic update. Mirrors `update_mesh_positions_batch`.
pub fn update_agent_node_positions_batch(updates: &[(i64, i64)]) -> SqlResult<()> {
    if updates.is_empty() { return Ok(()); }
    let db = get().lock().unwrap();
    for (id, pos) in updates {
        db.execute(
            "UPDATE agent_nodes SET position = ?1 WHERE id = ?2",
            params![pos, id],
        )?;
    }
    Ok(())
}

pub fn update_agent_node_name(id: i64, name: &str) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute(
        "UPDATE agent_nodes SET name = ?1 WHERE id = ?2",
        params![name, id],
    )?;
    Ok(())
}

pub fn get_agent_node_by_id(id: i64) -> SqlResult<AgentNode> {
    let db = get().lock().unwrap();
    get_agent_node_by_id_inner(&db, id)
}

pub fn list_agent_nodes() -> SqlResult<Vec<AgentNode>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM agent_nodes WHERE status != 'archived' ORDER BY mesh_id ASC, position ASC, created_at ASC", AGENT_NODE_COLUMNS)
    )?;
    let rows = stmt.query_map([], map_agent_node_row)?;
    rows.collect()
}

pub fn list_agent_nodes_by_mesh(mesh_id: i64) -> SqlResult<Vec<AgentNode>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM agent_nodes WHERE mesh_id = ?1 ORDER BY position ASC, created_at ASC", AGENT_NODE_COLUMNS)
    )?;
    let rows = stmt.query_map(params![mesh_id], map_agent_node_row)?;
    rows.collect()
}

pub fn update_agent_node_status(id: i64, status: SessionStatus) -> SqlResult<()> {
    let db = get().lock().unwrap();
    // Stamp `status_changed_at` on every transition — this is the single choke
    // point all lifecycle changes flow through (including attention mark/clear),
    // so the coordinator digest's `last_activity`/`waiting_since` stay accurate.
    // Stored as RFC3339 (unambiguous, timezone-aware) rather than SQLite's
    // space-separated `datetime('now')` form.
    db.execute(
        "UPDATE agent_nodes SET status = ?1, status_changed_at = ?2 WHERE id = ?3",
        params![status.to_db_str(), chrono::Utc::now().to_rfc3339(), id],
    )?;
    Ok(())
}

pub fn archive_agent_node(id: i64) -> SqlResult<()> {
    update_agent_node_status(id, SessionStatus::Archived)
}

pub fn update_cli_session_id(id: i64, cli_id: &str) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute("UPDATE agent_nodes SET cli_session_id = ?1 WHERE id = ?2", params![cli_id, id])?;
    Ok(())
}

/// Flip any nodes that cannot be running on startup to `suspended`.
///
/// Covers three states that all mean "we expected this node to be live but
/// its process is gone":
/// - `running` / `awaiting_input`: the agent process died with the app.
/// - `pending`: the two-stage spawn flow created the row in stage-1 but the
///   app crashed before stage-2 (`start_node_background`) could spawn the
///   process. Without this, a stuck `pending` row would render as a
///   perpetual "◌ Starting…" badge with no way to recover.
pub fn mark_running_nodes_suspended() -> SqlResult<usize> {
    let db = get().lock().unwrap();
    // Deliberately does NOT touch `status_changed_at`: a restart-time suspend is
    // bookkeeping, not agent activity, so the coordinator digest's
    // `last_activity` should keep reporting when the node *actually* last did
    // work (pre-crash), not the moment the app reopened. (See ADR-0008 spine.)
    let count = db.execute(
        "UPDATE agent_nodes SET status = 'suspended' WHERE status IN ('running', 'awaiting_input', 'pending')",
        [],
    )?;
    Ok(count)
}

pub fn list_suspended_nodes() -> SqlResult<Vec<AgentNode>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM agent_nodes WHERE status = 'suspended' AND cli_session_id IS NOT NULL", AGENT_NODE_COLUMNS)
    )?;
    let rows = stmt.query_map([], map_agent_node_row)?;
    rows.collect()
}

// --- Pending worktree removal queue ---
//
// Closing a node deletes its row immediately so the UI can drop it at once, but
// the worktree directory removal is slow and retry-prone. We record the intent
// here (atomically with the row delete) so a background task — or the next app
// launch — can finish the removal. `worktree_path` is UNIQUE so re-enqueuing the
// same path is a no-op rather than a duplicate.

fn enqueue_worktree_removal_inner(conn: &Connection, path: &str, node_name: &str) -> SqlResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO pending_worktree_removals (worktree_path, node_name) VALUES (?1, ?2)",
        params![path, node_name],
    )?;
    Ok(())
}

fn list_pending_worktree_removals_inner(conn: &Connection) -> SqlResult<Vec<PendingWorktreeRemoval>> {
    let mut stmt = conn.prepare(
        "SELECT worktree_path, node_name FROM pending_worktree_removals ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(PendingWorktreeRemoval {
            worktree_path: row.get(0)?,
            node_name: row.get(1)?,
        })
    })?;
    rows.collect()
}

fn delete_pending_worktree_removal_inner(conn: &Connection, path: &str) -> SqlResult<()> {
    conn.execute(
        "DELETE FROM pending_worktree_removals WHERE worktree_path = ?1",
        params![path],
    )?;
    Ok(())
}

fn delete_agent_node_enqueueing_removal_inner(
    conn: &Connection,
    id: i64,
    removal: Option<(&str, &str)>,
) -> SqlResult<()> {
    conn.execute("DELETE FROM agent_nodes WHERE id = ?1", params![id])?;
    if let Some((path, node_name)) = removal {
        enqueue_worktree_removal_inner(conn, path, node_name)?;
    }
    Ok(())
}

/// Delete an agent node row and, in the same transaction, enqueue its worktree
/// for background removal. Doing both atomically is what makes the optimistic
/// close honest: the system can never forget a worktree it owes a cleanup, even
/// if it's killed between the two writes.
pub fn delete_agent_node_enqueueing_removal(
    id: i64,
    removal: Option<(&str, &str)>,
) -> SqlResult<()> {
    let mut db = get().lock().unwrap();
    let tx = db.transaction()?;
    delete_agent_node_enqueueing_removal_inner(&tx, id, removal)?;
    tx.commit()
}

pub fn list_pending_worktree_removals() -> SqlResult<Vec<PendingWorktreeRemoval>> {
    let db = get().lock().unwrap();
    list_pending_worktree_removals_inner(&db)
}

pub fn delete_pending_worktree_removal(path: &str) -> SqlResult<()> {
    let db = get().lock().unwrap();
    delete_pending_worktree_removal_inner(&db, path)
}

