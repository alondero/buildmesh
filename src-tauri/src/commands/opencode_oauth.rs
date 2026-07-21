//! Tauri commands for the Buildmesh-managed OpenCode Go OAuth dance (issue
//! #956). Thin IPC adapters over [`crate::services::opencode_oauth`] — the
//! heavy lifting (Device Flow polling, refresh-on-401, workspace enumeration)
//! lives in the service module; each `#[command]` here is one body line that
//! forwards to a single service method. Mirrors the
//! `commands::usage::get_provider_meters` layout where the IPC surface is the
//! public boundary and `services::usage` owns the logic.

use tauri::command;

/// Revokes the Buildmesh-managed OpenCode Console credential by deleting
/// the Windows Credential Manager entry under `opencode:console`. Idempotent
/// — the Settings "Sign out" affordance never errors on a no-op (mirrors
/// `windows_cred::delete`'s own idempotency on missing credentials and the
/// non-Windows no-op successor in `services::opencode_oauth::revoke`).
///
/// Returns `Ok(())` on success or a `String` describing a non-recoverable
/// Windows failure (rare — e.g. credential-store corruption). Network/HTTP
/// errors are NOT possible; this command does no I/O beyond the local
/// credential manager call.
#[command]
pub async fn revoke_opencode_console() -> Result<(), String> {
    crate::services::opencode_oauth::revoke()
}
