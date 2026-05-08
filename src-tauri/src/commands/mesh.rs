//! Mesh management commands

use crate::db;
use crate::models::Mesh;
use crate::services;
use tauri::command;
use tauri_plugin_dialog::DialogExt;

use super::agent::inject_attention_hook;

/// Add a mesh by opening a folder picker dialog
#[command]
pub async fn add_project(app: tauri::AppHandle) -> Result<Mesh, String> {
    tracing::debug!("add_project called");
    let folder_path = app.dialog()
        .file()
        .blocking_pick_folder();
    tracing::debug!("folder picker returned: {:?}", folder_path);
    let folder_path = folder_path.ok_or("No folder selected")?;

    let path = folder_path.to_string();
    tracing::debug!("selected path: {}", path);
    let name = if let tauri_plugin_dialog::FilePath::Path(p) = folder_path {
        std::path::Path::new(&p)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| {
                // fallback: split on either slash to get last segment
                p.to_string_lossy()
                    .rsplit(|c| c == '/' || c == '\\')
                    .next()
                    .unwrap_or(&p.to_string_lossy())
                    .to_string()
            })
    } else {
        // Url case — rsplit on '/' to get last path segment
        services::mesh::name_from_path(&path)
    };
    tracing::debug!("mesh name: {}", name);

    let mesh = db::create_mesh(&name, &path).map_err(|e| {
        tracing::error!("create_mesh failed: {}", e);
        e.to_string()
    })?;
    inject_attention_hook(std::path::Path::new(&path));
    Ok(mesh)
}

/// Create a new mesh
#[command]
pub async fn create_project(name: String, path: String) -> Result<Mesh, String> {
    let mesh = db::create_mesh(&name, &path).map_err(|e| e.to_string())?;
    inject_attention_hook(std::path::Path::new(&path));
    Ok(mesh)
}

/// Create a mesh for testing without dialog (uses temp directory)
#[command]
pub async fn create_test_project(name: String) -> Result<Mesh, String> {
    services::mesh::create_test(&name).map_err(|e| e.to_string())
}

/// List all meshes
#[command]
pub async fn list_projects() -> Result<Vec<Mesh>, String> {
    db::list_meshes().map_err(|e| e.to_string())
}

/// Delete a mesh and its nodes
#[command]
pub async fn delete_project(project_id: i64) -> Result<(), String> {
    db::delete_mesh(project_id).map_err(|e| e.to_string())
}

/// Update a mesh's layout preference
#[command]
pub async fn update_project_layout(project_id: i64, layout: String) -> Result<(), String> {
    services::mesh::update_layout(project_id, &layout).map_err(|e| e.to_string())
}

/// Update multiple meshes' sort positions in the sidebar
#[command]
pub async fn update_project_positions(updates: Vec<(i64, i64)>) -> Result<(), String> {
    db::update_mesh_positions_batch(&updates).map_err(|e| e.to_string())
}

/// Get or create the root remote access token for the whole buildmesh instance
#[command]
pub async fn get_root_token() -> Result<String, String> {
    db::get_or_create_root_token().map_err(|e| e.to_string())
}

/// Get the local machine's LAN IP address.
#[command]
pub async fn get_local_ip() -> Result<String, String> {
    match local_ip_address::local_ip() {
        Ok(ip) => {
            let ip_str = ip.to_string();
            // Skip Docker/NPIP/Tunnel interfaces
            if !ip_str.starts_with("172.16.")
               && !ip_str.starts_with("192.168.56.")
               && !ip_str.starts_with("10.0.0.")
               && ip_str != "0.0.0.0"
            {
                Ok(ip_str)
            } else {
                Err("no suitable LAN interface found".to_string())
            }
        }
        Err(e) => Err(format!("failed to get local IP: {}", e)),
    }
}
