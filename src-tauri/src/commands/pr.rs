//! GitHub workflow via direct REST API calls (no `gh` CLI dependency)

use crate::db;
use crate::env;
use crate::models::SessionStatus;
use crate::services::github::{self, GitHubClient, PullRequest};
use git2::Repository;
use serde::{Deserialize, Serialize};
use tauri::command;
use ts_rs::TS;

/// Wire shape of `get_repo_issues` (desktop Tauri) and `GET /api/meshes/{id}/issues`
/// (mobile HTTP) — both serialise this exact struct. The TS type is generated from
/// here (issue #359); `i64` fields carry `#[ts(as = "i32")]` because serde_json
/// emits them as JS numbers, not the `bigint` ts-rs would default to.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "GitHubIssue.ts")]
pub struct GitHubIssue {
    #[ts(as = "i32")]
    pub number: i64,
    pub title: String,
    pub body: String,
    /// Absolute GitHub URL for the issue. The mobile "View ↗" link opens
    /// this directly. Always present — `services::github::Issue` carries
    /// `#[serde(default)]` on `html_url`, so a partial response yields `""`
    /// rather than failing to parse.
    pub url: String,
    /// `"open"` or `"closed"`. Currently always `"open"` because
    /// `list_issues_only` filters to open issues; kept on the wire so a
    /// future endpoint widening to both doesn't require a TS-side change.
    pub state: String,
    /// Label names (flattened from the GitHub API's `[{name, color, ...}]`).
    /// Empty array when the issue has no labels.
    pub labels: Vec<String>,
}

/// Wire shape of `get_repo_pulls` (desktop Tauri) — one entry per pull request.
/// Generated to TS via ts-rs (issue #359); `i64` carries `#[ts(as = "i32")]`
/// because serde_json emits it as a JS number, not the `bigint` ts-rs defaults
/// to. Mergeability is intentionally NOT on this struct: the `/pulls` list
/// endpoint omits it, so the panel enriches each PR via `get_pr_mergeability`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "GitHubPullRequest.ts")]
pub struct GitHubPullRequest {
    #[ts(as = "i32")]
    pub number: i64,
    pub title: String,
    pub body: String,
    /// Absolute GitHub URL for the PR — also the argument `merge_pr` parses.
    pub url: String,
    /// `"open"` or `"closed"` — echoes the requested `state` filter.
    pub state: String,
    /// `true` for draft PRs. Drafts can't be merged, so the panel flags them
    /// without needing a per-PR mergeability call.
    pub draft: bool,
}

/// Wire shape of `get_pr_mergeability` — the per-PR enrichment the panel
/// requests after the list loads. `mergeable` is `null`/`None` while GitHub is
/// still computing the merge; the panel renders a "checking" state for that.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "PrMergeability.ts")]
pub struct PrMergeability {
    /// `Some(true)` mergeable, `Some(false)` conflicts, `None` still computing.
    pub mergeable: Option<bool>,
    /// GitHub's `mergeable_state` (`clean`, `dirty`, `blocked`, `behind`,
    /// `unstable`, `unknown`, …) — used for the flag wording.
    pub mergeable_state: String,
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
        url: issue.html_url,
        state: issue.state,
        labels: issue.labels,
    }).collect())
}

/// Get pull requests for a mesh, filtered by `state` (`"open"` or `"closed"`).
/// Mirrors [`get_repo_issues`]: degrades to an empty list (with a `warn!`) when
/// the mesh has no GitHub origin, so the panel renders an empty state rather
/// than an error. Mergeability is fetched separately per PR — see
/// [`get_pr_mergeability`] — because the list endpoint omits it.
#[command(async)]
pub fn get_repo_pulls(mesh_id: i64, state: String) -> Result<Vec<GitHubPullRequest>, String> {
    // Only ever forward a known filter to GitHub; anything unexpected falls
    // back to "open" rather than letting an arbitrary string reach the API.
    let state = if state == "closed" { "closed" } else { "open" };

    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;

    let (owner, repo) = match resolve_github_owner_repo(&mesh) {
        Ok(pair) => pair,
        Err(reason) => {
            tracing::warn!("get_repo_pulls: {} — returning empty PR list", reason);
            return Ok(Vec::new());
        }
    };

    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    let prs = client.list_pull_requests(&owner, &repo, state).map_err(|e| e.to_string())?;

    Ok(prs.into_iter().map(|pr| GitHubPullRequest {
        number: pr.number,
        title: pr.title,
        body: pr.body,
        url: pr.html_url,
        state: pr.state,
        draft: pr.draft,
    }).collect())
}

/// Get a single PR's mergeability for a mesh's repo. The panel calls this once
/// per open PR after the list loads. `mergeable` is `null`/`None` while GitHub
/// computes the merge — surfaced as-is so the UI can show a "checking" state.
#[command(async)]
pub fn get_pr_mergeability(mesh_id: i64, pr_number: i64) -> Result<PrMergeability, String> {
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let (owner, repo) = resolve_github_owner_repo(&mesh)?;

    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    let (mergeable, mergeable_state) = client
        .pull_request_mergeability(&owner, &repo, pr_number)
        .map_err(|e| e.to_string())?;

    Ok(PrMergeability { mergeable, mergeable_state })
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

    // Read the branch from the worktree (if any) — the mesh root is on the
    // base branch for worktree nodes, which would create a "main → main" PR.
    let info = repo_info(&env::node_working_path(&node).host_path)?;
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

    // Route through the worktree (if any) — the mesh root is on the base
    // branch for worktree nodes. See `node_working_path` for the rationale.
    Ok(repo_info(&env::node_working_path(&node).host_path)?.branch)
}

/// Open PR summary for an agent node — shape matches the TS `OpenPr` type.
///
/// Generated to src/types/generated/OpenPr.ts (issue #404). `i64` carries
/// `#[ts(as = "i32")]` so it emits `number` (matches `GitHubIssue.number`).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "OpenPr.ts")]
pub struct OpenPr {
    #[ts(as = "i32")]
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

    // Worktree nodes: open the worktree directory, not the mesh root.
    // The mesh root is on the base branch while the agent's HEAD is on the
    // worktree's branch — see `node_working_path` for the rationale and the
    // matching frontend helper (`getNodeGitPath`).
    let info = match repo_info(&env::node_working_path(&node).host_path) {
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
    use crate::env::test_helpers::init_repo_with_commit as init_repo_for_test;
    use crate::git::worktree::create_git_worktree;
    use crate::models::{AgentNode, EnvType, Provider, SessionStatus};
    use chrono::Utc;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    /// Build a `TempGitRepo` and an `AgentNode` that uses a branched worktree
    /// under `<root>/.claude/worktrees/<name>`. The mesh root stays on
    /// `main`; the worktree is on `<name>` (matching the production
    /// `add_worktree_impl` branched-mode behaviour). Caller MUST hold
    /// `TempGitRepo` for the node's lifetime, otherwise Drop wipes the dir.
    fn make_worktree_node() -> (TempGitRepo, String, AgentNode) {
        let tmp = TempGitRepo::new();
        let root = tmp.path().to_path_buf();
        init_repo_for_test(&root, &[("README.md", "init\n")]);

        // The .claude/worktrees/ layout is what production uses — see
        // `env::resolve_agent_path`. Keep tests faithful so the path
        // resolution helper finds the worktree the same way production does.
        let wt_dir = root.join(".claude").join("worktrees").join("agent-1");
        create_git_worktree(
            root.to_str().unwrap(),
            wt_dir.to_str().unwrap(),
            "agent-1",
            "branched",
            "HEAD",
        )
        .expect("worktree creation must succeed");

        let node = AgentNode {
            id: 1,
            mesh_id: 1,
            name: "agent-1".into(),
            path: root.to_string_lossy().into_owned(),
            branch: "main".into(), // The base branch (NOT the agent's branch)
            env: EnvType::Windows,
            provider: Provider::Anthropic,
            status: SessionStatus::Idle,
            cli_session_id: None,
            worktree_name: Some("agent-1".into()),
            use_worktree: true,
            source_issue: None,
            position: 0,
            created_at: Utc::now(),
        };
        (tmp, wt_dir.to_string_lossy().into_owned(), node)
    }

    /// Build a non-worktree `AgentNode` — agent runs in the mesh root.
    /// `path` is what the agent works in; no `.claude/worktrees/`.
    fn make_root_node() -> (TempGitRepo, String, AgentNode) {
        let tmp = TempGitRepo::new();
        let root = tmp.path().to_path_buf();
        init_repo_for_test(&root, &[("README.md", "init\n")]);

        let node = AgentNode {
            id: 2,
            mesh_id: 1,
            name: "root-agent".into(),
            path: root.to_string_lossy().into_owned(),
            branch: "main".into(),
            env: EnvType::Windows,
            provider: Provider::Anthropic,
            status: SessionStatus::Idle,
            cli_session_id: None,
            worktree_name: None,
            use_worktree: false,
            source_issue: None,
            position: 0,
            created_at: Utc::now(),
        };
        (tmp, root.to_string_lossy().into_owned(), node)
    }

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

    /// The agent's HEAD is on the worktree's branch (NOT the mesh root's branch).
    /// `repo_info` previously opened `node.path` (= mesh root) and got `main`,
    /// so the PR chip was looking for `head=alondero:main` instead of the
    /// agent's actual branch. The `node_working_path` helper routes to the
    /// worktree directory; the chip then resolves the right branch.
    #[test]
    fn node_working_path_resolves_to_worktree_dir_for_worktree_nodes() {
        let (_guard, _wt_path, node) = make_worktree_node();
        let resolved = env::node_working_path(&node).host_path;
        // `resolve_agent_path` builds the path with `/` separators, so on
        // Windows we may see mixed slashes — git2 handles that fine, so we
        // assert on the canonical path components rather than the raw string.
        let canonical = std::fs::canonicalize(&resolved).expect("worktree path must exist");
        let canonical_str = canonical.to_string_lossy();
        assert!(
            canonical_str.contains(".claude")
                && canonical_str.contains("worktrees")
                && canonical_str.contains("agent-1"),
            "expected the canonical worktree path under .claude/worktrees/agent-1, got: {}",
            canonical_str,
        );
    }

    /// Non-worktree nodes have no worktree subdirectory — the agent works in
    /// the mesh root itself, so the helper should return `node.path` unchanged.
    #[test]
    fn node_working_path_resolves_to_mesh_path_for_root_nodes() {
        let (_guard, _root_path, node) = make_root_node();
        let resolved = env::node_working_path(&node).host_path;
        assert_eq!(resolved, node.path, "non-worktree node must resolve to its own path");
        assert!(!resolved.contains("worktrees"), "must NOT add a worktree subdir for root nodes");
    }

    /// End-to-end: with a real worktree on branch `agent-1` and the mesh root
    /// on `main`, reading `repo_info(working_path)` should give `agent-1`.
    /// This is the exact bug the PR chip had: previously it read `main`.
    #[test]
    fn repo_info_via_working_path_returns_worktree_branch_not_root_branch() {
        let (_guard, _wt_path, node) = make_worktree_node();
        let resolved = env::node_working_path(&node).host_path;
        let info = repo_info(&resolved).expect("worktree repo must open");
        assert_eq!(
            info.branch, "agent-1",
            "PR chip looks up head=<branch>; the agent's working branch is the worktree name, not the mesh's base branch"
        );
        // And — for the bug regression — opening the MESH ROOT itself gives
        // `main`, not `agent-1`. This is the exact mismatch that hid the chip.
        let root_info = repo_info(&node.path).expect("mesh root must open");
        assert_eq!(
            root_info.branch, "main",
            "sanity: mesh root is on the base branch; this is what the bug used to read"
        );
    }

    /// `get_current_branch` is the mobile REST route's backing command; for a
    /// worktree node it must report the worktree's branch, not the mesh's
    /// base branch. Same bug class as the PR chip.
    #[test]
    fn get_current_branch_via_working_path_returns_worktree_branch() {
        let (_guard, _wt_path, node) = make_worktree_node();
        let branch = repo_info(&env::node_working_path(&node).host_path)
            .expect("worktree repo must open")
            .branch;
        assert_eq!(branch, "agent-1", "must read the worktree's HEAD, not the mesh root's");
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
