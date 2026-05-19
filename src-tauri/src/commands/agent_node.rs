//! Agent Node management commands

use crate::db;
use crate::models::AgentNode;
use crate::services;
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
    services::agent_node::create(mesh_id, &path, &branch, provider.as_deref())
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
    crate::session_naming::cleanup(session_id);
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
    services::agent_node::update_status(session_id, &status)
        .map_err(|e| e.to_string())
}

/// Delete an agent node permanently
#[command]
pub async fn delete_session(session_id: i64) -> Result<(), String> {
    services::agent_node::delete(session_id)
        .map_err(|e| e.to_string())
}

/// Set the CLI session ID on an agent node (used when importing external sessions)
#[command]
pub async fn set_cli_session_id(session_id: i64, cli_session_id: String) -> Result<(), String> {
    db::update_cli_session_id(session_id, &cli_session_id).map_err(|e| e.to_string())
}

/// Set the active session (emits event for frontend to handle)
#[command]
pub async fn set_active_session(session_id: i64, app: tauri::AppHandle) -> Result<(), String> {
    tracing::debug!("set_active_session called: session_id={}", session_id);
    app.emit("session-activated", serde_json::json!({ "session_id": session_id }))
        .map_err(|e: tauri::Error| e.to_string())
}
