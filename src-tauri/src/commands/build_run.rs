//! Build/Run feature — spawns a shell in a worktree and runs build/run commands

use crate::db;
use crate::env;
use crate::models::MeshRow;
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
    /// Writer for sending user input to the PTY. Always populated (even for
    /// one-shot build/run) so `write_to_build_run` can be called for any
    /// entry in the registry. Build/run never receives user input today,
    /// but storing the writer is harmless and keeps the surface uniform.
    writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
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

    /// Write bytes (user keystrokes) to the PTY master of a live process.
    /// Returns `Err("Build run not running")` if the node has no live process
    /// — matches the agent registry's error string shape so the frontend can
    /// safely ignore the "not running" case.
    pub fn write_bytes(&self, node_id: i64, data: &[u8]) -> Result<(), String> {
        let process = self
            .processes
            .get(&node_id)
            .ok_or_else(|| "Build run not running".to_string())?;
        let mut writer = process.writer.lock().unwrap();
        writer.write_all(data).map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())
    }

    /// Resize the PTY to `cols` x `rows`. Same "not running" semantics.
    pub fn resize_pty(&self, node_id: i64, cols: u16, rows: u16) -> Result<(), String> {
        let process = self
            .processes
            .get(&node_id)
            .ok_or_else(|| "Build run not running".to_string())?;
        let master = process.master.lock().unwrap();
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())
    }
}

static BUILD_RUN_REGISTRY: once_cell::sync::Lazy<Arc<Mutex<BuildRunRegistry>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(BuildRunRegistry::new())));

// ---------------------------------------------------------------------------
// Shell selection
// ---------------------------------------------------------------------------

/// Build the `CommandBuilder` for either a one-shot build/run command or an
/// interactive shell. Terminal mode drops the `-c` / `-e` / `/c` flag and
/// the command argument — we want a long-running shell, not a one-shot.
///
/// Shell choice per platform (terminal mode):
/// - Wsl → `wsl.exe` (default user's login shell, cwd is the WSL path)
/// - macOS → `sh`
/// - Windows → `powershell.exe` (per user preference; ANSI renders in PTY)
/// - Linux → `sh` (matches the build/run macOS branch; both are
///   `cfg!(target_os = "macos") == false` in practice on Windows builds,
///   but the value is correct on macOS/Linux dev builds too)
fn build_shell_command(
    mode: BuildRunMode,
    command: &str,
    env_type: crate::models::EnvType,
) -> CommandBuilder {
    if mode == BuildRunMode::Terminal {
        if env_type == crate::models::EnvType::Wsl {
            CommandBuilder::new("wsl.exe")
        } else if cfg!(target_os = "macos") {
            CommandBuilder::new("sh")
        } else {
            CommandBuilder::new("powershell.exe")
        }
    } else if env_type == crate::models::EnvType::Wsl {
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
    }
}

// ---------------------------------------------------------------------------
// Tauri command
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildRunMode {
    Build,
    Run,
    /// Interactive shell spawned in the worktree directory. The user types
    /// into it via `write_to_build_run`; output is streamed on the same
    /// `build-run-output-{node_id}` event channel as build/run.
    Terminal,
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

    // 2. Read canonical mesh row from DB
    let mesh = db::get_mesh_by_path(&node.path)
        .map_err(|e| format!("failed to get mesh for path {}: {}", node.path, e))?;
    let row = MeshRow::from(&mesh);

    // 3. If use_worktree is false, work directly in repo root
    let use_worktree = row.use_worktree;
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
        if let Err(e) = crate::git::worktree::sanitize_git_worktree(&resolved.host_path, resolved.env_type) {
            tracing::warn!("build_run: failed to sanitize worktree .git file: {}", e);
        }
    }

    // 5. Get the command to run. Terminal mode spawns an interactive shell
    //    directly, so no command string is needed.
    let command = match mode {
        BuildRunMode::Build => row.build_command.as_deref()
            .ok_or_else(|| "build command not configured".to_string())?,
        BuildRunMode::Run => row.run_command.as_deref()
            .ok_or_else(|| "run command not configured".to_string())?,
        BuildRunMode::Terminal => "",
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

    let mut cmd = build_shell_command(mode, command, resolved.env_type);
    cmd.cwd(shell_cwd);
    crate::pty::strip_git_env_vars(&mut cmd);

    let child = pair.slave.spawn_command(cmd)
        .map_err(|e| format!("failed to spawn shell: {}", e))?;

    let reader = pair.master
        .try_clone_reader()
        .map_err(|e| format!("failed to clone PTY reader: {}", e))?;

    // Take writer up-front so the registry can serve `write_to_build_run`
    // for terminal mode. `take_writer()` is a one-shot — must be called
    // before the master is moved into the registry.
    let writer = pair.master
        .take_writer()
        .map_err(|e| format!("failed to take PTY writer: {}", e))?;

    // 8. Store process in registry
    {
        let mut registry = BUILD_RUN_REGISTRY.lock().unwrap();
        registry.insert(node_id, BuildRunProcess {
            master: Arc::new(Mutex::new(pair.master)),
            writer: Arc::new(Mutex::new(writer)),
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
pub async fn get_mesh_row(mesh_id: i64) -> Result<MeshRow, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("failed to get mesh {}: {}", mesh_id, e))?;
    Ok(MeshRow::from(&mesh))
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

/// Forward user keystrokes to the live build/run PTY. Currently meaningful
/// only for `BuildRunMode::Terminal`; build/run ignores input.
/// Mirrors `agent::write_to_agent` (`src-tauri/src/commands/agent.rs:291`).
#[tauri::command]
pub fn write_to_build_run(node_id: i64, data: String) -> Result<(), String> {
    let registry = BUILD_RUN_REGISTRY.lock().unwrap();
    registry.write_bytes(node_id, data.as_bytes())
}

/// Resize the live build/run PTY to the given terminal grid size.
/// Mirrors `agent::resize_agent` (`src-tauri/src/commands/agent.rs:286`).
#[tauri::command]
pub fn resize_build_run(node_id: i64, rows: u16, cols: u16) -> Result<(), String> {
    let registry = BUILD_RUN_REGISTRY.lock().unwrap();
    registry.resize_pty(node_id, cols, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_run_mode_serializes_lowercase() {
        for (variant, expected) in [
            (BuildRunMode::Build, "\"build\""),
            (BuildRunMode::Run, "\"run\""),
            (BuildRunMode::Terminal, "\"terminal\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected, "serialize {:?}", variant);

            let round: BuildRunMode = serde_json::from_str(&json).unwrap();
            assert_eq!(round, variant, "round-trip {:?}", variant);
        }
    }

    #[test]
    fn build_run_registry_write_bytes_to_dead_session() {
        let registry = BuildRunRegistry::new();
        let result = registry.write_bytes(42, b"hello");
        assert!(result.is_err());
        // Frontend matches on this substring to swallow the "not running"
        // case silently — keep the contract stable.
        assert!(result.unwrap_err().contains("not running"));
    }

    #[test]
    fn build_run_registry_resize_pty_to_dead_session() {
        let registry = BuildRunRegistry::new();
        let result = registry.resize_pty(42, 80, 24);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not running"));
    }
}
