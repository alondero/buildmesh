//! Build/Run feature — spawns a shell in a worktree and runs build/run commands

use crate::db;
use crate::env;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

const MESH_CONFIG_FILENAME: &str = "mesh.toml";

// ---------------------------------------------------------------------------
// Config parsing
// ---------------------------------------------------------------------------

/// Represents the parsed mesh.toml build/run configuration
#[derive(Debug, Clone, serde::Serialize)]
pub struct BuildRunConfig {
    pub build_command: String,
    pub run_command: String,
}

/// Parse [build]command and [run]command from a mesh.toml file
fn parse_mesh_config(mesh_path: &std::path::Path) -> Result<BuildRunConfig, String> {
    let config_path = mesh_path.join(MESH_CONFIG_FILENAME);
    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("mesh.toml not found at {:?}: {}", config_path, e))?;

    let build_command = extract_toml_value(&content, "build", "command")
        .ok_or_else(|| "build command not configured in mesh.toml".to_string())?;
    let run_command = extract_toml_value(&content, "run", "command")
        .ok_or_else(|| "run command not configured in mesh.toml".to_string())?;

    Ok(BuildRunConfig {
        build_command,
        run_command,
    })
}

/// Extract a value from a TOML table: [section] key = "value"
fn extract_toml_value(content: &str, section: &str, key: &str) -> Option<String> {
    let section_pattern = format!("[{}]", section);
    let section_start = content.find(&section_pattern)?;

    // Find the next [section] or end of file
    let section_content = if let Some(next_section_idx) = content[section_start + section_pattern.len()..]
        .find('[')
    {
        &content[section_start..section_start + section_pattern.len() + next_section_idx]
    } else {
        &content[section_start..]
    };

    let key_pattern = format!("{} = ", key);
    let key_start = section_content.find(&key_pattern)?;
    let value_start = section_content[key_start + key_pattern.len()..].find('"')? + key_start + key_pattern.len() + 1;
    let value_end = section_content[value_start..].find('"')? + value_start;

    Some(section_content[value_start..value_end].to_string())
}

// ---------------------------------------------------------------------------
// Worktree path resolution
// ---------------------------------------------------------------------------

/// Resolve the worktree path for a given agent node ID.
/// Uses `git worktree list --porcelain` to find the worktree directory.
fn resolve_worktree_path(mesh_path: &std::path::Path, node_id: i64) -> Result<String, String> {
    let worktree_name = format!("agent-{}", node_id);

    // Run git worktree list in the mesh directory
    let output = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(mesh_path)
        .output()
        .map_err(|e| format!("failed to list worktrees: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse output: each worktree starts with "worktree <path>" followed by "HEAD <ref>"
    let mut current_worktree_path: Option<String> = None;
    for line in stdout.lines() {
        if line.starts_with("worktree ") {
            current_worktree_path = Some(line.strip_prefix("worktree ").unwrap().to_string());
        } else if line.starts_with("HEAD ") {
            // Check if this worktree contains our agent- name
            if let Some(ref wp) = current_worktree_path {
                if wp.contains(&worktree_name) {
                    return Ok(wp.clone());
                }
            }
        } else if line.starts_with("baregit") || line.starts_with("gitdir") {
            current_worktree_path = None;
        }
    }

    // Fallback: construct the expected path
    let fallback = mesh_path.join("worktrees").join(&worktree_name);
    if fallback.exists() {
        return Ok(fallback.to_string_lossy().to_string());
    }

    Err(format!(
        "Worktree not found for agent-{}. Spawn an agent first to create the worktree.",
        node_id
    ))
}

/// Get the shell working directory for a worktree path, accounting for environment
fn get_shell_cwd(worktree_path: &str, env: env::Environment) -> String {
    match env {
        env::Environment::Wsl => {
            // If the path is a Windows UNC path, convert to Linux path for WSL shell
            if worktree_path.starts_with("\\\\wsl$\\") {
                env::to_host_path(worktree_path)
            } else {
                worktree_path.to_string()
            }
        }
        env::Environment::Windows => {
            // For Windows, use the path as-is
            worktree_path.to_string()
        }
    }
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
    // 1. Get agent node to find mesh path
    let node = db::get_agent_node_by_id(node_id)
        .map_err(|e| format!("failed to get agent node {}: {}", node_id, e))?;
    let mesh = db::get_mesh_by_id(node.mesh_id)
        .map_err(|e| format!("failed to get mesh {}: {}", node.mesh_id, e))?;

    // 2. Parse mesh.toml
    let config = parse_mesh_config(&std::path::PathBuf::from(&mesh.path))?;

    // 3. Resolve worktree path
    let worktree_path = resolve_worktree_path(&std::path::PathBuf::from(&mesh.path), node_id)?;

    // 4. Get the command to run
    let command = match mode {
        BuildRunMode::Build => &config.build_command,
        BuildRunMode::Run => &config.run_command,
    };

    // 5. Detect environment for path formatting
    let env_type = env::env_for_path(&std::path::PathBuf::from(&worktree_path));

    // 6. Get shell working directory
    let shell_cwd = get_shell_cwd(&worktree_path, env_type);

    // 7. Spawn PTY with shell
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }).map_err(|e| format!("failed to open PTY: {}", e))?;

    let mut cmd = CommandBuilder::new("cmd.exe");
    if matches!(env_type, env::Environment::Wsl) {
        // On WSL, use wsl.exe to run the command
        cmd = CommandBuilder::new("wsl.exe");
        cmd.arg("-e".to_string());
    } else {
        cmd.cwd(&shell_cwd);
        cmd.arg("/c".to_string());
    }
    cmd.arg(command.to_string());

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
                    let data = String::from_utf8_lossy(&buf[..n]).to_string();
                    let _ = app_handle.emit(&format!("build-run-output-{}", node_id_clone), &data);
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
pub async fn get_mesh_config(mesh_id: i64) -> Result<BuildRunConfig, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("failed to get mesh {}: {}", mesh_id, e))?;
    parse_mesh_config(&std::path::PathBuf::from(&mesh.path))
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
pub async fn ensure_mesh_config(mesh_id: i64) -> Result<String, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("failed to get mesh {}: {}", mesh_id, e))?;

    let config_path = std::path::PathBuf::from(&mesh.path).join(MESH_CONFIG_FILENAME);

    let template = r#"# Buildmesh configuration
# Commands are executed in the agent's worktree directory.

[build]
# command = "npm run build"

[run]
# command = "npm run dev"
"#;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&config_path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(template.as_bytes())
                .map_err(|e| format!("failed to write mesh.toml: {}", e))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("failed to create mesh.toml: {}", e)),
    }

    Ok(config_path.to_string_lossy().to_string())
}
