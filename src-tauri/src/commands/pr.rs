//! GitHub workflow via direct REST API calls (no `gh` CLI dependency)

use crate::db;
use crate::services::github::{self, GitHubClient};
use git2::Repository;
use serde::{Deserialize, Serialize};
use tauri::command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubIssue {
    pub number: i64,
    pub title: String,
    pub body: String,
}

/// Check whether the user has a valid GitHub token (env var or gh config).
#[command]
pub fn check_gh_auth() -> bool {
    match GitHubClient::new() {
        Ok(client) => client.check_auth(),
        Err(_) => false,
    }
}

/// Get open GitHub issues for a mesh.
/// Returns an empty list if no `origin` remote is configured.
#[command]
pub fn get_repo_issues(mesh_id: i64) -> Result<Vec<GitHubIssue>, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| e.to_string())?;

    let (owner, repo) = match resolve_owner_repo(&mesh.path)? {
        Some(pair) => pair,
        None => {
            tracing::warn!("get_repo_issues: no origin remote for mesh at {}", mesh.path);
            return Ok(Vec::new());
        }
    };

    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    let issues = client.list_issues_only(&owner, &repo).map_err(|e| e.to_string())?;

    Ok(issues.into_iter().map(|issue| GitHubIssue {
        number: issue.number,
        title: issue.title,
        body: issue.body,
    }).collect())
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

    let base_branch = &node.branch;
    let path = &node.path;

    let info = repo_info(path)?;
    if info.branch.is_empty() {
        return Err("Could not determine current branch".to_string());
    }

    let (owner, repo) = info.owner_repo()?;
    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    client.create_pull_request(&owner, &repo, &title, &body, &info.branch, base_branch)
        .map_err(|e| e.to_string())
}

/// Create a PR directly from a mesh directory path (no node required).
/// Detects the current branch via git2, then creates a PR targeting `base_branch`.
#[command]
pub fn create_pr_for_mesh(
    mesh_path: String,
    title: String,
    body: String,
    base_branch: String,
) -> Result<String, String> {
    let info = repo_info(&mesh_path)?;
    if info.branch.is_empty() || info.branch == base_branch {
        return Err(format!(
            "Current branch '{}' is the same as base branch '{}' — nothing to compare",
            info.branch, base_branch
        ));
    }

    let (owner, repo) = info.owner_repo()?;
    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    client.create_pull_request(&owner, &repo, &title, &body, &info.branch, &base_branch)
        .map_err(|e| e.to_string())
}

/// Merge a PR (squash + delete branch).
/// Accepts a full GitHub PR URL like `https://github.com/owner/repo/pull/123`.
#[command]
pub fn merge_pr(pr_url: String) -> Result<String, String> {
    let (owner, repo, pr_number) = parse_pr_url(&pr_url)
        .ok_or_else(|| format!("Could not parse PR URL: {}", pr_url))?;

    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    client.merge_pull_request(&owner, &repo, pr_number)
        .map_err(|e| e.to_string())
}

/// Get the current branch for a node
#[command]
pub fn get_current_branch(session_id: i64) -> Result<String, String> {
    let node = db::get_agent_node_by_id(session_id)
        .map_err(|e| e.to_string())?;

    Ok(repo_info(&node.path)?.branch)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct RepoInfo {
    branch: String,
    remote_url: Option<String>,
}

impl RepoInfo {
    fn owner_repo(&self) -> Result<(String, String), String> {
        let url = self.remote_url.as_deref()
            .ok_or_else(|| "No origin remote configured".to_string())?;
        github::parse_owner_repo(url)
            .ok_or_else(|| format!("unrecognized remote URL: {}", url))
    }
}

/// Open the repo once and extract both the current branch and origin URL.
fn repo_info(path: &str) -> Result<RepoInfo, String> {
    let repo = Repository::open(path).map_err(|e| format!("git error: {}", e))?;

    let branch = match repo.head() {
        Ok(head) => {
            if head.is_branch() {
                head.shorthand().unwrap_or("").to_string()
            } else {
                head.target()
                    .map(|oid| oid.to_string()[..8].to_string())
                    .unwrap_or_default()
            }
        }
        Err(_) => String::new(),
    };

    let remote_url = repo.find_remote("origin")
        .ok()
        .and_then(|r| r.url().map(|u| u.to_string()));

    Ok(RepoInfo { branch, remote_url })
}

/// Resolve owner/repo from a path, returning None if no origin remote.
fn resolve_owner_repo(path: &str) -> Result<Option<(String, String)>, String> {
    let repo = Repository::open(path).map_err(|e| format!("git error: {}", e))?;
    let url = match repo.find_remote("origin") {
        Ok(remote) => remote.url().map(|u| u.to_string()),
        Err(_) => return Ok(None),
    };
    match url {
        Some(u) => Ok(github::parse_owner_repo(&u)),
        None => Ok(None),
    }
}

/// Parse a GitHub PR URL into (owner, repo, pr_number).
fn parse_pr_url(url: &str) -> Option<(String, String, i64)> {
    let rest = url.strip_prefix("https://github.com/")?;
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() >= 4 && parts[2] == "pull" {
        let owner = parts[0].to_string();
        let repo = parts[1].to_string();
        let number: i64 = parts[3].parse().ok()?;
        Some((owner, repo, number))
    } else {
        None
    }
}
