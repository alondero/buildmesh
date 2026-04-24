//! File tree listing

use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};
use tauri::command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub children: Vec<FileNode>,
}

fn read_dir_recursive(path: &Path, depth: usize, max_depth: usize) -> FileNode {
    let name = path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());

    let is_dir = path.is_dir();
    let mut children = Vec::new();

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
                // Skip hidden files/dirs
                let name = entry.file_name();
                if name.to_string_lossy().starts_with('.') {
                    continue;
                }
                children.push(read_dir_recursive(&entry.path(), depth + 1, max_depth));
            }
        }
    }

    FileNode {
        name,
        path: path.to_string_lossy().to_string(),
        is_dir,
        children,
    }
}

/// List a directory as a tree structure
#[command]
pub fn list_directory(path: String, max_depth: Option<usize>) -> Result<FileNode, String> {
    let path = Path::new(&path);
    if !path.exists() {
        return Err(format!("Path does not exist: {}", path.display()));
    }
    if !path.is_dir() {
        return Err(format!("Path is not a directory: {}", path.display()));
    }
    let depth = max_depth.unwrap_or(3);
    Ok(read_dir_recursive(path, 0, depth))
}
