//! Database module using rusqlite for local SQLite storage

#[cfg(test)]
mod migration_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod mesh_tests;

use rusqlite::{Connection, params};
pub use rusqlite::Result as SqlResult;
use once_cell::sync::OnceCell;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::models::*;

static DB: OnceCell<Mutex<Connection>> = OnceCell::new();

/// Current schema version
const SCHEMA_VERSION: i32 = 10;

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

    // Safety nets: add any columns that may be missing on old or migrated DBs.
    // These are no-ops on fresh DBs (tables just created above have the base schema).
    ensure_mesh_config_columns(&conn)?;
    ensure_agent_node_source_issue(&conn)?;

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
                migrate_mesh_config_columns(conn)?;
            }
            if current_version < 9 {
                migrate_agent_node_source_issue(conn)?;
            }
            if current_version < 10 {
                migrate_gemini_to_agy(conn)?;
            }
        }

        conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
    }
    Ok(())
}

fn migrate_mesh_config_columns(conn: &Connection) -> SqlResult<()> {
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

/// Safety net: ensure the v9 source_issue column exists on agent_nodes.
/// Same shape as ensure_mesh_config_columns — fixes DBs whose schema_version
/// was bumped past 9 without the column being added because the migration
/// guard skipped them (see ensure_mesh_config_columns for the same bug class).
pub(crate) fn ensure_agent_node_source_issue(conn: &Connection) -> SqlResult<()> {
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
        tracing::warn!("ensure_agent_node_source_issue: added missing source_issue column");
    }
    Ok(())
}

/// Safety net: ensure all v8 config columns exist on the meshes table.
/// Called after migrate_if_needed to fix DBs that skipped migration due to
/// the projects-table guard (existing DBs that already had schema_version=8
/// but whose meshes table lacked the config columns).
fn ensure_mesh_config_columns(conn: &Connection) -> SqlResult<()> {
    let table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='meshes'",
            [],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        )
        .unwrap_or(false);
    if !table_exists {
        return Ok(());
    }

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
            )
            .unwrap_or(false);
        if !has_col {
            conn.execute(&format!("ALTER TABLE meshes ADD COLUMN {} {}", name, ty), [])?;
            tracing::warn!("ensure_mesh_config_columns: added missing column {}", name);
        }
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
    // the rename and then crash in migrate_mesh_config_columns (which references `meshes`).
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
        conn.execute("ALTER TABLE checkpoints RENAME COLUMN session_id TO node_id", [])?;
        conn.execute("COMMIT", [])?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            tracing::info!("Migrated to v6: projects→meshes, sessions→agent_nodes, project_id→mesh_id, session_id→node_id");
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
            migrate_mesh_config_columns(conn)?;
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
     COALESCE(default_provider, ''), COALESCE(base_ref, 'origin/main')";

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
    "id, mesh_id, name, path, branch, env, provider, status, cli_session_id, worktree_name, created_at, source_issue";

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
        use_worktree: true,
        source_issue: row.get(11)?,
        created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(10)?)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now()),
    })
}

fn get_agent_node_by_id_inner(conn: &Connection, id: i64) -> SqlResult<AgentNode> {
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
) -> SqlResult<AgentNode> {
    let db = get().lock().unwrap();
    db.execute(
        "INSERT INTO agent_nodes (mesh_id, name, path, branch, env, provider, status, worktree_name, source_issue)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'idle', ?7, ?8)",
        params![mesh_id, name, path, branch, env.to_string(), provider.to_string(), worktree_name, source_issue],
    )?;
    let id = db.last_insert_rowid();
    get_agent_node_by_id_inner(&db, id)
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
        &format!("SELECT {} FROM agent_nodes WHERE status != 'archived' ORDER BY created_at ASC", AGENT_NODE_COLUMNS)
    )?;
    let rows = stmt.query_map([], map_agent_node_row)?;
    rows.collect()
}

pub fn list_agent_nodes_by_mesh(mesh_id: i64) -> SqlResult<Vec<AgentNode>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM agent_nodes WHERE mesh_id = ?1 ORDER BY created_at ASC", AGENT_NODE_COLUMNS)
    )?;
    let rows = stmt.query_map(params![mesh_id], map_agent_node_row)?;
    rows.collect()
}

pub fn update_agent_node_status(id: i64, status: SessionStatus) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute("UPDATE agent_nodes SET status = ?1 WHERE id = ?2", params![status.to_db_str(), id])?;
    Ok(())
}

pub fn archive_agent_node(id: i64) -> SqlResult<()> {
    update_agent_node_status(id, SessionStatus::Archived)
}

pub fn delete_agent_node(id: i64) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute("DELETE FROM agent_nodes WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn restore_agent_node(id: i64) -> SqlResult<()> {
    update_agent_node_status(id, SessionStatus::Idle)
}

pub fn update_cli_session_id(id: i64, cli_id: &str) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute("UPDATE agent_nodes SET cli_session_id = ?1 WHERE id = ?2", params![cli_id, id])?;
    Ok(())
}

pub fn mark_running_nodes_suspended() -> SqlResult<usize> {
    let db = get().lock().unwrap();
    let count = db.execute(
        "UPDATE agent_nodes SET status = 'suspended' WHERE status IN ('running', 'awaiting_input')",
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

// --- Checkpoint operations ---

pub fn create_checkpoint(
    node_id: i64,
    git_ref: &str,
    turn_index: i32,
    message: Option<&str>,
) -> SqlResult<Checkpoint> {
    let db = get().lock().unwrap();
    db.execute(
        "INSERT INTO checkpoints (node_id, git_ref, turn_index, message) VALUES (?1, ?2, ?3, ?4)",
        params![node_id, git_ref, turn_index, message],
    )?;
    let id = db.last_insert_rowid();

    let mut stmt = db.prepare(
        "SELECT id, node_id, git_ref, turn_index, message, created_at
         FROM checkpoints WHERE id = ?1"
    )?;
    stmt.query_row(params![id], |row| {
        Ok(Checkpoint {
            id: row.get(0)?,
            node_id: row.get(1)?,
            git_ref: row.get(2)?,
            turn_index: row.get(3)?,
            message: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })
}

pub fn get_checkpoint_by_id(id: i64) -> SqlResult<Checkpoint> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, node_id, git_ref, turn_index, message, created_at
         FROM checkpoints WHERE id = ?1"
    )?;
    stmt.query_row(params![id], |row| {
        Ok(Checkpoint {
            id: row.get(0)?,
            node_id: row.get(1)?,
            git_ref: row.get(2)?,
            turn_index: row.get(3)?,
            message: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })
}

pub fn list_checkpoints(node_id: i64) -> SqlResult<Vec<Checkpoint>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, node_id, git_ref, turn_index, message, created_at
         FROM checkpoints WHERE node_id = ?1 ORDER BY turn_index ASC"
    )?;
    let rows = stmt.query_map(params![node_id], |row| {
        Ok(Checkpoint {
            id: row.get(0)?,
            node_id: row.get(1)?,
            git_ref: row.get(2)?,
            turn_index: row.get(3)?,
            message: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })?;
    rows.collect()
}
