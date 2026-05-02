//! Database module using rusqlite for local SQLite storage

#[cfg(test)]
mod migration_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod project_tests;

use rusqlite::{Connection, params};
pub use rusqlite::Result as SqlResult;
use once_cell::sync::OnceCell;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::models::*;

static DB: OnceCell<Mutex<Connection>> = OnceCell::new();

/// Current schema version
const SCHEMA_VERSION: i32 = 4;

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

    // Create schema (all tables + indexes, IF NOT EXISTS so they're idempotent)
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS projects (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            layout TEXT NOT NULL DEFAULT 'grid',
            position INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id INTEGER NOT NULL REFERENCES projects(id),
            name TEXT NOT NULL,
            path TEXT NOT NULL,
            branch TEXT NOT NULL DEFAULT 'main',
            env TEXT NOT NULL DEFAULT 'windows',
            provider TEXT NOT NULL DEFAULT 'anthropic',
            status TEXT NOT NULL DEFAULT 'idle',
            cli_session_id TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS checkpoints (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id INTEGER NOT NULL REFERENCES sessions(id),
            git_ref TEXT NOT NULL,
            turn_index INTEGER NOT NULL,
            message TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_id);
        "
    )?;

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

        // Check if projects table exists yet (fresh install vs. upgrade)
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
        }

        conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
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

/// Resets the DB global between test runs. This is only for use in tests.
#[cfg(test)]
pub(crate) fn reset_for_testing() {
    use std::sync::Mutex;
    // Replace the global DB with a fresh in-memory connection.
    // This is only safe in tests which run single-threaded with exclusive access.
    let fresh = Mutex::new(rusqlite::Connection::open_in_memory().unwrap());
    let _ = DB.set(fresh);
}

// --- Internal Helpers (no locking) ---

fn get_project_by_id_inner(conn: &Connection, id: i64) -> SqlResult<Project> {
    let mut stmt = conn.prepare("SELECT id, name, path, layout, position, created_at FROM projects WHERE id = ?1")?;
    stmt.query_row(params![id], |row| {
        Ok(Project {
            id: row.get(0)?,
            name: row.get(1)?,
            path: row.get(2)?,
            layout: row.get::<_, String>(3)?,
            position: row.get(4)?,
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })
}

fn get_session_by_id_inner(conn: &Connection, id: i64) -> SqlResult<Session> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, name, path, branch, env, provider, status, cli_session_id, created_at
         FROM sessions WHERE id = ?1"
    )?;
    stmt.query_row(params![id], |row| {
        Ok(Session {
            id: row.get(0)?,
            project_id: row.get(1)?,
            name: row.get(2)?,
            path: row.get(3)?,
            branch: row.get(4)?,
            env: match row.get::<_, String>(5)?.as_str() {
                "wsl" => EnvType::Wsl,
                _ => EnvType::Windows,
            },
            provider: match row.get::<_, String>(6)?.as_str() {
                "minimax" => Provider::Minimax,
                "gemini" => Provider::Gemini,
                "opencode" => Provider::OpenCode,
                _ => Provider::Anthropic,
            },
            status: SessionStatus::from_db_str(&row.get::<_, String>(7)?),
            cli_session_id: row.get(8)?,
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(9)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })
}

// --- Project operations ---

pub fn create_project(name: &str, path: &str) -> SqlResult<Project> {
    let db = get().lock().unwrap();

    // Check if project with this path already exists (idempotent upsert)
    let existing: Option<i64> = db.query_row(
        "SELECT id FROM projects WHERE path = ?1",
        params![path],
        |row| row.get(0),
    ).ok();

    if let Some(id) = existing {
        return get_project_by_id_inner(&db, id);
    }

    // Append at end of position list
    let next_position: i64 = db.query_row(
        "SELECT COALESCE(MAX(position), 0) + 1 FROM projects",
        [],
        |row| row.get(0),
    )?;

    db.execute(
        "INSERT INTO projects (name, path, layout, position) VALUES (?1, ?2, 'grid', ?3)",
        params![name, path, next_position],
    )?;
    let id = db.last_insert_rowid();
    get_project_by_id_inner(&db, id)
}

pub fn get_project_by_id(id: i64) -> SqlResult<Project> {
    let db = get().lock().unwrap();
    get_project_by_id_inner(&db, id)
}

pub fn update_project_layout(id: i64, layout: &str) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute(
        "UPDATE projects SET layout = ?1 WHERE id = ?2",
        params![layout, id],
    )?;
    Ok(())
}

pub fn update_project_positions_batch(updates: &[(i64, i64)]) -> SqlResult<()> {
    if updates.is_empty() { return Ok(()); }
    let db = get().lock().unwrap();
    for (id, pos) in updates {
        db.execute(
            "UPDATE projects SET position = ?1 WHERE id = ?2",
            params![pos, id],
        )?;
    }
    Ok(())
}

pub fn list_projects() -> SqlResult<Vec<Project>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare("SELECT id, name, path, layout, position, created_at FROM projects ORDER BY position ASC, name ASC")?;
    let rows = stmt.query_map([], |row| {
        Ok(Project {
            id: row.get(0)?,
            name: row.get(1)?,
            path: row.get(2)?,
            layout: row.get::<_, String>(3)?,
            position: row.get(4)?,
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })?;
    rows.collect()
}

pub fn delete_project(id: i64) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute("DELETE FROM sessions WHERE project_id = ?1", params![id])?;
    db.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
    Ok(())
}

// --- Session operations ---

pub fn create_session(
    project_id: i64,
    name: &str,
    path: &str,
    branch: &str,
    env: EnvType,
    provider: Provider,
) -> SqlResult<Session> {
    let db = get().lock().unwrap();
    db.execute(
        "INSERT INTO sessions (project_id, name, path, branch, env, provider, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'idle')",
        params![project_id, name, path, branch, env.to_string(), provider.to_string()],
    )?;
    let id = db.last_insert_rowid();
    get_session_by_id_inner(&db, id)
}

pub fn update_session_name(id: i64, name: &str) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute(
        "UPDATE sessions SET name = ?1 WHERE id = ?2",
        params![name, id],
    )?;
    Ok(())
}

pub fn get_session_by_id(id: i64) -> SqlResult<Session> {
    let db = get().lock().unwrap();
    get_session_by_id_inner(&db, id)
}

pub fn list_sessions() -> SqlResult<Vec<Session>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, project_id, name, path, branch, env, provider, status, cli_session_id, created_at
         FROM sessions WHERE status != 'archived' ORDER BY created_at DESC"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Session {
            id: row.get(0)?,
            project_id: row.get(1)?,
            name: row.get(2)?,
            path: row.get(3)?,
            branch: row.get(4)?,
            env: match row.get::<_, String>(5)?.as_str() {
                "wsl" => EnvType::Wsl,
                _ => EnvType::Windows,
            },
            provider: match row.get::<_, String>(6)?.as_str() {
                "minimax" => Provider::Minimax,
                "gemini" => Provider::Gemini,
                "opencode" => Provider::OpenCode,
                _ => Provider::Anthropic,
            },
            status: SessionStatus::from_db_str(&row.get::<_, String>(7)?),
            cli_session_id: row.get(8)?,
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(9)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })?;
    rows.collect()
}

pub fn list_sessions_by_project(project_id: i64) -> SqlResult<Vec<Session>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, project_id, name, path, branch, env, provider, status, cli_session_id, created_at
         FROM sessions WHERE project_id = ?1 ORDER BY created_at DESC"
    )?;
    let rows = stmt.query_map(params![project_id], |row| {
        Ok(Session {
            id: row.get(0)?,
            project_id: row.get(1)?,
            name: row.get(2)?,
            path: row.get(3)?,
            branch: row.get(4)?,
            env: match row.get::<_, String>(5)?.as_str() {
                "wsl" => EnvType::Wsl,
                _ => EnvType::Windows,
            },
            provider: match row.get::<_, String>(6)?.as_str() {
                "minimax" => Provider::Minimax,
                "gemini" => Provider::Gemini,
                "opencode" => Provider::OpenCode,
                _ => Provider::Anthropic,
            },
            status: SessionStatus::from_db_str(&row.get::<_, String>(7)?),
            cli_session_id: row.get(8)?,
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(9)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })?;
    rows.collect()
}

pub fn update_session_status(id: i64, status: SessionStatus) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute("UPDATE sessions SET status = ?1 WHERE id = ?2", params![status.to_db_str(), id])?;
    Ok(())
}

pub fn archive_session(id: i64) -> SqlResult<()> {
    update_session_status(id, SessionStatus::Archived)
}

pub fn delete_session(id: i64) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn restore_session(id: i64) -> SqlResult<()> {
    update_session_status(id, SessionStatus::Idle)
}

pub fn update_cli_session_id(id: i64, cli_id: &str) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute("UPDATE sessions SET cli_session_id = ?1 WHERE id = ?2", params![cli_id, id])?;
    Ok(())
}

pub fn mark_running_sessions_suspended() -> SqlResult<usize> {
    let db = get().lock().unwrap();
    let count = db.execute(
        "UPDATE sessions SET status = 'suspended' WHERE status IN ('running', 'awaiting_input')",
        [],
    )?;
    Ok(count)
}

pub fn list_suspended_sessions() -> SqlResult<Vec<Session>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, project_id, name, path, branch, env, provider, status, cli_session_id, created_at
         FROM sessions WHERE status = 'suspended' AND cli_session_id IS NOT NULL"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Session {
            id: row.get(0)?,
            project_id: row.get(1)?,
            name: row.get(2)?,
            path: row.get(3)?,
            branch: row.get(4)?,
            env: match row.get::<_, String>(5)?.as_str() {
                "wsl" => EnvType::Wsl,
                _ => EnvType::Windows,
            },
            provider: match row.get::<_, String>(6)?.as_str() {
                "minimax" => Provider::Minimax,
                "gemini" => Provider::Gemini,
                "opencode" => Provider::OpenCode,
                _ => Provider::Anthropic,
            },
            status: SessionStatus::from_db_str(&row.get::<_, String>(7)?),
            cli_session_id: row.get(8)?,
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(9)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })?;
    rows.collect()
}

// --- Checkpoint operations ---

pub fn create_checkpoint(
    session_id: i64,
    git_ref: &str,
    turn_index: i32,
    message: Option<&str>,
) -> SqlResult<Checkpoint> {
    let db = get().lock().unwrap();
    db.execute(
        "INSERT INTO checkpoints (session_id, git_ref, turn_index, message) VALUES (?1, ?2, ?3, ?4)",
        params![session_id, git_ref, turn_index, message],
    )?;
    let id = db.last_insert_rowid();
    
    let mut stmt = db.prepare(
        "SELECT id, session_id, git_ref, turn_index, message, created_at
         FROM checkpoints WHERE id = ?1"
    )?;
    stmt.query_row(params![id], |row| {
        Ok(Checkpoint {
            id: row.get(0)?,
            session_id: row.get(1)?,
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
        "SELECT id, session_id, git_ref, turn_index, message, created_at
         FROM checkpoints WHERE id = ?1"
    )?;
    stmt.query_row(params![id], |row| {
        Ok(Checkpoint {
            id: row.get(0)?,
            session_id: row.get(1)?,
            git_ref: row.get(2)?,
            turn_index: row.get(3)?,
            message: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })
}

pub fn list_checkpoints(session_id: i64) -> SqlResult<Vec<Checkpoint>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, session_id, git_ref, turn_index, message, created_at
         FROM checkpoints WHERE session_id = ?1 ORDER BY turn_index ASC"
    )?;
    let rows = stmt.query_map(params![session_id], |row| {
        Ok(Checkpoint {
            id: row.get(0)?,
            session_id: row.get(1)?,
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
