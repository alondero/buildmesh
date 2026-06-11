//! GitHub workflow via direct REST API calls (no `gh` CLI dependency)

use crate::db;
use crate::models::SessionStatus;
use crate::services::github::{self, GitHubClient, PullRequest};
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
///
/// Every command in this module talks to the GitHub REST API (blocking HTTP);
/// `(async)` moves them to a worker thread so a slow or offline network never
/// freezes the main thread (which would stall the whole UI and all other IPC).
#[command(async)]
pub fn check_gh_auth() -> bool {
    match GitHubClient::new() {
        Ok(client) => client.check_auth(),
        Err(_) => false,
    }
}

/// Get open GitHub issues for a mesh.
/// Returns an empty list if the mesh has no GitHub remote (or any error
/// resolving one), with a `warn!` capturing the reason. The modal degrades
/// gracefully — see [`resolve_github_owner_repo`] for the error wording the
/// sibling spawn endpoint surfaces directly.
#[command(async)]
pub fn get_repo_issues(mesh_id: i64) -> Result<Vec<GitHubIssue>, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| e.to_string())?;

    let (owner, repo) = match resolve_github_owner_repo(&mesh) {
        Ok(pair) => pair,
        Err(reason) => {
            tracing::warn!("get_repo_issues: {} — returning empty issue list", reason);
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
#[command(async)]
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
#[command(async)]
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
#[command(async)]
pub fn merge_pr(pr_url: String) -> Result<String, String> {
    let (owner, repo, pr_number) = parse_pr_url(&pr_url)
        .ok_or_else(|| format!("Could not parse PR URL: {}", pr_url))?;

    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    client.merge_pull_request(&owner, &repo, pr_number)
        .map_err(|e| e.to_string())
}

/// Get the current branch for a node
#[command(async)]
pub fn get_current_branch(session_id: i64) -> Result<String, String> {
    let node = db::get_agent_node_by_id(session_id)
        .map_err(|e| e.to_string())?;

    Ok(repo_info(&node.path)?.branch)
}

/// Open PR summary for an agent node — shape matches the TS `OpenPr` type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenPr {
    pub number: i64,
    pub url: String,
    pub title: String,
    pub draft: bool,
}

/// Find the open PR for the branch an agent node is working on, if any.
///
/// Returns `Ok(None)` (silent — no error) when:
///   - the node is `Archived` (closed; chip should hide, no point hitting GitHub)
///   - the path is not a git repo or the branch is unborn (detached HEAD with no name)
///   - there's no GitHub auth token (chip simply stays hidden)
///   - GitHub has no open PR for that branch (the common case)
///
/// Returns `Err(_)` only for true internal failures (DB lookup blows up, etc.).
#[command(async)]
pub fn get_open_pr_for_node(node_id: i64) -> Result<Option<OpenPr>, String> {
    let node = db::get_agent_node_by_id(node_id)
        .map_err(|e| e.to_string())?;

    // Archived = closed; saves a GitHub API call and matches the chip's
    // "doesn't show after close" contract. Node deletion removes the row,
    // so this guard is the secondary defence.
    if node.status == SessionStatus::Archived {
        return Ok(None);
    }

    let info = match repo_info(&node.path) {
        Ok(i) => i,
        Err(_) => return Ok(None),
    };

    let client = match GitHubClient::new() {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    let pr = match resolve_open_pr(&info, &client) {
        Ok(Some(p)) => p,
        Ok(None) => return Ok(None),
        Err(e) => {
            // Non-404 API failures (rate limit, network) — log and hide the chip
            // rather than spamming the UI. The next `GIT_CHANGED` will retry.
            tracing::warn!("get_open_pr_for_node({}): {}", node_id, e);
            return Ok(None);
        }
    };

    Ok(Some(OpenPr {
        number: pr.number,
        url: pr.html_url,
        title: pr.title,
        draft: pr.draft,
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct RepoInfo {
    branch: String,
    remote_url: Option<String>,
    /// `(owner, repo)` parsed from `remote_url`. `None` when the origin
    /// isn't a GitHub URL (e.g. GitLab) or no origin is configured.
    /// Cached here so [`resolve_open_pr`] and the existing `owner_repo`
    /// call sites don't reparse on every call.
    owner_repo: Option<(String, String)>,
}

impl RepoInfo {
    fn owner_repo(&self) -> Result<(String, String), String> {
        match self.owner_repo.clone() {
            Some(pair) => Ok(pair),
            None => match &self.remote_url {
                Some(u) => Err(format!("unrecognized remote URL: {}", u)),
                None => Err("No origin remote configured".to_string()),
            },
        }
    }
}

/// Pure helper: given a `RepoInfo` and a `GitHubClient`, return the open PR for the
/// current branch, or `None` if no branch / no PR / not a GitHub remote.
fn resolve_open_pr(
    info: &RepoInfo,
    client: &GitHubClient,
) -> Result<Option<PullRequest>, String> {
    if info.branch.is_empty() {
        return Ok(None);
    }
    let (owner, repo) = info.owner_repo()?;
    client
        .find_open_pr_for_branch(&owner, &repo, &info.branch)
        .map_err(|e| e.to_string())
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
    let owner_repo = remote_url.as_deref().and_then(github::parse_owner_repo);

    Ok(RepoInfo { branch, remote_url, owner_repo })
}

/// Resolve a Mesh's `origin` remote to (owner, repo) for GitHub operations,
/// with a diagnostic that disambiguates "no origin at all" from "origin exists
/// but isn't a GitHub URL". Used by both `get_repo_issues` (which degrades
/// gracefully to an empty list + warn) and `spawn_issue_agent` (which
/// propagates the error to the user, since they actively clicked Spawn).
pub(crate) fn resolve_github_owner_repo(
    mesh: &crate::models::Mesh,
) -> Result<(String, String), String> {
    match resolve_owner_repo(&mesh.path)? {
        Some(pair) => Ok(pair),
        None => {
            let has_origin = git2::Repository::open(&mesh.path)
                .ok()
                .and_then(|r| r.find_remote("origin").ok().map(|_| ()))
                .is_some();
            if has_origin {
                Err(format!(
                    "Mesh at {} has an `origin` remote, but it isn't a GitHub URL",
                    mesh.path
                ))
            } else {
                Err(format!("Mesh at {} has no `origin` remote", mesh.path))
            }
        }
    }
}

/// Resolve owner/repo from a path, returning None if no origin remote.
pub(crate) fn resolve_owner_repo(path: &str) -> Result<Option<(String, String)>, String> {
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

// ---------------------------------------------------------------------------
// Tests — focused on `repo_info` (the failure-prone git2/extraction half).
// The HTTP call to GitHub is exercised manually + via the `#[ignore]`-gated
// live test below; we don't pull in a wiremock dependency for one call site.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    /// RAII guard — deletes the temp dir on drop so tests don't leave state behind.
    /// The guard must be held by the TEST (not the helper) for as long as the
    /// path is in use; otherwise Drop fires early and the dir is gone.
    struct TempGitRepo(std::path::PathBuf);

    impl TempGitRepo {
        fn new() -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
            let tmp = std::env::temp_dir().join(format!("buildmesh_pr_test_{}", id));
            Self(tmp)
        }
        fn path(&self) -> &Path { &self.0 }
    }

    impl Drop for TempGitRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Init a fresh repo with a single commit on `main`. Returns `(guard, path)` —
    /// caller MUST hold `guard` for as long as `path` is in use.
    fn init_repo_with_commit() -> (TempGitRepo, String) {
        let tmp = TempGitRepo::new();
        fs::create_dir_all(tmp.path()).unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        let sig = git2::Signature::now("test", "test@example.com").unwrap();
        fs::write(tmp.path().join("file.txt"), "content").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        let path = tmp.path().to_string_lossy().into_owned();
        (tmp, path)
    }

    /// Init a fresh repo with NO commits. `repo.head()` will error, so
    /// `repo_info` should return an empty branch.
    fn init_repo_unborn() -> (TempGitRepo, String) {
        let tmp = TempGitRepo::new();
        fs::create_dir_all(tmp.path()).unwrap();
        git2::Repository::init(tmp.path()).unwrap();
        let path = tmp.path().to_string_lossy().into_owned();
        (tmp, path)
    }

    /// Init a fresh repo + commit + set the `origin` remote to `url`.
    fn init_repo_with_origin(url: &str) -> (TempGitRepo, String) {
        let (guard, path) = init_repo_with_commit();
        let repo = git2::Repository::open(&path).unwrap();
        repo.remote_set_url("origin", url).unwrap();
        (guard, path)
    }

    #[test]
    fn repo_info_returns_empty_branch_on_unborn_head() {
        let (_guard, path) = init_repo_unborn();
        let info = repo_info(&path).expect("repo_info should succeed even with no commits");
        assert_eq!(info.branch, "", "unborn head should produce empty branch");
        assert!(info.remote_url.is_none(), "no origin configured in this test");
        assert!(info.owner_repo.is_none(), "no origin → no owner_repo");
    }

    #[test]
    fn repo_info_parses_github_origin_https() {
        let (_guard, path) = init_repo_with_origin("https://github.com/alondero/buildmesh.git");
        let info = repo_info(&path).expect("repo_info");
        assert_eq!(info.branch, "main");
        assert_eq!(info.owner_repo, Some(("alondero".to_string(), "buildmesh".to_string())));
    }

    #[test]
    fn repo_info_parses_github_origin_ssh() {
        let (_guard, path) = init_repo_with_origin("git@github.com:alondero/buildmesh.git");
        let info = repo_info(&path).expect("repo_info");
        assert_eq!(info.owner_repo, Some(("alondero".to_string(), "buildmesh".to_string())));
    }

    #[test]
    fn repo_info_returns_none_owner_repo_for_non_github_origin() {
        let (_guard, path) = init_repo_with_origin("https://gitlab.com/alondero/buildmesh.git");
        let info = repo_info(&path).expect("repo_info");
        // remote_url is still set so the call site can render a useful error
        assert!(info.remote_url.is_some());
        // but owner_repo is None — `parse_owner_repo` is GitHub-specific
        assert!(info.owner_repo.is_none());
        // and the public accessor surfaces the original error wording
        let err = info.owner_repo().unwrap_err();
        assert!(err.contains("unrecognized remote URL"), "got: {}", err);
    }

    /// Opt-in live test — runs only with `cargo test -- --ignored` and a valid
    /// `GITHUB_TOKEN` / `gh auth login` setup. Sanity-checks that the URL
    /// format parses and the JSON shape matches the (extended) PullRequest struct.
    #[test]
    #[ignore]
    fn integration_find_open_pr_for_branch_live() {
        let client = GitHubClient::new().expect("GITHUB_TOKEN must be set");
        let pr = client
            .find_open_pr_for_branch("alondero", "buildmesh", "main")
            .expect("API call failed");
        // `main` may or may not have an open PR — both outcomes are valid signals.
        // What we're really testing is that the call shape works end-to-end.
        if let Some(p) = pr {
            assert!(p.number > 0);
            assert!(p.html_url.starts_with("https://github.com/"));
        }
    }
}
