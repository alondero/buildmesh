//! Checkpoint management via Git refs for Buildmesh

use crate::models::Checkpoint;
use crate::services;
use tauri::{command, Emitter};

/// Create a checkpoint — commits current state to a private git ref
#[command]
pub async fn create_checkpoint(
    session_id: i64,
    turn_index: i32,
    message: Option<String>,
    app: tauri::AppHandle,
) -> Result<Checkpoint, String> {
    let (checkpoint, node_path) =
        services::checkpoint::create(session_id, turn_index, message.as_deref())
            .map_err(|e| e.to_string())?;

    // Invalidate frontend git summary cache for this path
    let _ = app.emit("git-changed", serde_json::json!({ "path": &node_path }));

    Ok(checkpoint)
}

/// List checkpoints for a session
#[command]
pub async fn list_checkpoints(session_id: i64) -> Result<Vec<Checkpoint>, String> {
    crate::db::list_checkpoints(session_id).map_err(|e| e.to_string())
}

/// Revert session to a specific checkpoint
#[command]
pub async fn revert_to_checkpoint(checkpoint_id: i64, app: tauri::AppHandle) -> Result<(), String> {
    let node_path = services::checkpoint::revert(checkpoint_id)
        .map_err(|e| e.to_string())?;

    // Invalidate frontend git summary cache for this path
    let _ = app.emit("git-changed", serde_json::json!({ "path": &node_path }));

    Ok(())
}

/// Get the diff between two checkpoints
#[command]
pub async fn diff_checkpoints(
    checkpoint_a_id: i64,
    checkpoint_b_id: i64,
) -> Result<String, String> {
    services::checkpoint::diff(checkpoint_a_id, checkpoint_b_id)
        .map_err(|e| e.to_string())
}
