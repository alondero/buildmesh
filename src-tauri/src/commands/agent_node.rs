//! Agent Node management commands

use crate::db;
use crate::models::AgentNode;
use crate::services;
use crate::git::worktree::WorktreeCloseSafety;
use tauri::{command, Emitter};

/// Create a new agent node
#[command]
pub async fn create_agent_node(
    mesh_id: i64,
    _name: String,
    path: String,
    branch: String,
    provider: Option<String>,
    use_worktree: Option<bool>,
) -> Result<AgentNode, String> {
    // Tauri command surface has no PR-spawn plumbing; PR flows go via
    // `commands::pr::create_pr_node`. If we ever expose PR spawn here,
    // this is the call site to grow.
    services::agent_node::create(
        mesh_id,
        &path,
        &branch,
        provider.as_deref(),
        None, // source_issue
        None, // source_pr — non-PR spawn (issue #450)
        None, // source_pr_pinned_sha — non-PR spawn (issue #444)
        use_worktree,
        None, // name_override — Tauri surface doesn't accept one
    )
        .map_err(|e| {
            tracing::error!("create_agent_node failed: {}", e);
            e.to_string()
        })
}

/// List all agent nodes
#[command]
pub async fn list_agent_nodes() -> Result<Vec<AgentNode>, String> {
    db::list_agent_nodes().map_err(|e| e.to_string())
}

/// Get agent node by ID
#[command]
pub async fn get_agent_node(node_id: i64) -> Result<AgentNode, String> {
    db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())
}

/// Delete an agent node permanently.
///
/// Returns as soon as the node is killed and removed from the database (Phase 1),
/// so the UI can drop it at once. The slow worktree-directory removal runs in a
/// background task that emits `worktree-cleanup-failed` if it can't finish — the
/// node is already gone either way (#243).
#[command]
pub async fn delete_agent_node(
    node_id: i64,
    remove_worktree: Option<bool>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    services::agent_node::delete(node_id, remove_worktree.unwrap_or(false))
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

/// Persist new grid positions for a batch of agent nodes (drag-to-reorder).
/// The frontend sends the full new ordering for the affected mesh so the DB
/// stays in sync with its optimistic update. Mirrors `update_mesh_positions`.
#[command]
pub async fn update_agent_node_positions(updates: Vec<(i64, i64)>) -> Result<(), String> {
    db::update_agent_node_positions_batch(&updates).map_err(|e| e.to_string())
}

/// Check whether the node's worktree can be removed safely on close.
#[command]
pub async fn get_worktree_close_safety(node_id: i64) -> Result<WorktreeCloseSafety, String> {
    services::agent_node::get_worktree_close_safety(node_id)
        .map_err(|e| e.to_string())
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
/// a rename. Emits the same `node-renamed` event as the LLM path so the
/// frontend store and any other listeners stay in sync.
#[command]
pub async fn rename_agent_node(
    node_id: i64,
    name: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let trimmed = validate_rename_name(&name)?;

    // Drop any in-flight rename state FIRST so the LLM's eventual commit
    // hits our race guard (which re-reads the node's name from the DB).
    crate::session_naming::cleanup(node_id);

    db::update_agent_node_name(node_id, &trimmed).map_err(|e| e.to_string())?;

    let _ = app.emit(
        "node-renamed",
        serde_json::json!({
            "node_id": node_id,
            "name": trimmed,
        }),
    );
    Ok(())
}

/// Swap an agent node's Model Provider (issue #774 / #775). The
/// worktree, branch, name, position, and all other state are preserved;
/// the new agent resumes from the existing `cli_session_id` when both
/// providers share the same Agent Harness and the new adapter supports
/// resume, else starts fresh. Cross-harness swaps are allowed (the user
/// may pick any Spawn Option) but always start fresh — the existing
/// `cli_session_id` is bound to the old harness's session format.
///
/// Frontend trigger: right-click context menu on `NodeItem` (see
/// ticket #776). UI flow: confirmation dialog for running nodes
/// (ticket #778). Returns the updated `AgentNode` on success so the
/// caller's store can patch the local entry without a refetch; on
/// failure the caller gets an `Err` (the `provider` column has
/// already been updated at that point — the user can retry, and the
/// local store stays on the old provider). The existing
/// `agent-spawned` event from `spawn_agent_inner` drives PTY /
/// resize sync on the frontend.
#[command]
pub async fn regenerate_agent_node(
    node_id: i64,
    new_provider_id: String,
    app: tauri::AppHandle,
) -> Result<crate::models::AgentNode, String> {
    services::agent_node::regenerate(node_id, &new_provider_id, &app)
        .await
        .map_err(|e| e.to_string())
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
