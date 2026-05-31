//! File tree listing with host/guest path mapping

use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};
use tauri::command;
use crate::env;
use crate::process_util::command_no_window;

#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[command]
pub fn list_directory(path: String, max_depth: Option<usize>) -> Result<FileNode, String> {
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
}

/// Convert a guest/internal path to host-readable absolute path
#[command]
pub fn to_host_path(path: String) -> String {
    env::to_host_path(&path)
}

/// Open a file in the system default editor (VS Code)
#[command]
pub fn open_in_editor(path: String) -> Result<(), String> {
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

    #[test]
    fn test_to_host_path_converts_linux_path() {
        let result = to_host_path("/home/user/file.txt".to_string());
        assert!(result.contains("\\wsl$"), "Should convert to UNC path");
        assert!(result.contains("\\home\\user\\file.txt"), "Should use backslashes");
    }

    #[test]
    fn test_to_host_path_converts_mnt_path() {
        let result = to_host_path("/mnt/c/Users/test/file.txt".to_string());
        assert_eq!(result, "C:\\Users\\test\\file.txt");
    }
}
