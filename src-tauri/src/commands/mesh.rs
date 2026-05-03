//! Mesh management commands

use crate::db;
use crate::models::Mesh;
use tauri::command;
use tauri_plugin_dialog::DialogExt;

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
        path.rsplit('/')
            .next()
            .unwrap_or(&path)
            .to_string()
    };
    tracing::debug!("mesh name: {}", name);

    db::create_mesh(&name, &path).map_err(|e| {
        tracing::error!("create_mesh failed: {}", e);
        e.to_string()
    })
}

/// Create a new mesh
#[command]
pub async fn create_project(name: String, path: String) -> Result<Mesh, String> {
    db::create_mesh(&name, &path).map_err(|e| e.to_string())
}

/// Create a mesh for testing without dialog (uses temp directory)
#[command]
pub async fn create_test_project(name: String) -> Result<Mesh, String> {
    let temp_dir = std::env::temp_dir();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let mesh_path = temp_dir.join(format!("buildmesh_test_{}_{}", name.replace(' ', "_"), timestamp));
    std::fs::create_dir_all(&mesh_path).map_err(|e| e.to_string())?;
    db::create_mesh(&name, &mesh_path.to_string_lossy()).map_err(|e| e.to_string())
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
    if layout != "grid" && layout != "single" {
        return Err("layout must be 'grid' or 'single'".to_string());
    }
    db::update_mesh_layout(project_id, &layout).map_err(|e| e.to_string())
}

/// Update multiple meshes' sort positions in the sidebar
#[command]
pub async fn update_project_positions(updates: Vec<(i64, i64)>) -> Result<(), String> {
    db::update_mesh_positions_batch(&updates).map_err(|e| e.to_string())
}
