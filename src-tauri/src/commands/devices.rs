//! Authorized-devices control commands (issue #502).
//!
//! Desktop counterpart to the remote `/admin/devices` routes: the "Authorized
//! Devices" settings panel lists paired devices and revokes them through these
//! in-process Tauri commands. Both back onto the same `db` + `http::revocation`
//! layer, so a revoke from the desktop kicks a live mobile socket exactly as a
//! remote `POST /admin/devices/{id}/revoke` does.

use crate::db;
use crate::http::revocation;
use crate::models::DeviceSession;
use tauri::command;

/// List every paired device for the panel. The wire shape omits the token hash —
/// the secret never crosses the IPC boundary.
#[command]
pub async fn list_device_sessions() -> Result<Vec<DeviceSession>, String> {
    crate::commands::run_blocking("list_device_sessions", || {
        db::list_device_sessions().map_err(|e| e.to_string())
    })
    .await
}

/// Revoke a device: delete its row (blocking future requests) and broadcast the
/// revocation so any live WebSocket it holds closes immediately. Revoking an
/// already-gone id is a no-op success — the desired end state (device not
/// authorized) already holds.
#[command]
pub async fn revoke_device_session(id: i64) -> Result<(), String> {
    let removed = crate::commands::run_blocking("revoke_device_session", move || {
        db::revoke_device_session(id).map_err(|e| e.to_string())
    })
    .await?;
    if removed {
        revocation::revoke(id);
    }
    Ok(())
}
