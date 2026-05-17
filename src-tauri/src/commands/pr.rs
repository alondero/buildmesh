//! GitHub workflow via GitHub CLI (gh)

use crate::db;
use crate::models::EnvType;
use serde::{Deserialize, Serialize};
use std::process::Command;
use tauri::command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubIssue {
    pub number: i64,
    pub title: String,
    pub body: String,
}

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

/// Get open GitHub issues for a mesh via `gh issue list` + `gh issue view`.
/// Returns an empty list if no `origin` remote is configured.
#[command]
pub fn get_repo_issues(mesh_id: i64) -> Result<Vec<GitHubIssue>, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| e.to_string())?;

    // Get origin remote URL via git
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(&mesh.path)
        .output()
        .map_err(|e| format!("git error: {}", e))?;

    if !output.status.success() {
        // No origin remote or not a git repo — return empty list
        tracing::warn!("get_repo_issues: no origin remote for mesh at {}", mesh.path);
        return Ok(Vec::new());
    }

    let remote_url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let owner_repo = parse_owner_repo(&remote_url).ok_or_else(|| format!("unrecognized remote URL: {}", remote_url))?;

    // Get list of open issues with body included in a single call
    let list_output = Command::new("gh")
        .args(["issue", "list", "--repo", &owner_repo, "--state", "open", "--json", "number,title,body"])
        .current_dir(&mesh.path)
        .output()
        .map_err(|e| format!("gh issue list error: {}", e))?;

    if !list_output.status.success() {
        return Ok(Vec::new());
    }

    #[derive(Deserialize)]
    struct IssueListItem { number: i64, title: String, body: String }

    let items: Vec<IssueListItem> = serde_json::from_str(&String::from_utf8_lossy(&list_output.stdout))
        .map_err(|e| format!("failed to parse issue list: {}", e))?;

    Ok(items.into_iter().map(|item| GitHubIssue {
        number: item.number,
        title: item.title,
        body: item.body,
    }).collect())
}

/// Parse owner/repo from a GitHub URL.
/// Handles both HTTPS (https://github.com/owner/repo) and SSH (git@github.com:owner/repo) formats.
fn parse_owner_repo(url: &str) -> Option<String> {
    let rest = if url.starts_with("https://github.com/") {
        &url["https://github.com/".len()..]
    } else if url.starts_with("git@github.com:") {
        &url["git@github.com:".len()..]
    } else {
        return None;
    };

    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        Some(format!("{}/{}", parts[0], parts[1]))
    } else {
        None
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
/// `gh pr create` targeting `base_branch`. The `--head` flag is only needed
/// when the current branch differs from `base_branch` — if they are the same,
/// gh compares the branch to itself and returns "No commits between".
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
    // Only use --head if the current branch differs from base.
    // When they're identical, --head causes "No commits between X and X".
    if !branch.is_empty() && branch != base_branch {
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
