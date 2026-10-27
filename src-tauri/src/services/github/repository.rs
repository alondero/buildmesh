//! Repository identity and GitHub origin resolution for PR workflows.

use crate::db;

use crate::services::github::{self, GitHubClient, PullRequest};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) struct RepoInfo {
    pub(crate) branch: String,
    pub(crate) remote_url: Option<String>,
    /// `(owner, repo)` parsed from `remote_url`. `None` when the origin
    /// isn't a GitHub URL (e.g. GitLab) or no origin is configured.
    /// Cached here so [`resolve_open_pr`] and the existing `owner_repo`
    /// call sites don't reparse on every call.
    pub(crate) owner_repo: Option<(String, String)>,
    /// Names of every configured remote. A `base_ref` like `origin/main` is
    /// only unambiguous because we know `origin` is a remote and not the
    /// first segment of a branch named `feature/…` — see
    /// [`local_base_branch`](crate::services::github::pr_workflow::local_base_branch).
    pub(crate) remotes: Vec<String>,
}

impl RepoInfo {
    pub(crate) fn owner_repo(&self) -> Result<(String, String), String> {
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
pub(crate) fn resolve_open_pr(
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
pub(crate) fn repo_info(path: &str) -> Result<RepoInfo, String> {
    let identity = crate::git::remote::repo_identity(path)?;
    let owner_repo = identity
        .remote_url
        .as_deref()
        .and_then(github::parse_owner_repo);
    Ok(RepoInfo {
        branch: identity.branch,
        remote_url: identity.remote_url,
        owner_repo,
        remotes: identity.remotes,
    })
}

/// Resolve a Mesh's GitHub origin for actions that require one.
/// Unlike feed queries, a missing or non-GitHub origin is an error here.
pub(crate) fn resolve_github_owner_repo(
    mesh: &crate::models::Mesh,
) -> Result<(String, String), String> {
    let info = repo_info(&mesh.path)?;
    info.owner_repo.clone().ok_or_else(|| {
        if info.remote_url.is_some() {
            format!(
                "Mesh at {} has an `origin` remote, but it isn't a GitHub URL",
                mesh.path
            )
        } else {
            format!("Mesh at {} has no `origin` remote", mesh.path)
        }
    })
}

/// Resolve owner/repo from a path, returning None if no origin remote.
pub(crate) fn resolve_owner_repo(path: &str) -> Result<Option<(String, String)>, String> {
    let url = crate::git::remote::remote_url(path, "origin")?;
    Ok(url.and_then(|url| github::parse_owner_repo(&url)))
}

/// Derive the `https://github.com/{owner}/{repo}` web URL for a local
/// repo path. Returns `Ok(None)` when the path isn't a repo, has no
/// `origin` remote, or the remote isn't a `github.com` URL — these are
/// all the same outcome for the consumer (no GitHub link to show), so
/// collapsing them keeps the IPC surface simple. `Err(_)` is reserved
/// for actual git/libgit2 failures that should bubble up.
///
/// Pure helper extracted from `get_github_url_for_mesh` so the wire
/// command is one line and the path → URL derivation is unit-testable
/// against a `tempdir` without standing up a DB or Tauri runtime.
pub(crate) fn github_url_for_path(path: &str) -> Result<Option<String>, String> {
    Ok(resolve_owner_repo(path)?
        .map(|(owner, repo)| format!("https://github.com/{}/{}", owner, repo)))
}

pub(crate) fn get_github_url_for_mesh(mesh_id: i64) -> Result<Option<String>, String> {
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    // Issue #2602: a mesh whose folder is gone fails this read forever. The
    // marker tells the frontend to stop asking.
    github_url_for_path(&mesh.path)
        .map_err(|e| crate::services::mesh_folder::mark_if_missing(&mesh.path, e))
}
