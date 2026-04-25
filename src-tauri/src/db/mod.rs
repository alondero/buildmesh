//! Database module using rusqlite for local SQLite storage

use rusqlite::{Connection, params};
pub use rusqlite::Result as SqlResult;
use once_cell::sync::OnceCell;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::models::*;

static DB: OnceCell<Mutex<Connection>> = OnceCell::new();

/// Initialize the database — creates tables if they don't exist
pub fn init(db_path: &PathBuf) -> SqlResult<()> {
    let conn = Connection::open(db_path)?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS projects (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS workspaces (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id INTEGER NOT NULL REFERENCES projects(id),
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            branch TEXT NOT NULL DEFAULT 'main',
            env TEXT NOT NULL DEFAULT 'windows',
            provider TEXT NOT NULL DEFAULT 'anthropic',
            status TEXT NOT NULL DEFAULT 'idle',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS checkpoints (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_id INTEGER NOT NULL REFERENCES workspaces(id),
            git_ref TEXT NOT NULL,
            turn_index INTEGER NOT NULL,
            message TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS chat_messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_id INTEGER NOT NULL REFERENCES workspaces(id),
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            tool_calls TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS workspace_scripts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_id INTEGER NOT NULL REFERENCES workspaces(id),
            script_type TEXT NOT NULL,
            content TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS app_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_workspaces_project ON workspaces(project_id);
        CREATE INDEX IF NOT EXISTS idx_workspaces_status ON workspaces(status);
        CREATE INDEX IF NOT EXISTS idx_checkpoints_workspace ON checkpoints(workspace_id);
        CREATE INDEX IF NOT EXISTS idx_chat_workspace ON chat_messages(workspace_id);
        "
    )?;

    DB.set(Mutex::new(conn)).map_err(|_| rusqlite::Error::InvalidParameterName("db already initialized".to_string()))?;
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
    db.execute("DELETE FROM workspaces WHERE project_id = ?1", params![id])?;
    db.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
    Ok(())
}

// --- Workspace operations ---

pub fn create_workspace(
    project_id: i64,
    name: &str,
    path: &str,
    branch: &str,
    env: EnvType,
    provider: Provider,
) -> SqlResult<Workspace> {
    let db = get().lock().unwrap();
    db.execute(
        "INSERT INTO workspaces (project_id, name, path, branch, env, provider, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'idle')",
        params![project_id, name, path, branch, env.to_string(), provider.to_string()],
    )?;
    let id = db.last_insert_rowid();
    drop(db);
    get_workspace_by_id(id)
}

pub fn get_workspace_by_id(id: i64) -> SqlResult<Workspace> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, project_id, name, path, branch, env, provider, status, created_at
         FROM workspaces WHERE id = ?1"
    )?;
    stmt.query_row(params![id], |row| {
        Ok(Workspace {
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
            status: WorkspaceStatus::from_db_str(&row.get::<_, String>(7)?),
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(8)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })
}

pub fn list_workspaces() -> SqlResult<Vec<Workspace>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, project_id, name, path, branch, env, provider, status, created_at
         FROM workspaces WHERE status != 'archived' ORDER BY created_at DESC"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Workspace {
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
            status: WorkspaceStatus::from_db_str(&row.get::<_, String>(7)?),
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(8)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })?;
    rows.collect()
}

pub fn list_workspaces_by_project(project_id: i64) -> SqlResult<Vec<Workspace>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, project_id, name, path, branch, env, provider, status, created_at
         FROM workspaces WHERE project_id = ?1 ORDER BY created_at DESC"
    )?;
    let rows = stmt.query_map(params![project_id], |row| {
        Ok(Workspace {
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
            status: WorkspaceStatus::from_db_str(&row.get::<_, String>(7)?),
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(8)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })?;
    rows.collect()
}

pub fn update_workspace_status(id: i64, status: WorkspaceStatus) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute("UPDATE workspaces SET status = ?1 WHERE id = ?2", params![status.to_db_str(), id])?;
    Ok(())
}

pub fn archive_workspace(id: i64) -> SqlResult<()> {
    update_workspace_status(id, WorkspaceStatus::Archived)
}

pub fn restore_workspace(id: i64) -> SqlResult<()> {
    update_workspace_status(id, WorkspaceStatus::Idle)
}

// --- Checkpoint operations ---

pub fn create_checkpoint(
    workspace_id: i64,
    git_ref: &str,
    turn_index: i32,
    message: Option<&str>,
) -> SqlResult<Checkpoint> {
    let db = get().lock().unwrap();
    db.execute(
        "INSERT INTO checkpoints (workspace_id, git_ref, turn_index, message) VALUES (?1, ?2, ?3, ?4)",
        params![workspace_id, git_ref, turn_index, message],
    )?;
    let id = db.last_insert_rowid();
    drop(db);
    get_checkpoint_by_id(id)
}

pub fn get_checkpoint_by_id(id: i64) -> SqlResult<Checkpoint> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, workspace_id, git_ref, turn_index, message, created_at
         FROM checkpoints WHERE id = ?1"
    )?;
    stmt.query_row(params![id], |row| {
        Ok(Checkpoint {
            id: row.get(0)?,
            workspace_id: row.get(1)?,
            git_ref: row.get(2)?,
            turn_index: row.get(3)?,
            message: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })
}

pub fn list_checkpoints(workspace_id: i64) -> SqlResult<Vec<Checkpoint>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, workspace_id, git_ref, turn_index, message, created_at
         FROM checkpoints WHERE workspace_id = ?1 ORDER BY turn_index ASC"
    )?;
    let rows = stmt.query_map(params![workspace_id], |row| {
        Ok(Checkpoint {
            id: row.get(0)?,
            workspace_id: row.get(1)?,
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
    workspace_id: i64,
    role: &str,
    content: &str,
    tool_calls: Option<&str>,
) -> SqlResult<ChatMessage> {
    let db = get().lock().unwrap();
    db.execute(
        "INSERT INTO chat_messages (workspace_id, role, content, tool_calls) VALUES (?1, ?2, ?3, ?4)",
        params![workspace_id, role, content, tool_calls],
    )?;
    let id = db.last_insert_rowid();
    drop(db);
    get_chat_message_by_id(id)
}

pub fn get_chat_message_by_id(id: i64) -> SqlResult<ChatMessage> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, workspace_id, role, content, tool_calls, created_at
         FROM chat_messages WHERE id = ?1"
    )?;
    stmt.query_row(params![id], |row| {
        Ok(ChatMessage {
            id: row.get(0)?,
            workspace_id: row.get(1)?,
            role: row.get(2)?,
            content: row.get(3)?,
            tool_calls: row.get::<_, Option<String>>(4)?,
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    })
}

pub fn list_chat_messages(workspace_id: i64) -> SqlResult<Vec<ChatMessage>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, workspace_id, role, content, tool_calls, created_at
         FROM chat_messages WHERE workspace_id = ?1 ORDER BY created_at ASC"
    )?;
    let rows = stmt.query_map(params![workspace_id], |row| {
        Ok(ChatMessage {
            id: row.get(0)?,
            workspace_id: row.get(1)?,
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
