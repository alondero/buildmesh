//! GitHub workflow via direct REST API calls (no `gh` CLI dependency)

pub(crate) use crate::services::github::feeds::{
    get_pr_files_blocking, get_pr_mergeability_blocking, get_prs_mergeability_blocking,
    get_repo_issues_blocking, get_repo_issues_filtered_blocking, get_repo_pulls_blocking,
};
pub use crate::services::github::pr_workflow::PrSourceError;
pub(crate) use crate::services::github::pr_workflow::{
    create_pr_blocking_with_client, create_pr_for_mesh_blocking_with_client,
    create_pr_for_node_source_blocking_with_client, get_current_branch_blocking,
    get_open_pr_for_node_blocking, merge_pr_blocking, resolve_pr_source_for_node,
};

pub(crate) use crate::services::github::repository::resolve_github_owner_repo;

use crate::services::github::{GitHubClient, GitHubPageCompleteness};
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
    /// `"open"` or `"closed"`. The Issues Probe requests open issues;
    /// Companion may request either state or both.
    pub state: String,
    /// Label names (flattened from the GitHub API's `[{name, color, ...}]`).
    /// Empty array when the issue has no labels.
    pub labels: Vec<String>,
    /// Issue numbers extracted from the body's `**Blocked by**` section
    /// (issue #481 follow-up). Parsed by `services::github::parse_blocked_by`
    /// in `get_repo_issues`'s mapper — see that fn for the parser contract.
    ///
    /// The Issues Probe renders a red flag below the Spawn button when at
    /// least one of these numbers is still in the loaded open-issues list
    /// (i.e. not yet completed). Cross-reference is frontend-side, so
    /// blockers from a different repo or behind pagination (>100 open
    /// issues) won't trigger the flag — a known limitation documented
    /// in the plan.
    ///
    /// `Vec<i32>` on the wire (matches the existing `#[ts(as = "i32")]`
    /// convention on `number`, `additions`, etc.). The internal
    /// `services::github::Issue` keeps `Vec<i64>` for the GitHub API's
    /// native integer width; the mapper downcasts via `n as i32`.
    /// `#[serde(default)]` keeps the field additive across rolling
    /// deploys — a missing key parses to `vec![]`.
    #[serde(default)]
    pub blocked_by: Vec<i32>,
    /// GitHub login of the issue's author (`user.login`). Drives the
    /// contributor pill on the Issues probe row — clicking it opens
    /// `https://github.com/<login>`. `#[serde(default)]` keeps the field
    /// additive across rolling deploys; `services::github::Issue` already
    /// defaults a missing `user` to `\"\"`, and the pill simply doesn't
    /// render for an empty author.
    #[serde(default)]
    pub author: String,
}

/// Wire shape of `get_repo_pulls` (desktop Tauri) — one entry per pull request.
/// Generated to TS via ts-rs (issue #359); `i64` carries `#[ts(as = "i32")]`
/// because serde_json emits it as a JS number, not the `bigint` ts-rs defaults
/// to.
///
/// Issue #1529: mergeability rides inline on this struct via the GraphQL
/// PR-summaries connection (one request per page, not one per PR). The panel
/// consumes this single cohesive query and never orchestrates per-row
/// enrichment calls.
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
    /// PR's source-branch ref name (e.g. `"feature/some-thing"`). Captured from
    /// GitHub's `head.ref` so the spawn button (#420) can pass it to the
    /// backend, which fetches it and uses it as the worktree's `base_ref`.
    /// Empty when the API response is a partial shape or the head ref is
    /// unknown — the spawn path treats empty as a non-forkable case and the
    /// panel surfaces a clear error.
    #[serde(default)]
    pub head_ref: String,
    /// Owner login of the PR's head repo (e.g. `"alice"` for a fork PR opened
    /// from `alice/buildmesh`). Captured from `head.repo.owner.login`. For
    /// same-repo PRs the head's repo IS the destination repo, so the value
    /// matches the destination owner. Stage-2 spawn (`spawn_agent_inner`,
    /// issue #443) keys the fork-spawn decision on whether this field is
    /// `Some` — populated means the PR is from a fork and gets a `fork-<login>`
    /// remote; empty means the same-repo path. Empty when the field is missing.
    #[serde(default)]
    pub head_repo_owner: String,
    /// HTTPS clone URL of the PR's head repo (e.g.
    /// `"https://github.com/alice/buildmesh.git"`). Captured from
    /// `head.repo.clone_url`. Paired with [`head_repo_owner`](Self::head_repo_owner)
    /// — the spawn path uses both to register the fork as a remote and fetch
    /// the head ref from it. Empty when the field is missing.
    #[serde(default)]
    pub head_repo_clone_url: String,
    /// PR's head commit SHA (e.g. `"0123456789abcdef..."`). Mirrors
    /// `services::github::PullRequest::head_sha` and is the exact-pinning
    /// handle introduced in issue #444: the spawn path persists it on the
    /// new agent node and verifies the local `origin/<head_ref>` SHA matches
    /// it after `git fetch`. Empty on partial responses and some fork-PR
    /// payloads — the spawn path treats empty as "skip drift check" rather
    /// than failing, matching the existing `pr_head_unfetchable` fallback
    /// semantics.
    #[serde(default)]
    pub head_sha: String,
    /// GitHub login of the PR's author — the contributor pill on the PRs
    /// probe row links to `https://github.com/<login>`. Sourced from the
    /// GraphQL summaries connection's `author { login }` (same request as
    /// every other list field — no extra enrichment call).
    /// `#[serde(default)]` for old cached payloads.
    #[serde(default)]
    pub author: String,
    /// Mergeability inline (issue #1529): `Some(true)` mergeable,
    /// `Some(false)` conflicts, `None` while GitHub is still computing
    /// (`UNKNOWN`) — mirrors the old `PrMergeability.mergeable` contract so
    /// the panel's checking/unknown wording is preserved without a second
    /// request. `#[serde(default)]` keeps old cached payloads parsing.
    #[serde(default)]
    pub mergeable: Option<bool>,
    /// Lowercase REST vocabulary (`clean`, `dirty`, `blocked`, `behind`,
    /// `unstable`, `unknown`, …) mapped from GraphQL `mergeStateStatus`.
    /// Used for the flag wording. `#[serde(default)]` for old payloads.
    #[serde(default)]
    pub mergeable_state: String,
    /// Combined check/status state on the head commit; absent means not reported.
    #[serde(default)]
    pub checks_state: Option<String>,
    /// GitHub review decision; absent means no decision was reported.
    #[serde(default)]
    pub review_decision: Option<String>,
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

/// Wire shape of `get_prs_mergeability` (issue #418) — the batched enrichment
/// the panel requests after the list loads. Mirrors `PrMergeability` plus a
/// PR number so the frontend can key entries back onto the listed PRs. The
/// per-PR `number` round-trips through the wire so a mobile client (or a
/// future desktop caller that filters PRs out of band) doesn't need a
/// separate index lookup.
///
/// Distinct from `PrMergeability` (used by the still-supported
/// per-PR `get_pr_mergeability` command) so the batched entry carries the
/// PR number without forcing a non-nullable field onto the per-PR shape.
/// `#[ts(as = "i32")]` on `number` matches the convention on the sibling
/// wire types (`GitHubPullRequest`, `GitHubIssue`) so serde_json emits it
/// as a JS number, not the `bigint` ts-rs defaults to.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "PrMergeabilityEntry.ts")]
pub struct PrMergeabilityEntry {
    /// PR number this entry corresponds to. `i64` carries `#[ts(as = "i32")]`
    /// (same convention as `GitHubPullRequest.number`).
    #[ts(as = "i32")]
    pub number: i64,
    /// `Some(true)` mergeable, `Some(false)` conflicts, `None` still
    /// computing or this PR's individual probe failed (see
    /// `get_prs_mergeability`'s per-PR fallback — `mergeable_state` then
    /// carries `"error: <reason>"` so the panel can still surface a
    /// "Checking…" state instead of falsely claiming conflicts).
    pub mergeable: Option<bool>,
    /// GitHub's `mergeable_state` (`clean`, `dirty`, `blocked`, `behind`,
    /// `unstable`, `unknown`, …) for success entries, or `"error: <reason>"`
    /// when the individual PR probe failed (the panel renders both as
    /// "Checking…"; the next list reload retries from scratch).
    pub mergeable_state: String,
}

/// One file in a pull request — wire shape of `get_pr_files` (issue #421).
/// Mirrors `services::github::prs::PrFile`; the panel's Center Diff Overlay
/// (`source: 'pr'`) renders the `patch` text line-by-line rather than
/// reconstructing our own hunk structure (GitHub's patches are non-standard —
/// missing context lines, inline `rename from`/`rename to` — so a structural
/// round-trip would be brittle). The frontend imports the generated type
/// from `src/types/generated/PrFileEntry.ts`; never hand-mirror (issue #359).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "PrFileEntry.ts")]
pub struct PrFileEntry {
    /// Number of lines added (per GitHub). `#[ts(as = "i32")]` because
    /// serde_json emits `i64` as a JS number, not the `bigint` ts-rs defaults
    /// to (matches the convention in `GitHubPullRequest` and friends).
    #[ts(as = "i32")]
    pub additions: i64,
    #[ts(as = "i32")]
    pub deletions: i64,
    /// Path of the file at the head of the PR.
    pub filename: String,
    /// Unified diff text from GitHub. Empty for binary files (GitHub omits
    /// `patch` for them).
    pub patch: String,
    /// Old path for renames; null for everything else.
    pub previous_filename: Option<String>,
    /// `"added" | "modified" | "deleted" | "renamed" | …` — GitHub's
    /// vocabulary, used by the overlay to colour the file card badge.
    pub status: String,
}

/// Get open GitHub issues for a mesh.
/// Returns an empty list when a readable mesh has no GitHub remote.
/// Repository failures propagate so an inaccessible mesh cannot masquerade
/// as a repository with no issues.
#[command]
pub async fn get_repo_issues(mesh_id: i64) -> Result<GitHubIssueFeed, String> {
    crate::commands::run_blocking("get_repo_issues", move || get_repo_issues_blocking(mesh_id))
        .await
}

/// An issue list plus whether GitHub gave us all of it (issue #2024 rank 6).
///
/// A bare `Vec` made "page 1" and "everything" indistinguishable, so the panel
/// presented a 100-row read as the repository's issues. `completeness` rides
/// alongside so the UI can say "showing first N of M" instead of implying
/// completeness it cannot prove.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "GitHubIssueFeed.ts")]
pub struct GitHubIssueFeed {
    pub items: Vec<GitHubIssue>,
    pub completeness: GitHubPageCompleteness,
}

/// A pull-request list plus its completeness — see [`GitHubIssueFeed`].
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "GitHubPullRequestFeed.ts")]
pub struct GitHubPullRequestFeed {
    pub items: Vec<GitHubPullRequest>,
    pub completeness: GitHubPageCompleteness,
}

/// A PR file list plus its completeness — see [`GitHubIssueFeed`]. A PR with
/// more changed files than one page holds must not render as a whole diff.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "PrFileFeed.ts")]
pub struct PrFileFeed {
    pub items: Vec<PrFileEntry>,
    pub completeness: GitHubPageCompleteness,
}

/// Get pull requests for a mesh, filtered by `state` (`"open"` or `"closed"`).
/// Mirrors [`get_repo_issues`](crate::commands::pr::get_repo_issues): a readable mesh without a GitHub origin has
/// an empty feed; repository failures propagate to the panel.
///
/// Issue #1529: one cohesive summary query — list fields plus mergeability
/// ride inline via the GraphQL PR-summaries connection (O(pages), not O(PRs)).
/// The panel consumes this single call and never orchestrates per-row
/// enrichment.
#[command]
pub async fn get_repo_pulls(mesh_id: i64, state: String) -> Result<GitHubPullRequestFeed, String> {
    crate::commands::run_blocking("get_repo_pulls", move || {
        get_repo_pulls_blocking(mesh_id, state)
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

/// Create a PR for the node
#[command]
pub async fn create_pr(session_id: i64, title: String, body: String) -> Result<String, String> {
    crate::commands::run_blocking("create_pr", move || {
        let client = GitHubClient::new().map_err(|e| e.to_string())?;
        create_pr_blocking_with_client(&client, session_id, &title, &body)
    })
    .await
}

/// Create a PR directly from a mesh directory path (no node required).
/// Detects the current branch via git2, then creates a PR targeting `base_branch`.
#[command]
pub async fn create_pr_for_mesh(
    mesh_path: String,
    title: String,
    body: String,
    base_branch: String,
) -> Result<String, String> {
    crate::commands::run_blocking("create_pr_for_mesh", move || {
        let client = GitHubClient::new().map_err(|e| e.to_string())?;
        create_pr_for_mesh_blocking_with_client(&client, &mesh_path, &title, &body, &base_branch)
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

/// The source/target branch pair a create-PR request resolved to.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "PrSource.ts")]
pub struct PrSource {
    /// Branch read from the agent node's worktree — the PR's `head`.
    pub head_branch: String,
    /// Branch the PR targets — derived from the mesh's `base_ref`.
    pub base_branch: String,
}

/// Result of a successful create-PR.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "CreatePrResult.ts")]
pub struct CreatePrResult {
    pub url: String,
    /// Echo of the branches actually used, so the client can display what
    /// was created instead of what it guessed.
    pub head_branch: String,
    pub base_branch: String,
}

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
    crate::commands::run_blocking("create_pr_for_node_source", move || {
        let client = GitHubClient::new().map_err(|e| e.to_string())?;
        create_pr_for_node_source_blocking_with_client(
            &client,
            mesh_id,
            node_id,
            &title,
            &body,
            base_branch.as_deref(),
            expected_head.as_deref(),
        )
        // Tauri commands surface a plain string; the HTTP route keeps the
        // typed `PrSourceError` so it can pick a status code.
        .map_err(|e: PrSourceError| e.to_string())
    })
    .await
}

/// HTTP-facing twin of [`create_pr_for_node_source`](crate::commands::pr::create_pr_for_node_source).
///
/// Identical work, but it hands the caller the typed [`PrSourceError`] so the
/// route can answer 404 / 403 / 422 / 500 correctly. The `#[command]` version
/// flattens to `String` because Tauri surfaces command errors as plain text,
/// which is what loses the distinction — and a validation failure must never
/// be reported as a server fault (#2190 review).
pub async fn create_pr_for_node_source_http(
    mesh_id: i64,
    node_id: i64,
    title: String,
    body: String,
    base_branch: Option<String>,
    expected_head: Option<String>,
) -> Result<CreatePrResult, PrSourceError> {
    crate::commands::run_blocking_typed("create_pr_for_node_source", move || {
        let client = GitHubClient::new().map_err(|e| PrSourceError::Other(e.to_string()))?;
        create_pr_for_node_source_blocking_with_client(
            &client,
            mesh_id,
            node_id,
            &title,
            &body,
            base_branch.as_deref(),
            expected_head.as_deref(),
        )
    })
    .await
}

/// Merge a PR with the caller-chosen strategy + delete the branch.
/// Accepts a full GitHub PR URL like `https://github.com/owner/repo/pull/123`.
///
/// `merge_method` is GitHub's REST vocabulary: `\"squash\"`, `\"merge\"`, or
/// `\"rebase\"`. An absent or unrecognised value falls back to `\"squash\"`
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

/// Open PR summary for an agent node — shape matches the TS `OpenPr` type.
///
/// Generated to src/types/generated/OpenPr.ts (issue #404). `i64` carries
/// `#[ts(as = "i32")]` so it emits `number` (matches `GitHubIssue.number`).
///
/// The four `head_*` fields are the spawn inputs the PR pill's
/// "Spawn reviewer agent" path forwards to `create_pr_node` (the same values
/// the Pull Requests probe's `+` passes from `get_repo_pulls`). They ride
/// along on the already-fetched `PullRequest`, so the spawn needs no second
/// GitHub call. Empty strings keep their existing "unknown / skip" semantics
/// on the spawn path (see `validate_pr_spawn_inputs` and `head_sha`).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "OpenPr.ts")]
pub struct OpenPr {
    #[ts(as = "i32")]
    pub number: i64,
    pub url: String,
    pub title: String,
    pub draft: bool,
    /// PR's source-branch ref name (GitHub `head.ref`). Empty when unknown.
    pub head_ref: String,
    /// PR's head commit SHA (GitHub `head.sha`) — the exact-pinning handle
    /// (issue #444). Empty when unknown, which skips the drift check.
    pub head_sha: String,
    /// Owner login of the PR's head repo (`head.repo.owner.login`). For
    /// same-repo PRs this is the destination owner; empty when unknown.
    pub head_repo_owner: String,
    /// Clone URL of the PR's head repo (`head.repo.clone_url`). Paired with
    /// `head_repo_owner` for fork PRs (issue #443); empty when unknown.
    pub head_repo_clone_url: String,
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

// ---------------------------------------------------------------------------
// Tests — focused on `repo_info` (the failure-prone git2/extraction half).
// The HTTP call to GitHub is exercised manually + via the `#[ignore]`-gated
// live test below; we don't pull in a wiremock dependency for one call site.
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "pr_tests.rs"]
mod tests;
