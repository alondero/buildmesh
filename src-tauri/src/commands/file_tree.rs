//! File tree listing with host/guest path mapping

use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};
use tauri::command;
use ts_rs::TS;
use crate::env;
use crate::process_util::command_no_window;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "FileNode.ts")]
/// Generated to src/types/generated/FileNode.ts (issue #404). The recursive
/// `children: Vec<FileNode>` is handled by ts-rs.
pub struct FileNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub children: Vec<FileNode>,
}

fn read_dir_recursive(path: &Path, base_path: &str, host_base: &str, depth: usize, max_depth: usize) -> FileNode {
    let name = path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());

    let is_dir = path.is_dir();
    let mut children = Vec::new();

    // Calculate the internal path for the frontend
    // We replace the host-specific prefix with the original guest path
    let host_path_str = path.to_string_lossy().to_string();
    let internal_path = if let Some(stripped) = host_path_str.strip_prefix(host_base) {
        format!("{}{}", base_path, &stripped.replace('\\', "/"))
    } else {
        host_path_str.replace('\\', "/")
    };

    if is_dir && depth < max_depth {
        if let Ok(entries) = fs::read_dir(path) {
            let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            // Sort: directories first, then alphabetically
            entries.sort_by(|a, b| {
                let a_is_dir = a.path().is_dir();
                let b_is_dir = b.path().is_dir();
                match (a_is_dir, b_is_dir) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.file_name().cmp(&b.file_name()),
                }
            });
            for entry in entries {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with('.') {
                    continue;
                }
                children.push(read_dir_recursive(&entry.path(), base_path, host_base, depth + 1, max_depth));
            }
        }
    }

    FileNode {
        name,
        path: internal_path,
        is_dir,
        children,
    }
}

/// List a directory as a tree structure
// Offloaded via `run_blocking` (issue #762 convention): `#[command(async)]`
// ran the recursive walk on a *bounded tokio* worker, parking it for the
// whole traversal — slow on big trees and pathological on WSL UNC paths.
#[command]
pub async fn list_directory(path: String, max_depth: Option<usize>) -> Result<FileNode, String> {
    crate::commands::run_blocking("list_directory", move || {
        let host_path = env::to_host_path(&path);
        let path_obj = Path::new(&host_path);

        if !path_obj.exists() {
            return Err(format!("Path does not exist: {} (mapped from {})", host_path, path));
        }
        if !path_obj.is_dir() {
            return Err(format!("Path is not a directory: {}", host_path));
        }

        let depth = max_depth.unwrap_or(3);
        Ok(read_dir_recursive(path_obj, &path, &host_path, 0, depth))
    })
    .await
}

/// Convert a guest/internal path to host-readable absolute path
#[command]
pub fn to_host_path(path: String) -> String {
    env::to_host_path(&path)
}

/// Open a file in the system default editor (VS Code)
// Offloaded via `run_blocking`: process creation on a loaded Windows box can
// take tens of ms — cheap, but no reason to park a bounded tokio worker.
#[command]
pub async fn open_in_editor(path: String) -> Result<(), String> {
    crate::commands::run_blocking("open_in_editor", move || open_in_editor_blocking(path)).await
}

/// Sync core for [`open_in_editor`].
fn open_in_editor_blocking(path: String) -> Result<(), String> {
    let host_path = env::to_host_path(&path);

    #[cfg(target_os = "windows")]
    {
        command_no_window("cmd.exe")
            .args(["/c", "start", "code", &host_path])
            .spawn()
            .map_err(|e| format!("Failed to open editor: {}", e))?;
    }

    #[cfg(not(target_os = "windows"))]
    {
        command_no_window("code")
            .arg(&host_path)
            .spawn()
            .map_err(|e| format!("Failed to open editor: {}", e))?;
    }

    Ok(())
}

/// Open a folder in the OS file manager (Explorer on Windows, Finder on macOS,
/// the user's default file manager on Linux). The path must point to a
/// directory — opening a file in a file manager isn't useful and would render
/// the parent folder instead, which is surprising.
// Offloaded via `run_blocking` — same rationale as `open_in_editor`.
#[command]
pub async fn open_in_file_manager(path: String) -> Result<(), String> {
    crate::commands::run_blocking("open_in_file_manager", move || {
        open_in_file_manager_blocking(path)
    })
    .await
}

/// Sync core for [`open_in_file_manager`].
fn open_in_file_manager_blocking(path: String) -> Result<(), String> {
    let host_path = env::to_host_path(&path);
    let path_obj = Path::new(&host_path);

    if !path_obj.exists() {
        return Err(format!("Path does not exist: {}", host_path));
    }
    if !path_obj.is_dir() {
        return Err(format!("Path is not a directory: {}", host_path));
    }

    tracing::info!("open_in_file_manager: resolved path = {}", host_path);

    #[cfg(target_os = "windows")]
    {
        // Buildmesh stores worktree paths as mixed slashes — e.g. an agent's
        // worktree is `X:\src\buildmesh/.claude/worktrees/foo` (backslashes
        // in the mesh path, forward slashes for the worktree subdir). The
        // Windows file APIs accept that, but `explorer.exe`'s argument parser
        // does its own quote-handling and silently falls back to opening
        // My Documents when the path uses mixed separators. Normalize to
        // all-backslashes here so the path we hand the shell is unambiguous.
        let normalized = host_path.replace('/', "\\");

        // Canonical robust pattern: `cmd /c start "" "<path>"`. The first
        // quoted arg is `start`'s window title (must be present, can be empty),
        // the second is the path. Direct `explorer.exe <path>` works most of
        // the time but mishandles the mixed-slash case above; routing through
        // `start` lets cmd normalise the path before explorer sees it.
        command_no_window("cmd.exe")
            .args(["/c", "start", "", &normalized])
            .spawn()
            .map_err(|e| format!("Failed to open file manager: {}", e))?;
    }

    #[cfg(target_os = "macos")]
    {
        command_no_window("open")
            .arg(&host_path)
            .spawn()
            .map_err(|e| format!("Failed to open file manager: {}", e))?;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        command_no_window("xdg-open")
            .arg(&host_path)
            .spawn()
            .map_err(|e| format!("Failed to open file manager: {}", e))?;
    }

    Ok(())
}

/// Get the platform-specific ~/.claude directory path
#[command]
pub fn get_user_config_dir() -> String {
    env::claude_dir().to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_host_path_preserves_windows_path() {
        let result = to_host_path("C:\\Users\\test\\file.txt".to_string());
        assert_eq!(result, "C:\\Users\\test\\file.txt");
    }

    // WSL translation only happens on a Windows host (that's the only host with
    // a WSL filesystem). These two assert the Windows-side conversion, so they
    // are gated to Windows — on macOS/Linux `to_host_path` is an identity
    // pass-through (see `test_to_host_path_is_identity_off_windows`).
    #[cfg(target_os = "windows")]
    #[test]
    fn test_to_host_path_converts_linux_path() {
        let result = to_host_path("/home/user/file.txt".to_string());
        assert!(result.contains("\\wsl$"), "Should convert to UNC path"); // allow-wsl-path
        assert!(result.contains("\\home\\user\\file.txt"), "Should use backslashes");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_to_host_path_converts_mnt_path() {
        let result = to_host_path("/mnt/c/Users/test/file.txt".to_string());
        assert_eq!(result, "C:\\Users\\test\\file.txt");
    }

    /// On macOS / native Linux there is no WSL to translate into, so a POSIX
    /// path must be returned unchanged rather than rewritten to a WSL UNC path
    /// (readiness fix — otherwise git/diff/file-tree break on Linux).
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn test_to_host_path_is_identity_off_windows() {
        assert_eq!(
            to_host_path("/home/user/file.txt".to_string()),
            "/home/user/file.txt"
        );
        assert_eq!(
            to_host_path("/mnt/c/Users/test/file.txt".to_string()),
            "/mnt/c/Users/test/file.txt"
        );
    }

    #[test]
    fn test_open_in_file_manager_rejects_missing_path() {
        // A path that cannot exist anywhere — should fail before any spawn.
        // Exercises the `_blocking` core directly (the command is now a thin
        // `run_blocking` wrapper, issue #762 convention).
        let result = open_in_file_manager_blocking(
            "Z:\\definitely-not-a-real-buildmesh-path-12345".to_string(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn test_open_in_file_manager_rejects_file() {
        // cargo's manifest is a file, not a directory — opening it in a file
        // manager would render the parent (surprising), so we reject.
        let manifest = env!("CARGO_MANIFEST_DIR").to_string() + "\\Cargo.toml";
        let result = open_in_file_manager_blocking(manifest);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("is not a directory"));
    }
}
