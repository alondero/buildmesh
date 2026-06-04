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
    use_worktree: Option<bool>,
) -> Result<AgentNode, String> {
    services::agent_node::create(mesh_id, &path, &branch, provider.as_deref(), None, use_worktree)
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

/// Trim and validate a user-supplied rename. Returns the canonical (trimmed)
/// form on success, or an error message on rejection. Pulled out of the
/// `#[command]` body so it can be unit-tested without Tauri.
pub fn validate_rename_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("name cannot be empty".to_string());
    }
    if trimmed.chars().count() > 80 {
        return Err("name too long (max 80 chars)".to_string());
    }
    Ok(trimmed.to_string())
}

/// Manually rename an agent node, overriding the auto-LLM renamer.
///
/// The user's name is "sticky": `is_default_name` returns false for it, so
/// `should_trigger_rename` short-circuits on every subsequent turn. We also
/// tear down the in-memory rename state via `session_naming::cleanup` so
/// `SESSION_BUFFERS` doesn't keep growing for a node that no longer needs
/// a rename. Emits the same `session-renamed` event as the LLM path so the
/// frontend store and any other listeners stay in sync.
#[command]
pub async fn rename_session(
    session_id: i64,
    name: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let trimmed = validate_rename_name(&name)?;

    // Drop any in-flight rename state FIRST so the LLM's eventual commit
    // hits our race guard (which re-reads the node's name from the DB).
    crate::session_naming::cleanup(session_id);

    db::update_agent_node_name(session_id, &trimmed).map_err(|e| e.to_string())?;

    let _ = app.emit(
        "session-renamed",
        serde_json::json!({
            "session_id": session_id,
            "name": trimmed,
        }),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_rename_name;

    #[test]
    fn validate_accepts_trimmed_name() {
        assert_eq!(validate_rename_name("Fix OAuth callback").unwrap(), "Fix OAuth callback");
    }

    #[test]
    fn validate_strips_surrounding_whitespace() {
        assert_eq!(validate_rename_name("   spaced   ").unwrap(), "spaced");
    }

    #[test]
    fn validate_rejects_empty() {
        assert!(validate_rename_name("").is_err());
        assert!(validate_rename_name("    ").is_err());
        assert!(validate_rename_name("\t\n").is_err());
    }

    #[test]
    fn validate_rejects_over_80_chars() {
        let long = "x".repeat(81);
        let err = validate_rename_name(&long).unwrap_err();
        assert!(err.contains("too long"), "unexpected error: {}", err);
    }

    #[test]
    fn validate_accepts_exactly_80_chars() {
        let eighty = "x".repeat(80);
        assert_eq!(validate_rename_name(&eighty).unwrap().len(), 80);
    }
}
