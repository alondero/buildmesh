//! Project management commands

use crate::db;
use crate::models::Project;
use tauri::command;
use tauri_plugin_dialog::DialogExt;

/// Add a project by opening a folder picker dialog
#[command]
pub async fn add_project(app: tauri::AppHandle) -> Result<Project, String> {
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
    tracing::debug!("project name: {}", name);

    db::create_project(&name, &path).map_err(|e| {
        tracing::error!("create_project failed: {}", e);
        e.to_string()
    })
}

/// Create a new project
#[command]
pub async fn create_project(name: String, path: String) -> Result<Project, String> {
    db::create_project(&name, &path).map_err(|e| e.to_string())
}

/// Create a project for testing without dialog (uses temp directory)
#[command]
pub async fn create_test_project(name: String) -> Result<Project, String> {
    let temp_dir = std::env::temp_dir();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let project_path = temp_dir.join(format!("buildmesh_test_{}_{}", name.replace(' ', "_"), timestamp));
    std::fs::create_dir_all(&project_path).map_err(|e| e.to_string())?;
    db::create_project(&name, &project_path.to_string_lossy()).map_err(|e| e.to_string())
}

/// List all projects
#[command]
pub async fn list_projects() -> Result<Vec<Project>, String> {
    db::list_projects().map_err(|e| e.to_string())
}

/// Delete a project and its sessions
#[command]
pub async fn delete_project(project_id: i64) -> Result<(), String> {
    db::delete_project(project_id).map_err(|e| e.to_string())
}

/// Update a project's layout preference
#[command]
pub async fn update_project_layout(project_id: i64, layout: String) -> Result<(), String> {
    if layout != "grid" && layout != "single" {
        return Err("layout must be 'grid' or 'single'".to_string());
    }
    db::update_project_layout(project_id, &layout).map_err(|e| e.to_string())
}
