//! Workspace management commands

use crate::db;
use crate::env;
use crate::models::{EnvType, Provider, Project, Workspace, WorkspaceStatus};
use std::path::PathBuf;
use tauri::command;

/// Create a new workspace
#[command]
pub async fn create_workspace(
    project_id: i64,
    name: String,
    path: String,
    branch: String,
) -> Result<Workspace, String> {
    let path_buf = PathBuf::from(&path);
    let env_internal = env::env_for_path(&path_buf);
    let env_type = match env_internal {
        env::Environment::Wsl => EnvType::Wsl,
        env::Environment::Windows => EnvType::Windows,
    };
    let provider = Provider::Anthropic;
    db::create_workspace(project_id, &name, &path, &branch, env_type, provider)
        .map_err(|e| e.to_string())
}

/// List all workspaces
#[command]
pub async fn list_workspaces() -> Result<Vec<Workspace>, String> {
    db::list_workspaces().map_err(|e| e.to_string())
}

/// List workspaces for a specific project
#[command]
pub async fn list_workspaces_by_project(project_id: i64) -> Result<Vec<Workspace>, String> {
    db::list_workspaces_by_project(project_id).map_err(|e| e.to_string())
}

/// Get workspace by ID
#[command]
pub async fn get_workspace(workspace_id: i64) -> Result<Workspace, String> {
    db::get_workspace_by_id(workspace_id).map_err(|e| e.to_string())
}

/// Archive a workspace
#[command]
pub async fn archive_workspace(workspace_id: i64) -> Result<(), String> {
    db::archive_workspace(workspace_id).map_err(|e| e.to_string())
}

/// Restore an archived workspace
#[command]
pub async fn restore_workspace(workspace_id: i64) -> Result<(), String> {
    db::restore_workspace(workspace_id).map_err(|e| e.to_string())
}

/// Update workspace status
#[command]
pub async fn update_workspace_status(
    workspace_id: i64,
    status: String,
) -> Result<(), String> {
    let status = WorkspaceStatus::from_db_str(&status);
    db::update_workspace_status(workspace_id, status).map_err(|e| e.to_string())
}
