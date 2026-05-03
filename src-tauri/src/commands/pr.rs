//! PR workflow via GitHub CLI (gh)

use crate::db;
use crate::models::EnvType;
use std::process::Command;
use tauri::command;

/// Create a PR for the node
#[command]
pub fn create_pr(
    session_id: i64,
    title: String,
    body: String,
) -> Result<String, String> {
    let node = db::get_agent_node_by_id(session_id)
        .map_err(|e| e.to_string())?;

    let branch = &node.branch;
    let path = &node.path;

    let output = if node.env == EnvType::Wsl {
        Command::new("wsl.exe")
            .args(["--cd", path, "--", "gh", "pr", "create",
                   "--title", &title, "--body", &body, "--base", branch])
            .output()
    } else {
        Command::new("gh")
            .args(["pr", "create", "--title", &title, "--body", &body, "--base", branch])
            .output()
    };

    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let url = stdout.lines().find(|l| l.starts_with("https://github.com"))
                .unwrap_or_default()
                .to_string();
            Ok(url)
        }
        Err(e) => Err(format!("error: {}", e)),
    }
}

/// Merge a PR
#[command]
pub fn merge_pr(pr_url: String) -> Result<String, String> {
    let output = Command::new("gh")
        .args(["pr", "merge", &pr_url, "--squash", "--delete-branch"])
        .output();

    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            Ok(stdout.to_string())
        }
        Err(e) => Err(format!("error: {}", e)),
    }
}

/// Get the current branch for a node
#[command]
pub fn get_current_branch(session_id: i64) -> Result<String, String> {
    let node = db::get_agent_node_by_id(session_id)
        .map_err(|e| e.to_string())?;

    let output = if node.env == EnvType::Wsl {
        Command::new("wsl.exe")
            .args(["--cd", &node.path, "--", "git", "branch", "--show-current"])
            .output()
    } else {
        Command::new("git")
            .args(["branch", "--show-current"])
            .output()
    };

    match output {
        Ok(o) => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        Err(e) => Err(format!("error: {}", e)),
    }
}
