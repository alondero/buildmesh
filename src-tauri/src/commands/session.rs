//! Session management commands

use crate::db;
use crate::env;
use crate::models::{EnvType, Provider, Session, SessionStatus};
use std::path::PathBuf;
use tauri::{command, Emitter};

/// Create a new session
#[command]
pub async fn create_session(
    project_id: i64,
    name: String,
    path: String,
    branch: String,
) -> Result<Session, String> {
    tracing::debug!("create_session called: project_id={}, name={}, path={}, branch={}", project_id, name, path, branch);
    let path_buf = PathBuf::from(&path);
    let env_internal = env::env_for_path(&path_buf);
    let env_type = EnvType::from(env_internal);
    let provider = Provider::Anthropic;
    db::create_session(project_id, &name, &path, &branch, env_type, provider)
        .map_err(|e| {
            tracing::error!("create_session failed: {}", e);
            e.to_string()
        })
}

/// List all sessions
#[command]
pub async fn list_sessions() -> Result<Vec<Session>, String> {
    db::list_sessions().map_err(|e| e.to_string())
}

/// List sessions for a specific project
#[command]
pub async fn list_sessions_by_project(project_id: i64) -> Result<Vec<Session>, String> {
    db::list_sessions_by_project(project_id).map_err(|e| e.to_string())
}

/// Get session by ID
#[command]
pub async fn get_session(session_id: i64) -> Result<Session, String> {
    db::get_session_by_id(session_id).map_err(|e| e.to_string())
}

/// Archive a session
#[command]
pub async fn archive_session(session_id: i64) -> Result<(), String> {
    db::archive_session(session_id).map_err(|e| e.to_string())
}

/// Restore an archived session
#[command]
pub async fn restore_session(session_id: i64) -> Result<(), String> {
    db::restore_session(session_id).map_err(|e| e.to_string())
}

/// Update session status
#[command]
pub async fn update_session_status(
    session_id: i64,
    status: String,
) -> Result<(), String> {
    let status = SessionStatus::from_db_str(&status);
    db::update_session_status(session_id, status).map_err(|e| e.to_string())
}

/// Delete a session permanently
#[command]
pub async fn delete_session(session_id: i64) -> Result<(), String> {
    db::delete_session(session_id).map_err(|e| e.to_string())
}

/// Set the active session (emits event for frontend to handle)
#[command]
pub async fn set_active_session(session_id: i64, app: tauri::AppHandle) -> Result<(), String> {
    tracing::debug!("set_active_session called: session_id={}", session_id);
    app.emit("session-activated", serde_json::json!({ "session_id": session_id }))
        .map_err(|e: tauri::Error| e.to_string())
}
