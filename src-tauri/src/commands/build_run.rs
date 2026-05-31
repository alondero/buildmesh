//! Build/Run feature — spawns a shell in a worktree and runs build/run commands

use crate::db;
use crate::env;
use crate::models::MeshConfig;
use base64::Engine;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

// ---------------------------------------------------------------------------
// Worktree path resolution
// ---------------------------------------------------------------------------

/// Validate that the worktree directory exists on disk.
/// Returns an error if the node has no worktree_name or the directory hasn't been created yet.
fn validate_worktree_exists(resolved: &env::ResolvedPath, worktree_name: Option<&str>) -> Result<(), String> {
    let wt_name = worktree_name.ok_or_else(|| {
        "No worktree name set for this agent node. Spawn the agent first to create a worktree.".to_string()
    })?;

    // Use git2 to verify the worktree is registered in git metadata,
    // not just the directory exists on disk. This catches broken/corrupted worktrees.
    let repo = git2::Repository::open(&resolved.host_path)
        .or_else(|_| git2::Repository::discover(&resolved.host_path))
        .map_err(|e| format!("Failed to open repository: {}", e))?;

    let worktrees = repo.worktrees()
        .map_err(|e| format!("Failed to list worktrees: {}", e))?;

    // Check if our worktree name is in the list
    for i in 0..worktrees.len() {
        if let Some(name) = worktrees.get(i) {
            if name == wt_name {
                return Ok(());
            }
        }
    }

    // Also check if the path itself is a valid git worktree (it could be the main worktree)
    if std::path::Path::new(&resolved.host_path).join(".git").exists() {
        return Ok(());
    }

    Err(format!(
        "Worktree '{}' not found in git worktree list. Spawn the agent first to create the worktree.",
        wt_name
    ))
}

// ---------------------------------------------------------------------------
// Process management
// ---------------------------------------------------------------------------

/// A build/run process tracked separately from agents
struct BuildRunProcess {
    master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
}

/// Registry for build/run processes
struct BuildRunRegistry {
    processes: HashMap<i64, Arc<BuildRunProcess>>,
}

impl BuildRunRegistry {
    fn new() -> Self {
        Self {
            processes: HashMap::new(),
        }
    }

    fn insert(&mut self, node_id: i64, process: BuildRunProcess) {
        self.processes.insert(node_id, Arc::new(process));
    }

    fn remove(&mut self, node_id: &i64) -> Option<Arc<BuildRunProcess>> {
        self.processes.remove(node_id)
    }
}

static BUILD_RUN_REGISTRY: once_cell::sync::Lazy<Arc<Mutex<BuildRunRegistry>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(BuildRunRegistry::new())));

// ---------------------------------------------------------------------------
// Tauri command
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildRunMode {
    Build,
    Run,
}

#[tauri::command]
pub async fn build_run(
    node_id: i64,
    mode: BuildRunMode,
    app: AppHandle,
) -> Result<(), String> {
    // 1. Get agent node (node.path == mesh.path for all nodes)
    let node = db::get_agent_node_by_id(node_id)
        .map_err(|e| format!("failed to get agent node {}: {}", node_id, e))?;

    // 2. Read canonical mesh config from DB
    let mesh = db::get_mesh_by_path(&node.path)
        .map_err(|e| format!("failed to get mesh for path {}: {}", node.path, e))?;
    let config = MeshConfig::from(&mesh);

    // 3. If use_worktree is false, work directly in repo root
    let use_worktree = config.use_worktree;
    let spawn_worktree_name = if use_worktree {
        node.worktree_name.as_deref()
    } else {
        None
    };

    // 4. Resolve worktree path via centralized env module
    let resolved = env::resolve_agent_path(&node.path, spawn_worktree_name);

    // Only validate worktree exists if use_worktree is true
    if use_worktree {
        validate_worktree_exists(&resolved, spawn_worktree_name)?;

        // Sanitize .git file to ensure proper worktree isolation across environments
        if let Err(e) = env::sanitize_git_worktree(&resolved.host_path, resolved.env_type) {
            tracing::warn!("build_run: failed to sanitize worktree .git file: {}", e);
        }
    }

    // 5. Get the command to run
    let command = match mode {
        BuildRunMode::Build => config.build_command.as_deref()
            .ok_or_else(|| "build command not configured".to_string())?,
        BuildRunMode::Run => config.run_command.as_deref()
            .ok_or_else(|| "run command not configured".to_string())?,
    };

    // 5. Get shell working directory from resolved path
    let shell_cwd = &resolved.spawn_path;

    // 7. Spawn PTY with shell
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }).map_err(|e| format!("failed to open PTY: {}", e))?;

    let mut cmd = if resolved.env_type == crate::models::EnvType::Wsl {
        let mut c = CommandBuilder::new("wsl.exe");
        c.arg("-e");
        c.arg(command);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = CommandBuilder::new("sh");
        c.arg("-c");
        c.arg(command);
        c
    } else {
        let mut c = CommandBuilder::new("cmd.exe");
        c.arg("/c");
        c.arg(command);
        c
    };
    cmd.cwd(shell_cwd);
    crate::pty::strip_git_env_vars(&mut cmd);

    let child = pair.slave.spawn_command(cmd)
        .map_err(|e| format!("failed to spawn shell: {}", e))?;

    let reader = pair.master
        .try_clone_reader()
        .map_err(|e| format!("failed to clone PTY reader: {}", e))?;

    // 8. Store process in registry
    {
        let mut registry = BUILD_RUN_REGISTRY.lock().unwrap();
        registry.insert(node_id, BuildRunProcess {
            master: Arc::new(Mutex::new(pair.master)),
        });
    }

    // 9. Spawn reader thread to stream output to frontend
    let node_id_clone = node_id;
    let app_handle = app.clone();
    std::thread::spawn(move || {
        let mut r = reader;
        let mut buf = [0u8; 1024];
        loop {
            match r.read(&mut buf) {
                Ok(0) => {
                    // EOF — process exited
                    break;
                }
                Ok(n) => {
                    let data = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                    let _ = app_handle.emit(
                        &format!("build-run-output-{}", node_id_clone),
                        serde_json::json!({ "data": data }),
                    );
                }
                Err(e) => {
                    tracing::error!("build_run PTY read error: {}", e);
                    break;
                }
            }
        }
        // Emit end event
        let _ = app_handle.emit(&format!("build-run-output-{}", node_id_clone), &"[process exited]\r\n");
    });

    // 10. Drop child guard — the process continues in the reader thread
    drop(child);

    Ok(())
}

#[tauri::command]
pub async fn get_mesh_config(mesh_id: i64) -> Result<MeshConfig, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("failed to get mesh {}: {}", mesh_id, e))?;
    Ok(MeshConfig::from(&mesh))
}

/// Close a build/run terminal for a node
#[tauri::command]
pub async fn close_build_run(node_id: i64) -> Result<(), String> {
    let mut registry = BUILD_RUN_REGISTRY.lock().unwrap();
    if let Some(process) = registry.remove(&node_id) {
        let master = process.master.lock().unwrap();
        drop(master);
    }
    Ok(())
}

#[tauri::command]
pub async fn ensure_mesh_config(_mesh_id: i64) -> Result<String, String> {
    // Config is now in the DB from mesh creation time — nothing to create
    Ok(String::new())
}
