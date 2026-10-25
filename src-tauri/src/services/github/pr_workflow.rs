//! Node/mesh PR creation, source validation, open-PR discovery, and merge workflows.

use super::repository::*;
use crate::commands::pr::{CreatePrResult, OpenPr, PrSource};
use crate::db;
use crate::env;
use crate::models::SessionStatus;
use crate::services::github::{CreatePrRequest, GitHubClient};

/// Sync core for [`create_pr`](crate::commands::pr::create_pr) — see [`get_repo_issues_blocking`](crate::services::github::feeds::get_repo_issues_blocking).
///
/// Takes a `&GitHubClient` so tests can drive the full production boundary
/// (DB lookup → worktree branch detection → owner_repo parsing → GitHub
/// call) with a `GitHubClient::for_test(base, token)` pointed at a fake
/// server, without needing to stand up a Tauri runtime or resolve a real
/// token via env / gh-config / keyring.
pub(crate) fn create_pr_blocking_with_client(
    client: &GitHubClient,
    session_id: i64,
    title: &str,
    body: &str,
) -> Result<String, String> {
    let node = db::get_agent_node_by_id(session_id).map_err(|e| e.to_string())?;

    let base_branch = &node.branch;

    // Read the branch from the worktree (if any) — the mesh root is on the
    // Base Ref for worktree nodes, which would create a "main → main" PR.
    let info = repo_info(&env::node_git_path(&node).host_path)?;
    if info.branch.is_empty() {
        return Err("Could not determine current branch".to_string());
    }

    let (owner, repo) = info.owner_repo()?;
    let req = CreatePrRequest {
        owner: &owner,
        repo: &repo,
        title,
        body,
        head: &info.branch,
        base: base_branch,
    };
    client
        .create_pull_request_idempotent(req)
        .map(|pr| pr.html_url)
        .map_err(|e| e.to_string())
}

/// Sync core for [`create_pr_for_mesh`](crate::commands::pr::create_pr_for_mesh) — see [`get_repo_issues_blocking`](crate::services::github::feeds::get_repo_issues_blocking).
///
/// `pub(crate)` for the same testability reason as
/// [`create_pr_blocking_with_client`].
pub(crate) fn create_pr_for_mesh_blocking_with_client(
    client: &GitHubClient,
    mesh_path: &str,
    title: &str,
    body: &str,
    base_branch: &str,
) -> Result<String, String> {
    let info = repo_info(mesh_path)?;
    if info.branch.is_empty() || info.branch == base_branch {
        return Err(format!(
            "Current branch '{}' is the same as Base Ref '{}' — nothing to compare",
            info.branch, base_branch
        ));
    }

    let (owner, repo) = info.owner_repo()?;
    let req = CreatePrRequest {
        owner: &owner,
        repo: &repo,
        title,
        body,
        head: &info.branch,
        base: base_branch,
    };
    client
        .create_pull_request_idempotent(req)
        .map(|pr| pr.html_url)
        .map_err(|e| e.to_string())
}

/// Owner/repo parsed from the node worktree's origin. Kept separate from
/// [`PrSource`] because a preview must still render the branches for a
/// non-GitHub remote, while creating a PR legitimately cannot.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedPrSource {
    pub source: PrSource,
    pub owner_repo: Option<(String, String)>,
}

/// Strip a remote qualifier from a mesh `base_ref`: `origin/main` → `main`,
/// `upstream/release` → `release`. GitHub's `base=` expects a branch name
/// without the remote, so a mesh configured against `origin/trunk` must not
/// silently become `main`.
///
/// The prefix is only stripped when it is one of the repository's **configured
/// remotes**. There is no syntax that separates a remote from the first
/// segment of a real branch name — `feature/x` and `origin/main` are the same
/// shape — so guessing from the characters alone would rewrite a branch called
/// `feature/x` into `x` and target a branch that does not exist. An unrecognised
/// prefix is left intact.
pub(crate) fn local_base_branch(base_ref: &str, remotes: &[String]) -> String {
    let trimmed = base_ref.trim();
    match trimmed.split_once('/') {
        Some((remote, rest)) if !rest.is_empty() && remotes.iter().any(|r| r == remote) => {
            rest.to_string()
        }
        _ => trimmed.to_string(),
    }
}

/// Why resolving a PR source failed, in the terms the HTTP layer needs.
///
/// This exists because the route cannot infer intent from message text. An
/// earlier version matched `Err(e) if e.starts_with("Agent node")` to pick
/// `403`, but three different failures share that prefix — including the
/// harmless "branch is the same as the Base Ref" validation case — so a
/// validation error came back as 403, which the mobile client treats as an
/// auth failure (`isAuthError`) and answers by wiping the session and
/// bouncing the user to the pairing screen (issue #2190 review).
///
/// Each variant maps to exactly one status:
/// - [`PrSourceError::NotFound`] → 404
/// - [`PrSourceError::NotOwned`] → 403 (genuinely an authorization failure)
/// - [`PrSourceError::SameBranch`] / [`PrSourceError::BranchUnknown`] → 422
/// - [`PrSourceError::Other`] → 500
#[derive(Debug)]
pub enum PrSourceError {
    /// No agent node row with that id.
    NotFound(i64),
    /// The node exists but belongs to a different mesh.
    NotOwned { node_id: i64, mesh_id: i64 },
    /// The worktree sits on the branch the PR would target — nothing to compare.
    SameBranch { branch: String, base: String },
    /// The worktree's checked-out branch could not be read.
    BranchUnknown,
    /// The worktree moved off the branch the client previewed, so submitting
    /// would publish something the user never saw.
    StaleHead { actual: String, expected: String },
    /// Git, filesystem, database or GitHub-client failure.
    Other(String),
}

impl std::fmt::Display for PrSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrSourceError::NotFound(node_id) => {
                write!(f, "Agent node {node_id} was not found")
            }
            PrSourceError::NotOwned { node_id, mesh_id } => {
                write!(f, "Agent node {node_id} does not belong to mesh {mesh_id}")
            }
            PrSourceError::SameBranch { branch, base } => write!(
                f,
                "Agent node branch '{branch}' is the same as the mesh Base Ref '{base}' — nothing to compare"
            ),
            PrSourceError::BranchUnknown => {
                write!(f, "Could not determine the agent node's current branch")
            }
            PrSourceError::StaleHead { actual, expected } => write!(
                f,
                "Agent node worktree is on '{actual}', not the source branch '{expected}' that was previewed"
            ),
            PrSourceError::Other(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for PrSourceError {}

impl From<String> for PrSourceError {
    fn from(message: String) -> Self {
        PrSourceError::Other(message)
    }
}

/// HTTP status this failure must be reported as. Lives beside the enum so the
/// mapping is stated once and both routes use it.
impl PrSourceError {
    pub(crate) fn status(&self) -> &'static str {
        match self {
            PrSourceError::NotFound(_) => "404 Not Found",
            PrSourceError::NotOwned { .. } => "403 Forbidden",
            PrSourceError::SameBranch { .. }
            | PrSourceError::BranchUnknown
            | PrSourceError::StaleHead { .. } => "422 Unprocessable Entity",
            PrSourceError::Other(_) => "500 Internal Server Error",
        }
    }
}

/// Resolve the source/target branches for a create-PR from an agent node.
///
/// Shared by the preview route and the create route so the branches the
/// sheet shows before submitting are, by construction, the branches the
/// create will use — they cannot drift apart.
pub(crate) fn resolve_pr_source_for_node(
    mesh_id: i64,
    node_id: i64,
) -> Result<ResolvedPrSource, PrSourceError> {
    let node = db::get_agent_node_by_id(node_id).map_err(|_| PrSourceError::NotFound(node_id))?;
    // Ownership check: a node id from another mesh must not be usable to mint
    // a PR against this mesh's repo. Enforced here and in the create route;
    // both need it, and this is where the distinction between "not yours" and
    // "not resolvable" is actually made. Not a `#[command]` — the registered
    // entry point is `create_pr_for_node_source` below.
    if node.mesh_id != mesh_id {
        return Err(PrSourceError::NotOwned { node_id, mesh_id });
    }
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| PrSourceError::Other(e.to_string()))?;

    // The node's worktree, never `mesh.path`. `host_path` is the
    // Windows-side view; git operations must never see a WSL path
    // (see `env::host_path`).
    let info = repo_info(&env::node_git_path(&node).host_path)?;
    if info.branch.is_empty() {
        return Err(PrSourceError::BranchUnknown);
    }

    let base_branch = local_base_branch(&mesh.base_ref, &info.remotes);
    if info.branch == base_branch {
        return Err(PrSourceError::SameBranch {
            branch: info.branch,
            base: base_branch,
        });
    }

    Ok(ResolvedPrSource {
        source: PrSource {
            head_branch: info.branch,
            base_branch,
        },
        owner_repo: info.owner_repo,
    })
}

/// Sync core for [`create_pr_for_node_source`](crate::commands::pr::create_pr_for_node_source) — see
/// [`create_pr_for_mesh_blocking_with_client`] for why the client is injected.
pub(crate) fn create_pr_for_node_source_blocking_with_client(
    client: &GitHubClient,
    mesh_id: i64,
    node_id: i64,
    title: &str,
    body: &str,
    base_branch: Option<&str>,
    expected_head: Option<&str>,
) -> Result<CreatePrResult, PrSourceError> {
    let resolved = resolve_pr_source_for_node(mesh_id, node_id)?;
    let mut source = resolved.source;

    if let Some(expected) = expected_head.map(str::trim).filter(|h| !h.is_empty()) {
        if expected != source.head_branch {
            return Err(PrSourceError::StaleHead {
                actual: source.head_branch,
                expected: expected.to_string(),
            });
        }
    }

    if let Some(requested) = base_branch.map(str::trim).filter(|b| !b.is_empty()) {
        if requested == source.head_branch {
            return Err(PrSourceError::SameBranch {
                branch: source.head_branch,
                base: requested.to_string(),
            });
        }
        source.base_branch = requested.to_string();
    }

    let (owner, repo) = resolved.owner_repo.ok_or_else(|| {
        PrSourceError::Other("This repository has no GitHub origin remote".into())
    })?;

    let req = CreatePrRequest {
        owner: &owner,
        repo: &repo,
        title,
        body,
        head: &source.head_branch,
        base: &source.base_branch,
    };
    // `create_pull_request_idempotent` carries the #771 open-PR recovery, and
    // it keys that recovery on `head` — so running it AFTER source
    // resolution means the duplicate check consults the node's real branch
    // rather than the mesh root's.
    client
        .create_pull_request_idempotent(req)
        .map(|pr| CreatePrResult {
            url: pr.html_url,
            head_branch: source.head_branch,
            base_branch: source.base_branch,
        })
        .map_err(|e| PrSourceError::Other(e.to_string()))
}

/// Sync core for [`merge_pr`](crate::commands::pr::merge_pr) — see [`get_repo_issues_blocking`](crate::services::github::feeds::get_repo_issues_blocking).
pub(crate) fn merge_pr_blocking(
    pr_url: String,
    merge_method: Option<String>,
) -> Result<String, String> {
    let (owner, repo, pr_number) =
        parse_pr_url(&pr_url).ok_or_else(|| format!("Could not parse PR URL: {}", pr_url))?;

    let method = normalise_merge_method(merge_method);

    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    client
        .merge_pull_request(&owner, &repo, pr_number, &method)
        .map_err(|e| e.to_string())
}

/// Sync core for [`get_current_branch`](crate::commands::pr::get_current_branch).
pub(crate) fn get_current_branch_blocking(session_id: i64) -> Result<String, String> {
    let node = db::get_agent_node_by_id(session_id).map_err(|e| e.to_string())?;

    // Route through the worktree (if any) — the mesh root is on the base
    // branch for worktree nodes. See `node_git_path` for the rationale.
    Ok(repo_info(&env::node_git_path(&node).host_path)?.branch)
}

/// Sync core for [`get_open_pr_for_node`](crate::commands::pr::get_open_pr_for_node) — see [`get_repo_issues_blocking`](crate::services::github::feeds::get_repo_issues_blocking).
pub(crate) fn get_open_pr_for_node_blocking(node_id: i64) -> Result<Option<OpenPr>, String> {
    let node = db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())?;

    // Archived = closed; saves a GitHub API call and matches the chip's
    // "doesn't show after close" contract. Node deletion removes the row,
    // so this guard is the secondary defence.
    if node.status == SessionStatus::Archived {
        return Ok(None);
    }

    // Worktree nodes: open the worktree directory, not the mesh root.
    // The mesh root is on the base branch while the agent's HEAD is on the
    // worktree's branch — see `node_git_path` for the rationale and the
    // matching frontend helper (`getNodeGitPath`).
    let info = match repo_info(&env::node_git_path(&node).host_path) {
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
        head_ref: pr.head_ref,
        head_sha: pr.head_sha,
        head_repo_owner: pr.head_repo_owner,
        head_repo_clone_url: pr.head_repo_clone_url,
    }))
}

/// Parse a GitHub PR URL into (owner, repo, pr_number).
/// Validate the merge-method vocabulary at the wire seam. An absent or
/// unrecognised value falls back to `"squash"` (the historical
/// behaviour) so older clients keep working; the panel's dropdown sends
/// one of the three literals GitHub's REST merge endpoint understands.
pub(crate) fn normalise_merge_method(merge_method: Option<String>) -> String {
    merge_method
        .filter(|m| matches!(m.as_str(), "squash" | "merge" | "rebase"))
        .unwrap_or_else(|| "squash".to_string())
}

pub(crate) fn parse_pr_url(url: &str) -> Option<(String, String, i64)> {
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
