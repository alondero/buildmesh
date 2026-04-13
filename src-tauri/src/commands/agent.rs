//! Agent spawning and management via cc wrapper

use crate::db;
use crate::models::{EnvType, Provider};
use crate::models::WorkspaceStatus;
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use tauri::command;

struct ChildProcess {
    child: std::process::Child,
}

static AGENT_PROCESSES: once_cell::sync::Lazy<Arc<Mutex<HashMap<i64, ChildProcess>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Spawn a new agent using the cc wrapper
#[command]
pub async fn spawn_agent(
    workspace_id: i64,
    provider: String,
) -> Result<(), String> {
    let provider_flag = match provider.as_str() {
        "minimax" => "--minimax",
        _ => "--anthropic",
    };

    let workspace = db::get_workspace_by_id(workspace_id)
        .map_err(|e| e.to_string())?;
    let is_wsl = workspace.env == EnvType::Wsl;

    // Kill existing agent if running
    kill_agent(workspace_id).await.ok();

    let child = if is_wsl {
        Command::new("wsl.exe")
            .args(["--cd", &workspace.path, "--", "cc", provider_flag])
            .current_dir(&workspace.path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn wsl cc: {}", e))?
    } else {
        Command::new("cc")
            .arg(provider_flag)
            .current_dir(&workspace.path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn cc: {}", e))?
    };

    {
        let mut processes = AGENT_PROCESSES.lock().unwrap();
        processes.insert(workspace_id, ChildProcess { child });
    }

    db::update_workspace_status(workspace_id, WorkspaceStatus::Running)
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Kill the running agent for a workspace
#[command]
pub async fn kill_agent(workspace_id: i64) -> Result<(), String> {
    let mut processes = AGENT_PROCESSES.lock().unwrap();
    if let Some(mut cp) = processes.remove(&workspace_id) {
        cp.child.kill().ok();
    }
    db::update_workspace_status(workspace_id, WorkspaceStatus::Idle)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Check if agent is running for a workspace
#[command]
pub async fn is_agent_running(workspace_id: i64) -> bool {
    let processes = AGENT_PROCESSES.lock().unwrap();
    processes.contains_key(&workspace_id)
}
