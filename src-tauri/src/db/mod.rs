//! Database module using rusqlite for local SQLite storage

use rusqlite::{Connection, params};
pub use rusqlite::Result as SqlResult;
use once_cell::sync::OnceCell;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::models::*;

static DB: OnceCell<Mutex<Connection>> = OnceCell::new();

/// Current schema version
const SCHEMA_VERSION: i32 = 3;

/// Initialize the database
pub fn init(db_path: &PathBuf) -> SqlResult<()> {
    let conn = Connection::open(db_path)?;

    // Run migrations
    migrate_if_needed(&conn)?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS projects (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
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

        CREATE TABLE IF NOT EXISTS app_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
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
        tracing::info!("Resetting database for version {}", SCHEMA_VERSION);
        // Simplest migration for dev: drop and recreate
        conn.execute_batch("
            PRAGMA foreign_keys = OFF;
            DROP TABLE IF EXISTS sessions;
            DROP TABLE IF EXISTS projects;
            DROP TABLE IF EXISTS checkpoints;
            DROP TABLE IF EXISTS app_settings;
            PRAGMA foreign_keys = ON;
            CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        ")?;
        conn.execute(
            "INSERT INTO app_settings (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
    }
    Ok(())
}

pub fn get() -> &'static Mutex<Connection> {
    DB.get().expect("database not initialized")
}

// --- Internal Helpers (no locking) ---

fn get_project_by_id_inner(conn: &Connection, id: i64) -> SqlResult<Project> {
    let mut stmt = conn.prepare("SELECT id, name, path, created_at FROM projects WHERE id = ?1")?;
    stmt.query_row(params![id], |row| {
        Ok(Project {
            id: row.get(0)?,
            name: row.get(1)?,
            path: row.get(2)?,
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(3)?)
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
    db.execute("INSERT INTO projects (name, path) VALUES (?1, ?2)", params![name, path])?;
    let id = db.last_insert_rowid();
    get_project_by_id_inner(&db, id)
}

pub fn get_project_by_id(id: i64) -> SqlResult<Project> {
    let db = get().lock().unwrap();
    get_project_by_id_inner(&db, id)
}

pub fn list_projects() -> SqlResult<Vec<Project>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare("SELECT id, name, path, created_at FROM projects ORDER BY name")?;
    let rows = stmt.query_map([], |row| {
        Ok(Project {
            id: row.get(0)?,
            name: row.get(1)?,
            path: row.get(2)?,
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(3)?)
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

pub fn restore_session(id: i64) -> SqlResult<()> {
    update_session_status(id, SessionStatus::Idle)
}

pub fn update_cli_session_id(id: i64, cli_id: &str) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute("UPDATE sessions SET cli_session_id = ?1 WHERE id = ?2", params![cli_id, id])?;
    Ok(())
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
