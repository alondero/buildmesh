//! Agent Node management commands

use crate::db;
use crate::models::AgentNode;
use crate::services;
use crate::services::agent_node::WorktreeCloseSafety;
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
    services::agent_node::create(mesh_id, &path, &branch, provider.as_deref(), None)
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

/// Delete an agent node permanently.
///
/// Returns as soon as the node is killed and removed from the database (Phase 1),
/// so the UI can drop it at once. The slow worktree-directory removal runs in a
/// background task that emits `worktree-cleanup-failed` if it can't finish — the
/// node is already gone either way (#243).
#[command]
pub async fn delete_session(
    session_id: i64,
    remove_worktree: Option<bool>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    services::agent_node::delete(session_id, remove_worktree.unwrap_or(false))
        .map_err(|e| e.to_string())?;

    drain_pending_removals(app);
    Ok(())
}

/// Spawn the background worktree-removal drain, emitting `worktree-cleanup-failed`
/// for any removal that couldn't complete. Shared by close and startup reconcile.
pub fn drain_pending_removals(app: tauri::AppHandle) {
    tauri::async_runtime::spawn_blocking(move || {
        for (removal, error) in services::agent_node::process_pending_removals() {
            let _ = app.emit(
                "worktree-cleanup-failed",
                serde_json::json!({
                    "node_name": removal.node_name,
                    "worktree_path": removal.worktree_path,
                    "error": error,
                }),
            );
        }
    });
}

/// Check whether the node's worktree can be removed safely on close.
#[command]
pub async fn get_worktree_close_safety(session_id: i64) -> Result<WorktreeCloseSafety, String> {
    services::agent_node::get_worktree_close_safety(session_id)
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
