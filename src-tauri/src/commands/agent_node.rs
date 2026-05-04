//! Agent Node management commands

use crate::db;
use crate::env;
use crate::models::{EnvType, Provider, AgentNode, SessionStatus};
use std::path::PathBuf;
use tauri::{command, Emitter};

/// Create a new agent node
#[command]
pub async fn create_session(
    mesh_id: i64,
    _name: String,
    path: String,
    branch: String,
    provider: Option<String>,
) -> Result<AgentNode, String> {
    let session_name = crate::naming::generate_random_name();
    tracing::debug!("create_session called: mesh_id={}, name={}, path={}, branch={}, provider={:?}", mesh_id, session_name, path, branch, provider);
    let path_buf = PathBuf::from(&path);
    let env_internal = env::env_for_path(&path_buf);
    let env_type = EnvType::from(env_internal);

    let provider_enum = match provider.as_deref() {
        Some("minimax") => Provider::Minimax,
        Some("gemini") => Provider::Gemini,
        Some("opencode") => Provider::OpenCode,
        _ => Provider::Anthropic,
    };

    db::create_agent_node(mesh_id, &session_name, &path, &branch, env_type, provider_enum, Some(&session_name))
        .map_err(|e| {
            tracing::error!("create_session failed: {}", e);
            e.to_string()
        })
}

/// List all agent nodes
#[command]
pub async fn list_sessions() -> Result<Vec<AgentNode>, String> {
    db::list_agent_nodes().map_err(|e| e.to_string())
}

/// List all agent nodes across all meshes (for remote access mobile app)
#[command]
pub async fn list_all_sessions() -> Result<Vec<AgentNode>, String> {
    db::list_agent_nodes().map_err(|e| e.to_string())
}

/// List agent nodes for a specific mesh
#[command]
pub async fn list_sessions_by_project(mesh_id: i64) -> Result<Vec<AgentNode>, String> {
    db::list_agent_nodes_by_mesh(mesh_id).map_err(|e| e.to_string())
}

/// Get agent node by ID
#[command]
pub async fn get_session(session_id: i64) -> Result<AgentNode, String> {
    db::get_agent_node_by_id(session_id).map_err(|e| e.to_string())
}

/// Archive an agent node
#[command]
pub async fn archive_session(session_id: i64) -> Result<(), String> {
    db::archive_agent_node(session_id).map_err(|e| e.to_string())
}

/// Restore an archived agent node
#[command]
pub async fn restore_session(session_id: i64) -> Result<(), String> {
    db::restore_agent_node(session_id).map_err(|e| e.to_string())
}

/// Update agent node status
#[command]
pub async fn update_session_status(
    session_id: i64,
    status: String,
) -> Result<(), String> {
    let status = SessionStatus::from_db_str(&status);
    db::update_agent_node_status(session_id, status).map_err(|e| e.to_string())
}

/// Delete an agent node permanently
#[command]
pub async fn delete_session(session_id: i64) -> Result<(), String> {
    crate::node_namer::cleanup(session_id);
    db::delete_agent_node(session_id).map_err(|e| e.to_string())
}

/// Set the active session (emits event for frontend to handle)
#[command]
pub async fn set_active_session(session_id: i64, app: tauri::AppHandle) -> Result<(), String> {
    tracing::debug!("set_active_session called: session_id={}", session_id);
    app.emit("session-activated", serde_json::json!({ "session_id": session_id }))
        .map_err(|e: tauri::Error| e.to_string())
}
