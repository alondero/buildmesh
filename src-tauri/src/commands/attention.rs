//! Attention notification system for agent sessions
//!
//! Agents signal that they need user input via a stop hook configured in
//! `.claude/settings.local.json`. This module receives those notifications and
//! relays them to the frontend as Tauri events.

use crate::db;
use crate::models::SessionStatus;
use std::sync::{Arc, Mutex};
use tauri::{command, AppHandle, Emitter};

/// In-memory set of sessions awaiting user input
static ATTENTION_PENDING: once_cell::sync::Lazy<Arc<Mutex<std::collections::HashSet<i64>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(std::collections::HashSet::new())));

/// Register that a session is awaiting user input.
/// Called by the agent via the notify-attention stop hook.
/// Emits an `attention-needed` event to the frontend.
#[command]
pub async fn register_attention_session(
    app: AppHandle,
    session_id: i64,
) -> Result<(), String> {
    {
        let mut pending = ATTENTION_PENDING.lock().unwrap();
        pending.insert(session_id);
    }

    // Update database status
    db::update_agent_node_status(session_id, SessionStatus::AwaitingInput)
        .map_err(|e| e.to_string())?;

    // Emit event to frontend
    app.emit("attention-needed", serde_json::json!({
        "session_id": session_id
    }))
    .map_err(|e| e.to_string())?;

    crate::node_namer::record_turn(session_id, app.clone());

    tracing::info!("Session {} awaiting user input", session_id);
    Ok(())
}

/// Clear the attention state for a session.
/// Called when the user clicks on an awaiting session to resume interaction.
#[command]
pub async fn clear_attention_session(
    app: AppHandle,
    session_id: i64,
) -> Result<(), String> {
    {
        let mut pending = ATTENTION_PENDING.lock().unwrap();
        pending.remove(&session_id);
    }

    // Update database status back to running
    db::update_agent_node_status(session_id, SessionStatus::Running)
        .map_err(|e| e.to_string())?;

    // Emit event to frontend
    app.emit("attention-cleared", serde_json::json!({
        "session_id": session_id
    }))
    .map_err(|e| e.to_string())?;

    tracing::info!("Session {} attention cleared", session_id);
    Ok(())
}

/// Check if a session is currently awaiting input
#[command]
pub async fn is_attention_pending(session_id: i64) -> bool {
    let pending = ATTENTION_PENDING.lock().unwrap();
    pending.contains(&session_id)
}
