//! Scratch Pad — a mesh-scoped free-text note field, surfaced in the Probe
//! Panel as the 📝 "Scratch Pad" tab.
//!
//! The tab is intentionally a "type whatever you want" surface: no
//! structure, no markdown rendering, no length cap on the backend.
//! Frontend debounces saves (~500ms) to keep the IPC chatter bounded
//! while the user is mid-thought.
//!
//! The scratch pad is private to Buildmesh — never written to disk, never
//! visible to spawned agents. If a future feature needs agent-readable
//! notes, that should be a separate column / file, not a side-channel on
//! this one.

use tauri::command;

use crate::db;

/// Read the scratch pad for a mesh. Resolves to the empty string (not
/// an error) when the mesh id doesn't exist — the editor mounts a
/// blank surface in that case, and the unknown-id "no row" case is
/// indistinguishable from "row exists, empty notes" at the wire
/// boundary. The db helper stays honest about "no such row" so the
/// SQL layer can be reasoned about; this command translates that to
/// the wire contract.
#[command]
pub async fn get_mesh_scratchpad(mesh_id: i64) -> Result<String, String> {
    crate::commands::run_blocking("get_mesh_scratchpad", move || {
        match db::get_mesh_scratchpad(mesh_id) {
            Ok(content) => Ok(content),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(String::new()),
            Err(e) => Err(e.to_string()),
        }
    })
    .await
}

/// Overwrite a mesh's scratch pad. Empty string is a normal value
/// (cleared notes), not a deletion. Returns an error if the mesh id
/// doesn't exist — a debounced save that fires after the mesh was
/// deleted should NOT silently succeed and report "Saved" to the user,
/// since the data went nowhere.
#[command]
pub async fn set_mesh_scratchpad(mesh_id: i64, content: String) -> Result<(), String> {
    crate::commands::run_blocking("set_mesh_scratchpad", move || {
        db::set_mesh_scratchpad(mesh_id, &content).map_err(|e| e.to_string())
    })
    .await
}
