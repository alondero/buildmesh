//! Tauri commands for Settings > Data & Diagnostics (issue #1537).
//!
//! Thin skins over [`crate::services::state_recovery`]. Every command that
//! touches the filesystem or the database runs through `run_blocking`, so the
//! tokio worker is never the thread doing a `VACUUM INTO`.
//!
//! The save and open dialogs are resolved **here, in Rust**, matching
//! `commands::mesh::pick_mesh_folder` — the app-data directory is not readable
//! from the frontend, and the capability file grants no filesystem access, so
//! there is nothing for a JS-side picker to do that this cannot.

use tauri::command;
use tauri_plugin_dialog::DialogExt;

use crate::services::state_recovery::{
    self, StateExportResult, StateIntegrityReport, StateRecoveryInfo, StateRestorePlan,
    StateSnapshot,
};

/// Everything the Data & Diagnostics pane needs for its first render, in one
/// round trip. One command rather than three so the pane cannot render a
/// half-populated header while a second request is in flight.
#[command]
pub fn get_state_recovery_info() -> Result<StateRecoveryInfo, String> {
    state_recovery::info()
}

#[command]
pub fn list_state_snapshots() -> Result<Vec<StateSnapshot>, String> {
    state_recovery::list_snapshots()
}

/// Take a snapshot on demand. Snapshots are full fidelity and count against
/// the same retention cap as the automatic ones.
#[command]
pub fn create_state_snapshot() -> Result<StateSnapshot, String> {
    state_recovery::create_snapshot()
}

/// `full = false` is the fast `quick_check`; `full = true` is the thorough
/// `integrity_check` for the explicit "Run full check" action.
#[command]
pub fn check_state_integrity(full: bool) -> Result<StateIntegrityReport, String> {
    state_recovery::check_integrity(full)
}

/// Export to a path the caller supplies (the mobile HTTP surface and tests
/// use this; the desktop dialog path is [`export_state`]).
#[command]
pub async fn export_state_to(path: String, redacted: bool) -> Result<StateExportResult, String> {
    crate::commands::run_blocking("export_state_to", move || {
        state_recovery::export_to(std::path::Path::new(&path), redacted)
    })
    .await
}

/// Export, asking the user where to put it.
///
/// The dialog is opened on the blocking thread rather than the UI thread
/// because `blocking_save_file` is a synchronous call into the OS file
/// association layer; that is the same reason `pick_mesh_folder` is offloaded.
#[command]
pub async fn export_state(app: tauri::AppHandle, redacted: bool) -> Result<Option<StateExportResult>, String> {
    crate::commands::run_blocking("export_state", move || {
        let Some(picked) = app
            .dialog()
            .file()
            .set_file_name(state_recovery::default_export_name())
            .add_filter("Buildmesh state", &["bmsnap"])
            .blocking_save_file()
        else {
            return Ok(None);
        };
        let path = picked
            .into_path()
            .map_err(|e| format!("That location cannot be used: {e}"))?;
        state_recovery::export_to(&path, redacted).map(Some)
    })
    .await
}

/// Report what a bundle contains *without* staging it, so the UI can warn
/// before the user commits. `Ok(None)` means the user closed the dialog.
#[command]
pub async fn inspect_state_bundle(app: tauri::AppHandle) -> Result<Option<StateRestorePlan>, String> {
    crate::commands::run_blocking("inspect_state_bundle", move || {
        let Some(picked) = app
            .dialog()
            .file()
            .add_filter("Buildmesh state", &["bmsnap"])
            .blocking_pick_file()
        else {
            return Ok(None);
        };
        let path = picked
            .into_path()
            .map_err(|e| format!("That file cannot be read: {e}"))?;
        state_recovery::inspect_bundle(&path).map(Some)
    })
    .await
}

/// Stage a restore for the next launch.
///
/// This does not apply anything. It verifies the bundle, snapshots the current
/// state for rollback, and writes the payload; `run_profile_startup` applies
/// it on the next launch, before any connection or worker exists.
#[command]
pub async fn stage_state_restore(app: tauri::AppHandle) -> Result<Option<StateRestorePlan>, String> {
    crate::commands::run_blocking("stage_state_restore", move || {
        let Some(picked) = app
            .dialog()
            .file()
            .add_filter("Buildmesh state", &["bmsnap"])
            .blocking_pick_file()
        else {
            return Ok(None);
        };
        let path = picked
            .into_path()
            .map_err(|e| format!("That file cannot be read: {e}"))?;
        state_recovery::stage_restore_from(&path).map(Some)
    })
    .await
}

/// Discard a staged restore without restarting.
#[command]
pub fn cancel_state_restore() -> Result<(), String> {
    state_recovery::cancel_pending_restore()
}

/// The path "Open data folder" hands to the file manager.
///
/// Returned as data rather than opened here so the existing
/// `commands::file_tree::open_in_file_manager` stays the single OS-open seam —
/// it already normalizes mixed separators, which this Windows path never has
/// but the shared code path assumes it might.
#[command]
pub fn get_state_data_folder() -> Result<String, String> {
    state_recovery::info().map(|info| info.app_data_dir)
}
