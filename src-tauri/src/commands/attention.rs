//! Attention notification system for agent sessions
//!
//! Agents signal that they need user input via a stop hook configured in
//! `.claude/settings.local.json`. This module receives those notifications and
//! relays them to the frontend as Tauri events.

use crate::db;
use crate::models::WorkspaceStatus;
use std::sync::{Arc, Mutex};
use tauri::{command, AppHandle, Emitter};

/// In-memory set of workspaces awaiting user input
static ATTENTION_PENDING: once_cell::sync::Lazy<Arc<Mutex<std::collections::HashSet<i64>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(std::collections::HashSet::new())));

/// Register that a workspace session is awaiting user input.
/// Called by the agent via the notify-attention stop hook.
/// Emits an `attention-needed` event to the frontend.
#[command]
pub async fn register_attention_session(
    app: AppHandle,
    workspace_id: i64,
) -> Result<(), String> {
    {
        let mut pending = ATTENTION_PENDING.lock().unwrap();
        pending.insert(workspace_id);
    }

    // Update database status
    db::update_workspace_status(workspace_id, WorkspaceStatus::AwaitingInput)
        .map_err(|e| e.to_string())?;

    // Emit event to frontend
    app.emit("attention-needed", serde_json::json!({
        "workspace_id": workspace_id
    }))
    .map_err(|e| e.to_string())?;

    tracing::info!("Session {} awaiting user input", workspace_id);
    Ok(())
}

/// Clear the attention state for a workspace session.
/// Called when the user clicks on an awaiting session to resume interaction.
#[command]
pub async fn clear_attention_session(
    app: AppHandle,
    workspace_id: i64,
) -> Result<(), String> {
    {
        let mut pending = ATTENTION_PENDING.lock().unwrap();
        pending.remove(&workspace_id);
    }

    // Update database status back to running
    db::update_workspace_status(workspace_id, WorkspaceStatus::Running)
        .map_err(|e| e.to_string())?;

    // Emit event to frontend
    app.emit("attention-cleared", serde_json::json!({
        "workspace_id": workspace_id
    }))
    .map_err(|e| e.to_string())?;

    tracing::info!("Session {} attention cleared", workspace_id);
    Ok(())
}

/// Check if a workspace session is currently awaiting input
#[command]
pub async fn is_attention_pending(workspace_id: i64) -> bool {
    let pending = ATTENTION_PENDING.lock().unwrap();
    pending.contains(&workspace_id)
}
