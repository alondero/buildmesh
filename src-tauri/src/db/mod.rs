//! Database module using rusqlite for local SQLite storage

use rusqlite::{Connection, params};
pub use rusqlite::Result as SqlResult;
use once_cell::sync::OnceCell;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::models::*;

static DB: OnceCell<Mutex<Connection>> = OnceCell::new();

/// Current schema version — increment when schema changes
const SCHEMA_VERSION: i32 = 2;

/// Initialize the database — creates tables if they don't exist
pub fn init(db_path: &PathBuf) -> SqlResult<()> {
    let conn = Connection::open(db_path)?;

    // Run migrations if needed (gated by schema version)
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

        CREATE TABLE IF NOT EXISTS chat_messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id INTEGER NOT NULL REFERENCES sessions(id),
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            tool_calls TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS session_scripts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id INTEGER NOT NULL REFERENCES sessions(id),
            script_type TEXT NOT NULL,
            content TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS app_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_id);
        CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);
        CREATE INDEX IF NOT EXISTS idx_checkpoints_session ON checkpoints(session_id);
        CREATE INDEX IF NOT EXISTS idx_chat_session ON chat_messages(session_id);
        "
    )?;

    DB.set(Mutex::new(conn)).map_err(|_| rusqlite::Error::InvalidParameterName("db already initialized".to_string()))?;
    Ok(())
}

/// Check schema version and run migrations if needed
fn migrate_if_needed(conn: &Connection) -> SqlResult<()> {
    let current_version: i32 = conn
        .query_row("SELECT value FROM app_settings WHERE key = 'schema_version'", [], |row| {
            row.get::<_, String>(0).map(|v| v.parse().unwrap_or(0))
        })
        .unwrap_or(0);

    if current_version >= SCHEMA_VERSION {
        return Ok(()); // Already up to date
    }

    tracing::info!("Migrating database from schema version {} to {}", current_version, SCHEMA_VERSION);

    if current_version < 1 {
        migrate_to_v1(conn)?;
    }
    if current_version < 2 {
        migrate_to_v2(conn)?;
    }

    // Mark migration complete
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    )?;

    tracing::info!("Database migration complete");
    Ok(())
}

/// v0 -> v1: Create sessions table (replaces workspaces)
fn migrate_to_v1(conn: &Connection) -> SqlResult<()> {
    // Check if sessions table already exists
    let sessions_exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='sessions'",
        [],
        |row| row.get(0),
    )?;

    if !sessions_exists {
        // Create sessions table fresh
        conn.execute_batch(
            "CREATE TABLE sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project_id INTEGER NOT NULL REFERENCES projects(id),
                name TEXT NOT NULL,
                path TEXT NOT NULL,
                branch TEXT NOT NULL DEFAULT 'main',
                env TEXT NOT NULL DEFAULT 'windows',
                provider TEXT NOT NULL DEFAULT 'anthropic',
                status TEXT NOT NULL DEFAULT 'idle',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX idx_sessions_project ON sessions(project_id);
            CREATE INDEX idx_sessions_status ON sessions(status);"
        )?;
    }
    Ok(())
}

/// v1 -> v2: Fix sessions.path UNIQUE constraint and migrate checkpoints/chat_messages workspace_id -> session_id
fn migrate_to_v2(conn: &Connection) -> SqlResult<()> {
    // Check if the sessions table has the problematic unique index on path
    let has_old_unique_index: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='sqlite_autoindex_sessions_1'",
        [],
        |row| row.get(0),
    )?;

    // Check if checkpoints/chat_messages still use workspace_id
    let has_workspace_id: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('checkpoints') WHERE name='workspace_id'",
        [],
        |row| row.get(0),
    )?;

    if !has_old_unique_index && !has_workspace_id {
        return Ok(()); // Already migrated
    }

    conn.execute_batch(
        "
        BEGIN TRANSACTION;

        -- Migrate sessions: recreate without UNIQUE on path
        CREATE TABLE sessions_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id INTEGER NOT NULL REFERENCES projects(id),
            name TEXT NOT NULL,
            path TEXT NOT NULL,
            branch TEXT NOT NULL DEFAULT 'main',
            env TEXT NOT NULL DEFAULT 'windows',
            provider TEXT NOT NULL DEFAULT 'anthropic',
            status TEXT NOT NULL DEFAULT 'idle',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        INSERT INTO sessions_new SELECT * FROM sessions;
        DROP TABLE sessions;
        ALTER TABLE sessions_new RENAME TO sessions;
        CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_id);
        CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);

        -- Migrate checkpoints: rename workspace_id -> session_id
        CREATE TABLE checkpoints_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id INTEGER NOT NULL REFERENCES sessions(id),
            git_ref TEXT NOT NULL,
            turn_index INTEGER NOT NULL,
            message TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        INSERT INTO checkpoints_new SELECT id, workspace_id, git_ref, turn_index, message, created_at FROM checkpoints;
        DROP TABLE checkpoints;
        ALTER TABLE checkpoints_new RENAME TO checkpoints;
        CREATE INDEX IF NOT EXISTS idx_checkpoints_session ON checkpoints(session_id);

        -- Migrate chat_messages: rename workspace_id -> session_id
        CREATE TABLE chat_messages_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id INTEGER NOT NULL REFERENCES sessions(id),
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            tool_calls TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        INSERT INTO chat_messages_new SELECT id, workspace_id, role, content, tool_calls, created_at FROM chat_messages;
        DROP TABLE chat_messages;
        ALTER TABLE chat_messages_new RENAME TO chat_messages;
        CREATE INDEX IF NOT EXISTS idx_chat_session ON chat_messages(session_id);

        COMMIT;
        "
    )?;

    Ok(())
}

/// Get a handle to the database
pub fn get() -> &'static Mutex<Connection> {
    DB.get().expect("database not initialized — call init() first")
}

// --- Project operations ---

pub fn create_project(name: &str, path: &str) -> SqlResult<Project> {
    let db = get().lock().unwrap();
    db.execute(
        "INSERT INTO projects (name, path) VALUES (?1, ?2)",
        params![name, path],
    )?;
    let id = db.last_insert_rowid();
    get_project_by_id(id)
}

pub fn get_project_by_id(id: i64) -> SqlResult<Project> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare("SELECT id, name, path, created_at FROM projects WHERE id = ?1")?;
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
    drop(db);
    get_session_by_id(id)
}

pub fn get_session_by_id(id: i64) -> SqlResult<Session> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, project_id, name, path, branch, env, provider, status, created_at
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
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(8)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })
}

pub fn list_sessions() -> SqlResult<Vec<Session>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, project_id, name, path, branch, env, provider, status, created_at
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
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(8)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })?;
    rows.collect()
}

pub fn list_sessions_by_project(project_id: i64) -> SqlResult<Vec<Session>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, project_id, name, path, branch, env, provider, status, created_at
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
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(8)?)
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
    drop(db);
    get_checkpoint_by_id(id)
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

// --- Chat message operations ---

pub fn add_chat_message(
    session_id: i64,
    role: &str,
    content: &str,
    tool_calls: Option<&str>,
) -> SqlResult<ChatMessage> {
    let db = get().lock().unwrap();
    db.execute(
        "INSERT INTO chat_messages (session_id, role, content, tool_calls) VALUES (?1, ?2, ?3, ?4)",
        params![session_id, role, content, tool_calls],
    )?;
    let id = db.last_insert_rowid();
    drop(db);
    get_chat_message_by_id(id)
}

pub fn get_chat_message_by_id(id: i64) -> SqlResult<ChatMessage> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, session_id, role, content, tool_calls, created_at
         FROM chat_messages WHERE id = ?1"
    )?;
    stmt.query_row(params![id], |row| {
        Ok(ChatMessage {
            id: row.get(0)?,
            session_id: row.get(1)?,
            role: row.get(2)?,
            content: row.get(3)?,
            tool_calls: row.get::<_, Option<String>>(4)?,
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })
}

pub fn list_chat_messages(session_id: i64) -> SqlResult<Vec<ChatMessage>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, session_id, role, content, tool_calls, created_at
         FROM chat_messages WHERE session_id = ?1 ORDER BY created_at ASC"
    )?;
    let rows = stmt.query_map(params![session_id], |row| {
        Ok(ChatMessage {
            id: row.get(0)?,
            session_id: row.get(1)?,
            role: row.get(2)?,
            content: row.get(3)?,
            tool_calls: row.get::<_, Option<String>>(4)?,
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })?;
    rows.collect()
}

// --- Settings operations ---

pub fn get_setting(key: &str) -> SqlResult<Option<String>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare("SELECT value FROM app_settings WHERE key = ?1")?;
    let result = stmt.query_row(params![key], |row| row.get(0));
    match result {
        Ok(val) => Ok(Some(val)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn set_setting(key: &str, value: &str) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        params![key, value],
    )?;
    Ok(())
}
