//! Environment detection and path handling for Windows + WSL hybrid setup

mod mesh_config;

pub use mesh_config::{read_mesh_spawn_config, MeshSpawnConfig};

use std::path::PathBuf;
use once_cell::sync::Lazy;
use std::env;

use crate::process_util::command_no_window;

/// The default WSL distro name (e.g., "Ubuntu"), cached after first detection
static DETECTED_DISTRO: Lazy<Option<String>> = Lazy::new(get_default_wsl_distro_impl);

/// The Windows username, cached after first lookup
static WINDOWS_USERNAME: Lazy<Option<String>> = Lazy::new(get_windows_username_impl);

/// Get the default WSL distro name by parsing `wsl.exe -l -v` output.
/// Returns the distro marked as (default) or the first one if none marked.
fn get_default_wsl_distro_impl() -> Option<String> {
    let output = command_no_window("wsl.exe")
        .args(["-l", "-v"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if line.contains('(') && line.contains("Default") {
            // Line format: "  Ubuntu    Active          2"
            return line.split_whitespace().next().map(|s| s.to_string());
        }
    }
    // No default marked, use first distro
    stdout
        .lines()
        .skip(1) // skip header
        .filter(|l| !l.trim().is_empty())
        .next()
        .and_then(|l| l.split_whitespace().next())
        .map(|s| s.to_string())
}

/// Get the cached default WSL distro name
fn get_default_wsl_distro() -> Option<String> {
    DETECTED_DISTRO.clone()
}

/// Get the Windows username (used for path construction)
fn get_windows_username_impl() -> Option<String> {
    env::var("USERNAME").ok()
}

/// Get the cached Windows username
fn get_windows_username() -> Option<String> {
    WINDOWS_USERNAME.clone()
}

/// The detected runtime environment for this process
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    /// Running on native Windows (Git Bash/MSYS2)
    Windows,
    /// Running inside WSL (Windows Subsystem for Linux)
    Wsl,
}

impl Environment {
    /// Detect the current environment by checking for WSL signature
    pub fn detect() -> Self {
        if cfg!(target_os = "windows") {
            // On Windows, check if /proc/version contains "microsoft" (WSL signature)
            if let Ok(versions) = std::fs::read_to_string("/proc/version") {
                if versions.to_lowercase().contains("microsoft") {
                    return Environment::Wsl;
                }
            }
            // Check via wsl.exe detection
            if let Ok(output) = command_no_window("wsl.exe")
                .args(["--detect-nested"])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.trim() == "1" {
                    return Environment::Wsl;
                }
            }
            Environment::Windows
        } else {
            // Non-Windows (Linux/WSL)
            if let Ok(versions) = std::fs::read_to_string("/proc/version") {
                if versions.to_lowercase().contains("microsoft") {
                    Environment::Wsl
                } else {
                    Environment::Windows // treat native Linux as "Windows" for our purposes
                }
            } else {
                Environment::Windows
            }
        }
    }

    /// Returns true if we're running inside WSL
    pub fn is_wsl(&self) -> bool {
        matches!(self, Environment::Wsl)
    }
}

static CURRENT_ENV: Lazy<Environment> = Lazy::new(Environment::detect);

/// Get the current environment (cached)
pub fn current_env() -> Environment {
    *CURRENT_ENV
}

/// Convert a session path to the correct form for spawning commands
/// WSL paths are stored as Unix paths internally, Windows paths as Windows paths
pub fn to_spawn_path(path: &PathBuf) -> PathBuf {
    match current_env() {
        Environment::Wsl => {
            // If path looks like /mnt/c/... convert to WSL form
            if path.to_string_lossy().starts_with("/mnt/") {
                path.clone()
            } else if path.to_string_lossy().starts_with("C:\\") || path.to_string_lossy().starts_with("c:\\") {
                // Convert C:\... to /mnt/c/...
                let path_str = path.to_string_lossy().to_lowercase();
                let drive = path_str.chars().next().unwrap_or('c');
                let rest = &path_str[2..].replace('\\', "/");
                PathBuf::from(format!("/mnt/{}{}", drive, rest))
            } else {
                path.clone()
            }
        }
        Environment::Windows => {
            // Keep Windows paths as-is
            path.clone()
        }
    }
}

/// Get the path to the cc wrapper script
pub fn cc_path() -> PathBuf {
    match current_env() {
        Environment::Wsl => PathBuf::from("/mnt/c/Users/alond/.local/bin/cc"),
        Environment::Windows => {
            // Try to find cc in PATH
            if let Ok(output) = command_no_window("where")
                .arg("cc")
                .output()
            {
                let path_str = String::from_utf8_lossy(&output.stdout);
                let path = path_str.trim().lines().next().unwrap_or("");
                if !path.is_empty() {
                    return PathBuf::from(path);
                }
            }
            PathBuf::from("C:/Users/alond/.local/bin/cc")
        }
    }
}

/// Get the Git binary path for the correct environment
pub fn git_path() -> PathBuf {
    match current_env() {
        Environment::Wsl => PathBuf::from("git"),
        Environment::Windows => {
            if let Ok(output) = command_no_window("where")
                .arg("git")
                .output()
            {
                let path_str = String::from_utf8_lossy(&output.stdout);
                let path = path_str.trim().lines().next().unwrap_or("");
                if !path.is_empty() {
                    return PathBuf::from(path);
                }
            }
            PathBuf::from("git")
        }
    }
}

/// Determine the environment for a given session path
pub fn env_for_path(path: &PathBuf) -> Environment {
    if cfg!(target_os = "macos") {
        return Environment::Windows;
    }

    let path_str = path.to_string_lossy().to_lowercase();

    // WSL detection: paths starting with /mnt/, /home/, or \\wsl$
    if path_str.starts_with("/mnt/")
        || path_str.starts_with("/home/")
        || path_str.starts_with("\\\\wsl$")
    {
        Environment::Wsl
    } else {
        Environment::Windows
    }
}

/// Convert a path from session internal form to host-readable form
/// (e.g., /home/user -> \\wsl$\Ubuntu\home\user, /mnt/c/Users -> C:\Users, /c/Users -> C:\Users)
pub fn to_host_path(path: &str) -> String {
    if path.starts_with('/') {
        if path.starts_with("/mnt/") {
            // /mnt/c/Users -> C:\Users
            let drive = path.chars().nth(5).unwrap_or('c').to_uppercase().next().unwrap();
            format!("{}:{}", drive, path[6..].replace('/', "\\"))
        } else if path.starts_with("/home/") {
            // /home/user -> \\wsl$\Ubuntu\home\user
            let distro = get_default_wsl_distro().unwrap_or_else(|| "Ubuntu".to_string());
            format!("\\\\wsl$\\{}{}", distro, path.replace('/', "\\"))
        } else if path.len() >= 2 && path.chars().nth(1).unwrap().is_alphabetic() && (path.len() == 2 || path.chars().nth(2) == Some('/')) {
            // Handle Git Bash style /c/Users/ or /c
            let drive = path.chars().nth(1).unwrap().to_uppercase().next().unwrap();
            let rest = if path.len() > 2 { &path[2..] } else { "" };
            format!("{}:{}", drive, rest.replace('/', "\\"))
        } else {
            // Other Unix-style absolute path on Windows (e.g. /Users/...)
            // Return as-is, caller will handle if needed.
            path.to_string()
        }
    } else {
        path.to_string()
    }
}

// ---------------------------------------------------------------------------
// ResolvedPath — high-level path resolution for agent operations
// ---------------------------------------------------------------------------

use crate::models::EnvType;

/// A fully-resolved set of paths for an agent node, ready for use by callers
/// without needing to compose env detection + host conversion + worktree logic.
#[derive(Debug, Clone)]
pub struct ResolvedPath {
    /// Host-accessible path for file system operations (e.g. Windows UNC path for WSL,
    /// or native path on macOS/Windows).
    pub host_path: String,
    /// Path to use as CWD when spawning agent/shell processes.
    pub spawn_path: String,
    /// The detected environment type for this path.
    pub env_type: EnvType,
}

/// Resolve the working directory for an agent node, accounting for worktree
/// layout and environment differences.
///
/// - `base_path`: The agent node's stored `path` field (project root).
/// - `worktree_name`: If set, the worktree subdirectory name under
///   `{base_path}/.claude/worktrees/{name}`.
///
/// Returns a `ResolvedPath` with host, spawn, and env fields populated.
pub fn resolve_agent_path(base_path: &str, worktree_name: Option<&str>) -> ResolvedPath {
    // Compute the effective path (with worktree if applicable)
    let effective_path = match worktree_name {
        Some(wt_name) if !wt_name.is_empty() => {
            format!("{}/.claude/worktrees/{}", base_path, wt_name)
        }
        _ => base_path.to_string(),
    };

    // Detect environment from the effective path
    let path_buf = PathBuf::from(&effective_path);
    let env_internal = env_for_path(&path_buf);
    let env_type = EnvType::from(env_internal);

    // On macOS, paths are always native — no WSL conversion needed.
    // On Windows, convert based on detected environment.
    let (host_path, spawn_path) = if cfg!(target_os = "macos") {
        (effective_path.clone(), effective_path)
    } else {
        let host = to_host_path(&effective_path);
        let spawn = to_spawn_path(&path_buf).to_string_lossy().to_string();
        (host, spawn)
    };

    ResolvedPath {
        host_path,
        spawn_path,
        env_type,
    }
}

/// Sanitize the .git file in a worktree to ensure it uses the correct path format
/// for the target environment (Windows vs WSL).
pub fn sanitize_git_worktree(worktree_host_path: &str, env_type: EnvType) -> Result<(), String> {
    let git_file_path = std::path::Path::new(worktree_host_path).join(".git");
    
    if !git_file_path.is_file() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&git_file_path)
        .map_err(|e| format!("Failed to read .git file: {}", e))?;

    if !content.starts_with("gitdir: ") {
        return Ok(());
    }

    let git_dir_path = content.trim_start_matches("gitdir: ").trim();
    if git_dir_path.is_empty() {
        return Err("invalid .git file: empty gitdir path".to_string());
    }

    // Convert the path to the target environment's format
    let new_path = match env_type {
        EnvType::Wsl => {
            // Ensure it's a WSL-friendly path
            if git_dir_path.contains(':') || git_dir_path.starts_with("\\\\") {
                // Convert Windows path to WSL (/mnt/c/...)
                let path_str = git_dir_path.replace('\\', "/");
                if let Some(pos) = path_str.find(':') {
                    let drive = path_str[..pos].to_lowercase();
                    format!("/mnt/{}{}", drive, &path_str[pos + 1..])
                } else {
                    path_str
                }
            } else {
                git_dir_path.to_string()
            }
        }
        EnvType::Windows => {
            // Target is Windows. Use to_host_path which now handles /mnt/, /home/, and /c/ styles.
            to_host_path(git_dir_path)
        }
    };

    if new_path != git_dir_path {
        tracing::info!("Sanitizing .git file: {} -> {}", git_dir_path, new_path);
        // Ensure we use Unix line endings for the .git file as Git expects
        std::fs::write(&git_file_path, format!("gitdir: {}\n", new_path))
            .map_err(|e| format!("Failed to write sanitized .git file: {}", e))?;
    }

    Ok(())
}

/// Add a worktree to the repo — branched (new branch from HEAD) or detached.
fn add_worktree_impl(
    repo: &git2::Repository,
    branch_name: &str,
    host_path: &std::path::Path,
    use_branched: bool,
) -> Result<(), String> {
    if use_branched {
        let head_commit = repo.head()
            .and_then(|h| h.peel_to_commit())
            .map_err(|e| format!("Failed to resolve HEAD commit: {}", e))?;
        let branch = repo.branch(branch_name, &head_commit, false)
            .map_err(|e| format!("Failed to create branch '{}': {}", branch_name, e))?;
        let reference = branch.into_reference();
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(&reference));
        repo.worktree(branch_name, host_path, Some(&opts))
            .map_err(|e| format!("Git worktree add failed: {}", e))?;
    } else {
        // git2 worktree add without a reference auto-creates a branch; detach HEAD after.
        repo.worktree(branch_name, host_path, None)
            .map_err(|e| format!("Git worktree add failed: {}", e))?;

        let wt_repo = git2::Repository::open(host_path)
            .map_err(|e| format!("Failed to open worktree repo: {}", e))?;
        let head_oid = wt_repo.head()
            .and_then(|h| h.peel_to_commit())
            .map_err(|e| format!("Failed to resolve worktree HEAD: {}", e))?
            .id();
        wt_repo.set_head_detached(head_oid)
            .map_err(|e| format!("Failed to detach HEAD in worktree: {}", e))?;

        if let Ok(mut branch) = repo.find_branch(branch_name, git2::BranchType::Local) {
            let _ = branch.delete();
        }
    }
    Ok(())
}

/// Remove worktrees whose on-disk path no longer exists.
fn prune_stale_worktrees(repo: &git2::Repository) {
    if let Ok(worktree_names) = repo.worktrees() {
        for i in 0..worktree_names.len() {
            if let Some(name) = worktree_names.get(i) {
                if let Ok(wt) = repo.find_worktree(name) {
                    if wt.validate().is_err() {
                        let mut prune_opts = git2::WorktreePruneOptions::new();
                        prune_opts.valid(false);
                        let _ = wt.prune(Some(&mut prune_opts));
                    }
                }
            }
        }
    }
}

/// Create a new Git worktree at the specified path and honor .worktreeinclude.
pub fn create_git_worktree(
    project_root: &str,
    worktree_host_path: &str,
    branch_name: &str,
    worktree_mode: &str,
) -> Result<(), String> {
    let host_path = std::path::Path::new(worktree_host_path);

    if host_path.exists() {
        return Ok(());
    }

    // Ensure parent directory exists
    if let Some(parent) = host_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create worktrees directory: {}", e))?;
    }

    tracing::info!("Creating git worktree: {} at {} (mode: {})", branch_name, worktree_host_path, worktree_mode);

    let repo = git2::Repository::open(project_root)
        .map_err(|e| format!("Failed to open repository at {}: {}", project_root, e))?;

    let use_branched = worktree_mode == "branched";

    match add_worktree_impl(&repo, branch_name, host_path, use_branched) {
        Ok(()) => {}
        Err(err) if err.contains("already exists") || err.contains("already checked out") => {
            tracing::warn!("Worktree already exists in git metadata, attempting to repair...");
            prune_stale_worktrees(&repo);
            add_worktree_impl(&repo, branch_name, host_path, use_branched)?;
        }
        Err(err) => return Err(err),
    }

    // Honor .worktreeinclude if it exists in the project root
    let include_file_path = std::path::Path::new(project_root).join(".worktreeinclude");
    if include_file_path.exists() {
        tracing::info!("Found .worktreeinclude, copying included files...");
        if let Ok(content) = std::fs::read_to_string(&include_file_path) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }

                let src = std::path::Path::new(project_root).join(trimmed);
                let dest = host_path.join(trimmed);

                if src.exists() {
                    if let Some(parent) = dest.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }

                    if src.is_file() {
                        if let Err(e) = std::fs::copy(&src, &dest) {
                            tracing::warn!("Failed to copy included file {} -> {}: {}", src.display(), dest.display(), e);
                        } else {
                            tracing::info!("Copied included file: {}", trimmed);
                        }
                    } else if src.is_dir() {
                        tracing::warn!(".worktreeinclude: directory copying not yet implemented: {}", trimmed);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Check if the source branch is clean (no uncommitted changes).
/// Used before creating branched worktrees to prevent state pollution.
pub fn check_source_branch_clean(project_root: &str) -> Result<bool, String> {
    let repo = git2::Repository::open(project_root)
        .map_err(|e| format!("Failed to open repository at {}: {}", project_root, e))?;

    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true);

    let statuses = repo.statuses(Some(&mut opts))
        .map_err(|e| format!("Failed to check git status: {}", e))?;

    // Clean means no entries (ignoring ignored files)
    let has_changes = statuses.iter().any(|entry| !entry.status().is_ignored());
    Ok(!has_changes)
}

/// Get the .claude directory for session storage in the correct environment
pub fn claude_dir() -> PathBuf {
    match current_env() {
        Environment::Wsl => PathBuf::from("/mnt/c/Users/alond/.claude"),
        Environment::Windows => {
            if let Ok(home) = env::var("USERPROFILE") {
                PathBuf::from(home).join(".claude")
            } else if let Ok(home) = env::var("HOME") {
                PathBuf::from(home).join(".claude")
            } else {
                PathBuf::from("C:/Users/alond/.claude")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: when worktree_name is None, resolve_agent_path returns base_path directly
    /// (i.e., no .claude/worktrees/ subdirectory)
    #[test]
    fn resolve_agent_path_with_none_worktree_returns_base_path() {
        let base = "/home/user/my-repo";
        let resolved = resolve_agent_path(base, None);

        // Should NOT contain worktrees subdirectory
        assert!(!resolved.host_path.contains("worktrees"),
            "Expected base path without worktree subdir, got: {}", resolved.host_path);
        assert!(!resolved.spawn_path.contains("worktrees"),
            "Expected base path without worktree subdir, got: {}", resolved.spawn_path);
    }

    /// Test: when worktree_name is Some("foo"), resolve_agent_path returns
    /// {base}/.claude/worktrees/foo
    #[test]
    fn resolve_agent_path_with_some_worktree_returns_worktree_path() {
        let base = "/home/user/my-repo";
        let resolved = resolve_agent_path(base, Some("foo"));

        // Path should contain worktrees subdirectory and the specific worktree name
        assert!(resolved.host_path.contains("worktrees") && resolved.host_path.contains("foo"),
            "Expected worktree subdir, got: {}", resolved.host_path);
        assert!(resolved.spawn_path.contains("worktrees") && resolved.spawn_path.contains("foo"),
            "Expected worktree subdir, got: {}", resolved.spawn_path);
    }

    /// Test: when worktree_name is Some(""), it's treated as no worktree
    #[test]
    fn resolve_agent_path_with_empty_worktree_returns_base_path() {
        let base = "/home/user/my-repo";
        let resolved = resolve_agent_path(base, Some(""));

        assert!(!resolved.host_path.contains(".claude/worktrees"),
            "Expected base path without worktree subdir, got: {}", resolved.host_path);
    }

    /// Test: resolve_agent_path works with Windows paths too
    #[test]
    fn resolve_agent_path_with_windows_path() {
        let base = "C:\\Users\\user\\my-repo";
        let resolved = resolve_agent_path(base, None);

        // Should return a valid path without crashing
        assert!(!resolved.host_path.is_empty());
        assert!(!resolved.spawn_path.is_empty());
    }

    /// Test: resolve_agent_path works with WSL paths
    #[test]
    fn resolve_agent_path_with_wsl_path() {
        let base = "/mnt/c/Users/user/my-repo";
        let resolved = resolve_agent_path(base, None);

        // Should return a valid path without crashing
        assert!(!resolved.host_path.is_empty());
        assert!(!resolved.spawn_path.is_empty());
    }
}
