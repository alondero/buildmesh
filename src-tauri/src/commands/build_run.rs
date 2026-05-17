//! Build/Run feature — spawns a shell in a worktree and runs build/run commands

use crate::db;
use crate::env;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::HashMap;
use std::process::Command;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

pub const MESH_CONFIG_FILENAME: &str = "mesh.toml";

// ---------------------------------------------------------------------------
// Config parsing
// ---------------------------------------------------------------------------

/// Extract a value from a TOML table: [section] key = "value"
/// Also handles bare booleans: key = true or key = false
pub fn extract_toml_value(content: &str, section: &str, key: &str) -> Option<String> {
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

    // Get the rest of the line after "key = "
    let rest = &section_content[key_start + key_pattern.len()..];

    // Try quoted string first
    if let Some(quote_start) = rest.find('"') {
        let after_quote = &rest[quote_start + 1..];
        if let Some(quote_end) = after_quote.find('"') {
            return Some(after_quote[..quote_end].to_string());
        }
    }

    // Try bare boolean or identifier (true, false, or unquoted string)
    let trimmed = rest.trim_start();
    if trimmed.starts_with("true") || trimmed.starts_with("false") {
        let bool_str = if trimmed.starts_with("true") { "true" } else { "false" };
        return Some(bool_str.to_string());
    }

    // Try unquoted value until whitespace or end
    let end = trimmed.find(|c: char| c.is_ascii_whitespace()).unwrap_or(trimmed.len());
    if end > 0 {
        return Some(trimmed[..end].to_string());
    }

    None
}

/// Represents the parsed mesh.toml build/run configuration
#[derive(Debug, Clone, serde::Serialize)]
pub struct BuildRunConfig {
    pub build_command: String,
    pub run_command: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// If false, agents work directly in the repo root instead of in a worktree
    pub use_worktree: bool,
    /// "detached" (default) or "branched" - controls git worktree add behavior
    pub worktree_mode: Option<String>,
}

/// Parse [build]command, [run]command, and [agent]model/effort from a mesh.toml file
fn parse_mesh_config(mesh_path: &std::path::Path) -> Result<BuildRunConfig, String> {
    let config_path = mesh_path.join(MESH_CONFIG_FILENAME);
    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("mesh.toml not found at {:?}: {}", config_path, e))?;

    let build_command = extract_toml_value(&content, "build", "command")
        .ok_or_else(|| "build command not configured in mesh.toml".to_string())?;
    let run_command = extract_toml_value(&content, "run", "command")
        .ok_or_else(|| "run command not configured in mesh.toml".to_string())?;
    let model = extract_toml_value(&content, "agent", "model");
    let effort = extract_toml_value(&content, "agent", "effort");
    let use_worktree = extract_toml_value(&content, "agent", "use_worktree")
        .map(|v| v == "true")
        .unwrap_or(true);

    Ok(BuildRunConfig {
        build_command,
        run_command,
        model,
        effort,
        use_worktree,
        worktree_mode: extract_toml_value(&content, "agent", "worktree_mode"),
    })
}

/// Parse mesh config returning an Option (used by agent.rs for spawn-time reading).
/// Unlike parse_mesh_config, this does NOT fail if the file is missing or fields are absent.
pub fn parse_mesh_config_for_spawn(mesh_path: &std::path::Path) -> Option<BuildRunConfig> {
    let config_path = mesh_path.join(MESH_CONFIG_FILENAME);
    let content = std::fs::read_to_string(&config_path).ok()?;

    Some(BuildRunConfig {
        build_command: extract_toml_value(&content, "build", "command").unwrap_or_default(),
        run_command: extract_toml_value(&content, "run", "command").unwrap_or_default(),
        model: extract_toml_value(&content, "agent", "model"),
        effort: extract_toml_value(&content, "agent", "effort"),
        use_worktree: extract_toml_value(&content, "agent", "use_worktree")
            .map(|v| v == "true")
            .unwrap_or(true),
        worktree_mode: extract_toml_value(&content, "agent", "worktree_mode"),
    })
}

// ---------------------------------------------------------------------------
// Worktree path resolution
// ---------------------------------------------------------------------------

/// Validate that the worktree directory exists on disk.
/// Returns an error if the node has no worktree_name or the directory hasn't been created yet.
fn validate_worktree_exists(resolved: &env::ResolvedPath, worktree_name: Option<&str>) -> Result<(), String> {
    let wt_name = worktree_name.ok_or_else(|| {
        "No worktree name set for this agent node. Spawn the agent first to create a worktree.".to_string()
    })?;

    // Use git worktree list to verify the worktree is registered in git metadata,
    // not just the directory exists on disk. This catches broken/corrupted worktrees.
    let output = Command::new("git")
        .args(["-C", &resolved.host_path, "worktree", "list", "--porcelain"])
        .output()
        .map_err(|e| format!("Failed to list worktrees: {}", e))?;

    let list = String::from_utf8_lossy(&output.stdout);
    if list.contains(&resolved.host_path) {
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

    // 2. Parse mesh.toml
    let config = parse_mesh_config(&std::path::PathBuf::from(&node.path))?;

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
        BuildRunMode::Build => &config.build_command,
        BuildRunMode::Run => &config.run_command,
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
        c.arg(command.to_string());
        c
    } else if cfg!(target_os = "macos") {
        let mut c = CommandBuilder::new("sh");
        c.arg("-c");
        c.arg(command.to_string());
        c
    } else {
        let mut c = CommandBuilder::new("cmd.exe");
        c.arg("/c");
        c.arg(command.to_string());
        c
    };
    cmd.cwd(shell_cwd);

    // Ensure clean worktree isolation by removing any inherited Git environment variables
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    cmd.env_remove("GIT_INDEX_FILE");
    cmd.env_remove("GIT_OBJECT_DIRECTORY");
    cmd.env_remove("GIT_COMMON_DIR");

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

[agent]
# model = "claude-opus-4-7"
# effort = "medium"
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: BuildRunConfig should have use_worktree field
    #[test]
    fn build_run_config_has_use_worktree_field() {
        let config = BuildRunConfig {
            build_command: "npm run build".to_string(),
            run_command: "npm run dev".to_string(),
            model: None,
            effort: None,
            use_worktree: true,
            worktree_mode: None,
        };
        assert!(config.use_worktree);
    }

    /// Test: extract_toml_value can extract use_worktree boolean
    #[test]
    fn extract_toml_value_extracts_use_worktree_bool() {
        let content = r#"
[agent]
use_worktree = true
"#;
        let value = extract_toml_value(content, "agent", "use_worktree");
        assert_eq!(value, Some("true".to_string()));
    }

    /// Test: extract_toml_value returns None for missing key
    #[test]
    fn extract_toml_value_returns_none_for_missing_key() {
        let content = r#"
[build]
command = "npm run build"
"#;
        let value = extract_toml_value(content, "build", "use_worktree");
        assert_eq!(value, None);
    }

    /// Test: extract_toml_value handles section with multiple keys
    #[test]
    fn extract_toml_value_handles_multiple_keys() {
        let content = r#"
[agent]
model = "opus-4"
effort = "medium"
use_worktree = true
"#;
        assert_eq!(extract_toml_value(content, "agent", "model"), Some("opus-4".to_string()));
        assert_eq!(extract_toml_value(content, "agent", "effort"), Some("medium".to_string()));
        assert_eq!(extract_toml_value(content, "agent", "use_worktree"), Some("true".to_string()));
    }
}
