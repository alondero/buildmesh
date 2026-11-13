//! GitHub workflow via direct REST API calls (no `gh` CLI dependency)
//!
//! A thin adapter layer: each `#[command]` here validates its arguments,
//! calls one workflow in [`crate::services::github`], and maps the error to
//! a `String`. The workflows, the repository identity and the wire shapes
//! live in `services/github/` (issue #2236) — this module owns no policy of
//! its own.

pub(crate) use crate::services::github::feeds::{
    get_pr_files_blocking, get_pr_mergeability_blocking, get_prs_mergeability_blocking,
    get_repo_issues_blocking, get_repo_pulls_blocking,
};
pub use crate::services::github::pr_workflow::PrSourceError;
pub(crate) use crate::services::github::pr_workflow::{
    get_current_branch_blocking, get_open_pr_for_node_blocking, merge_pr_blocking,
};

pub(crate) use crate::services::github::repository::resolve_github_owner_repo;

use crate::services::github::wire::{
    CreatePrResult, GitHubIssueFeed, GitHubPullRequestFeed, OpenPr, PrFileFeed, PrMergeability,
    PrMergeabilityEntry,
};
use tauri::command;

/// Get open GitHub issues for a mesh.
/// Returns an empty list when a readable mesh has no GitHub remote.
/// Repository failures propagate so an inaccessible mesh cannot masquerade
/// as a repository with no issues.
#[command]
pub async fn get_repo_issues(mesh_id: i64) -> Result<GitHubIssueFeed, String> {
    crate::commands::run_blocking("get_repo_issues", move || get_repo_issues_blocking(mesh_id))
        .await
}

/// Get pull requests for a mesh, filtered by `state` (`"open"` or `"closed"`).
/// Mirrors [`get_repo_issues`](crate::commands::pr::get_repo_issues): a readable mesh without a GitHub origin has
/// an empty feed; repository failures propagate to the panel.
///
/// Issue #1529: one cohesive summary query — list fields plus mergeability
/// ride inline via the GraphQL PR-summaries connection (O(pages), not O(PRs)).
/// The panel consumes this single call and never orchestrates per-row
/// enrichment.
///
/// One call returns ONE page of 100 PRs plus `next_cursor`; passing that
/// cursor back reads the next (older) page. The read used to walk every page
/// before answering, so a repo with a long closed history blocked the panel
/// on up to ten sequential GraphQL round trips. Callers that want the whole
/// list in one response (the mobile route) follow the cursor themselves.
#[command]
pub async fn get_repo_pulls(
    mesh_id: i64,
    state: String,
    cursor: Option<String>,
) -> Result<GitHubPullRequestFeed, String> {
    crate::commands::run_blocking("get_repo_pulls", move || {
        get_repo_pulls_blocking(mesh_id, state, cursor)
    })
    .await
}

/// Get a single PR's mergeability for a mesh's repo. The panel calls this once
/// per open PR after the list loads. `mergeable` is `null`/`None` while GitHub
/// computes the merge — surfaced as-is so the UI can show a "checking" state.
#[command]
pub async fn get_pr_mergeability(mesh_id: i64, pr_number: i64) -> Result<PrMergeability, String> {
    crate::commands::run_blocking("get_pr_mergeability", move || {
        get_pr_mergeability_blocking(mesh_id, pr_number)
    })
    .await
}

/// Get mergeability for a batch of PRs on a mesh's repo (issue #418,
/// reimplemented O(pages) for issue #1529).
///
/// Historical note: the original implementation resolved the client once and
/// then looped one REST detail request per PR (N HTTP requests). The desktop
/// panel no longer calls this endpoint — `get_repo_pulls` now returns
/// mergeability inline via the GraphQL summaries connection — but the command
/// survives for backward compat (mobile/older frontends). Its HTTP cost is
/// now O(pages): one GraphQL connection request per page (currently one for
/// the 100-row cap), regardless of how many PR numbers were requested.
///
/// **Per-PR failure semantics (preserved).** A PR number absent from the
/// summaries connection (closed/merged while the list was stale, or a partial
/// page) does NOT fail the whole batch. It returns `mergeable: None` with
/// `mergeable_state: "error: PR #<n> not in summary results"` so callers keep
/// the row in their checking/error state rather than falsely claiming
/// conflicts. A whole-query transport failure (rate limit, network) still
/// propagates as `Err` so the caller can surface a retryable panel error.
#[command]
pub async fn get_prs_mergeability(
    mesh_id: i64,
    pr_numbers: Vec<i64>,
) -> Result<Vec<PrMergeabilityEntry>, String> {
    crate::commands::run_blocking("get_prs_mergeability", move || {
        get_prs_mergeability_blocking(mesh_id, pr_numbers)
    })
    .await
}

/// Get the files changed in a single pull request, for the "View changes"
/// button on the PR tab (issue #421). Returns the file list with each file's
/// unified-diff `patch`. The Center Diff Overlay parses the patch line-by-line
/// to colour +/−/context rows in place. Mirrors the other PR commands:
/// resolves owner/repo via the mesh's `origin` remote, hits the GitHub
/// REST API, and forwards the HTTP error verbatim.
///
/// The list is paginated (issue #2024 rank 6): a PR with more changed files
/// than one page holds arrives flagged in `completeness` rather than rendering
/// as a whole diff.
#[command]
pub async fn get_pr_files(mesh_id: i64, pr_number: i64) -> Result<PrFileFeed, String> {
    crate::commands::run_blocking("get_pr_files", move || {
        get_pr_files_blocking(mesh_id, pr_number)
    })
    .await
}

// ----- node-scoped create-PR (issue #2024 rank 4, issue #1567) ----------
//
// The mobile Create-PR flow used to post a mesh id only, and the route
// resolved `mesh.path`. For a mesh whose root checkout sits on `main`, that
// published a `main -> main` PR — or, when the root sat on some unrelated
// feature branch, published *that* branch instead of the agent's. The
// Changes screen already displayed the node's branch, so the sheet and the
// PR it produced disagreed.
//
// The fix resolves the source from the NODE's worktree
// (`env::node_git_path`) and derives the base from the mesh's own
// `base_ref` rather than assuming `main`.
//
// This is the only create-PR path. The mesh-scoped and session-scoped
// commands that derived their source from the project root were removed
// with no remaining callers (issue #2236).

/// Create a PR from an agent node's worktree (issue #1567).
///
/// `base_branch` is optional: when the client does not pin one, the mesh's
/// `base_ref` decides it. `expected_head` lets the client assert the branch
/// it displayed is still the branch on disk, so a worktree that moved
/// between preview and submit fails loudly instead of publishing something
/// the user never saw.
#[command]
pub async fn create_pr_for_node_source(
    mesh_id: i64,
    node_id: i64,
    title: String,
    body: String,
    base_branch: Option<String>,
    expected_head: Option<String>,
) -> Result<CreatePrResult, String> {
    // Tauri commands surface a plain string; the HTTP route keeps the
    // typed `PrSourceError` so it can pick a status code.
    crate::services::github::pr_workflow::create_pr_for_node_source(
        mesh_id,
        node_id,
        title,
        body,
        base_branch,
        expected_head,
    )
    .await
    .map_err(|e: PrSourceError| e.to_string())
}

/// Merge a PR with the caller-chosen strategy + delete the branch.
/// Accepts a full GitHub PR URL like `https://github.com/owner/repo/pull/123`.
///
/// `merge_method` is GitHub's REST vocabulary: `"squash"`, `"merge"`, or
/// `"rebase"`. An absent or unrecognised value falls back to `"squash"`
/// (the historical behaviour) so older clients — the PrPill menu, the
/// mobile HTTP route — keep working unchanged; the panel's dropdown is
/// the only caller that names a method explicitly.
#[command]
pub async fn merge_pr(pr_url: String, merge_method: Option<String>) -> Result<String, String> {
    crate::commands::run_blocking("merge_pr", move || merge_pr_blocking(pr_url, merge_method)).await
}

// PR-spawn commands live in `commands/agent.rs` next to `create_issue_node`.

/// Get the current branch for a node.
///
/// Thin async wrapper; see [`crate::commands::git::get_git_branch_status`]
/// for the offload rationale. `repo_info` (a libgit2 walk over the working
/// tree) can take hundreds of ms on a large repo and must not park a Tauri
/// tokio worker.
#[command]
pub async fn get_current_branch(session_id: i64) -> Result<String, String> {
    crate::commands::run_blocking("get_current_branch", move || {
        get_current_branch_blocking(session_id)
    })
    .await
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
#[command]
pub async fn get_open_pr_for_node(node_id: i64) -> Result<Option<OpenPr>, String> {
    crate::commands::run_blocking("get_open_pr_for_node", move || {
        get_open_pr_for_node_blocking(node_id)
    })
    .await
}

/// Return the `https://github.com/{owner}/{repo}` URL for a mesh's
/// `origin` remote, or `None` if the origin isn't a GitHub URL (or the
/// mesh has no origin at all). Thin wrapper around
/// [`github_url_for_path`](crate::services::github::repository::github_url_for_path) for the contexts that only need the web URL
/// (mesh context menu, probe header GitHub buttons) and would otherwise
/// pay for the GitHubClient construction that the other `commands::pr`
/// functions do.
///
/// Sync `#[command]` (not `async`) — `github_url_for_path` only does
/// local git2 work, so the bounded tokio worker pool doesn't need to
/// carry this. Matches the lesson in
/// `[[buildmesh-overnight-freeze-reqwest-no-timeout]]`: keep
/// synchronous local work off the async pool. If a future call site
/// needs to hit the GitHub HTTP API for the URL (e.g. resolving a
/// redirect to a renamed repo), the new path must go through
/// `GitHubClient` with a `reqwest::Client` that has a real timeout.
#[command]
pub fn get_github_url_for_mesh(mesh_id: i64) -> Result<Option<String>, String> {
    crate::services::github::repository::get_github_url_for_mesh(mesh_id)
}
