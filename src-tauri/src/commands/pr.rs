//! PR workflow via GitHub CLI (gh)

use crate::db;
use crate::models::EnvType;
use std::process::Command;
use tauri::command;

/// Check whether the user is authenticated with GitHub via `gh`.
#[command]
pub fn check_gh_auth() -> bool {
    let output = Command::new("gh")
        .args(["auth", "status"])
        .output();

    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

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

/// Create a PR directly from a mesh directory path (no node required).
/// Detects the current branch via `git branch --show-current`, then runs
/// `gh pr create` targeting `base_branch`. On detached HEAD (empty branch)
/// the `--head` flag is added so gh uses the commit directly.
#[command]
pub fn create_pr_for_mesh(
    mesh_path: String,
    title: String,
    body: String,
    base_branch: String,
) -> Result<String, String> {
    // Detect the current branch
    let branch = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(&mesh_path)
        .output()
        .map_err(|e| format!("git error: {}", e))?
        .stdout;
    let branch = String::from_utf8_lossy(&branch).trim().to_string();

    let mut gh_args = vec!["pr", "create",
               "--title", &title, "--body", &body, "--base", &base_branch];
    if !branch.is_empty() {
        gh_args.extend(["--head", &branch]);
    }

    let output = Command::new("gh")
        .args(&gh_args)
        .current_dir(&mesh_path)
        .output()
        .map_err(|e| format!("gh error: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let url = stdout
        .lines()
        .find(|l| l.starts_with("https://github.com"))
        .unwrap_or_default()
        .to_string();
    Ok(url)
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
