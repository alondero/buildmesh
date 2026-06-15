//! Coordinator read API control commands (ADR-0008).
//!
//! This slice (issue #315) needs no UI — the master switch and the read-scoped
//! token are driven entirely from these backend commands. A later slice adds
//! the Settings surface on top of the same commands.

use crate::db;
use tauri::command;
use ts_rs::TS;

/// Snapshot of the coordinator API's control state for a settings/status view.
/// `has_token` reports whether a read token has been minted without ever
/// leaking the token value.
///
/// Generated to src/types/generated/CoordinatorStatus.ts (issue #404).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, TS)]
#[ts(export, export_to = "CoordinatorStatus.ts")]
pub struct CoordinatorStatus {
    pub enabled: bool,
    pub has_token: bool,
}

/// Read the coordinator API's enable switch and whether a read token exists.
#[command]
pub async fn get_coordinator_status() -> Result<CoordinatorStatus, String> {
    let enabled = db::coordinator_api_enabled().map_err(|e| e.to_string())?;
    let has_token = db::coordinator_read_token()
        .map_err(|e| e.to_string())?
        .is_some();
    Ok(CoordinatorStatus { enabled, has_token })
}

/// Flip the master enable switch for the coordinator read API. Defaults off, so
/// the surface is closed until the user explicitly opts in.
#[command]
pub async fn set_coordinator_api_enabled(enabled: bool) -> Result<(), String> {
    db::set_coordinator_api_enabled(enabled).map_err(|e| e.to_string())
}

/// Mint (or replace) the read-scoped coordinator token and return it for the
/// user to copy. Replacing invalidates any previously issued token.
#[command]
pub async fn generate_coordinator_read_token() -> Result<String, String> {
    db::generate_coordinator_read_token().map_err(|e| e.to_string())
}
