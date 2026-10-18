//! GitHub workflow via direct REST API calls (no `gh` CLI dependency)

use crate::db;
use crate::env;
use crate::models::SessionStatus;
use crate::services::github::{
    self, CreatePrRequest, GitHubClient, GitHubError, GitHubPageCompleteness, PullRequest,
};
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

/// A feed we never had to read: the mesh has no GitHub remote, so there is
/// nothing that *could* be incomplete. Reported complete so the panel keeps
/// its existing "no issues" empty state rather than showing a truncation
/// warning about a read that never happened.
fn empty_feed_completeness() -> GitHubPageCompleteness {
    GitHubPageCompleteness {
        returned: 0,
        pages_fetched: 0,
        complete: true,
        incomplete_reason: None,
        reported_total: None,
    }
}

/// Sync core for [`get_repo_issues`]. Kept as a plain fn so the mobile HTTP
/// route (`http::routes::issues`) can call it directly; the Tauri command
/// wraps it in `spawn_blocking` (see [`crate::commands::run_blocking`]).
pub(crate) fn get_repo_issues_blocking(mesh_id: i64) -> Result<GitHubIssueFeed, String> {
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;

    let Some((owner, repo)) = resolve_owner_repo(&mesh.path)? else {
        return Ok(GitHubIssueFeed {
            items: Vec::new(),
            completeness: empty_feed_completeness(),
        });
    };

    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    let page = client
        .list_issues_only_paged(&owner, &repo)
        .map_err(|e| e.to_string())?;

    let items = page
        .items
        .into_iter()
        .map(|issue| {
            // Extract `blocked_by` BEFORE moving `issue.body` into the struct
            // literal (Rust's move checker rejects the borrow-after-move).
            // The parser is pure and bounded — see its doc comment.
            let blocked_by: Vec<i32> = github::parse_blocked_by(&issue.body)
                .into_iter()
                .map(|n| n as i32)
                .collect();
            GitHubIssue {
                number: issue.number,
                title: issue.title,
                body: issue.body,
                url: issue.html_url,
                state: issue.state,
                labels: issue.labels,
                author: issue.author,
                // Downcast internal `i64` → wire `i32` (issue numbers fit
                // comfortably in i32's ~2.1B max; matches the existing
                // `#[ts(as = "i32")]` convention on the wire struct's other
                // integer fields).
                blocked_by,
            }
        })
        .collect();

    Ok(GitHubIssueFeed {
        items,
        completeness: page.completeness,
    })
}

/// Get pull requests for a mesh, filtered by `state` (`"open"` or `"closed"`).
/// Mirrors [`get_repo_issues`]: a readable mesh without a GitHub origin has
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

/// Sync core for [`get_repo_pulls`] — see [`get_repo_issues_blocking`] for the
/// split rationale.
pub(crate) fn get_repo_pulls_blocking(
    mesh_id: i64,
    state: String,
) -> Result<GitHubPullRequestFeed, String> {
    // Only ever forward a known filter to GitHub; anything unexpected falls
    // back to "open" rather than letting an arbitrary string reach the API.
    let state = if state == "closed" { "closed" } else { "open" };

    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;

    let Some((owner, repo)) = resolve_owner_repo(&mesh.path)? else {
        return Ok(GitHubPullRequestFeed {
            items: Vec::new(),
            completeness: empty_feed_completeness(),
        });
    };

    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    let page = client
        .list_pr_summaries_paged(&owner, &repo, state)
        .map_err(|e| e.to_string())?;

    let items = page
        .items
        .into_iter()
        .map(|pr| GitHubPullRequest {
            number: pr.number,
            title: pr.title,
            body: pr.body,
            url: pr.html_url,
            state: pr.state,
            draft: pr.draft,
            head_ref: pr.head_ref,
            head_repo_owner: pr.head_repo_owner,
            head_repo_clone_url: pr.head_repo_clone_url,
            head_sha: pr.head_sha,
            author: pr.author,
            mergeable: pr.mergeable,
            mergeable_state: pr.mergeable_state,
            checks_state: pr.checks_state,
            review_decision: pr.review_decision,
        })
        .collect();

    Ok(GitHubPullRequestFeed {
        items,
        completeness: page.completeness,
    })
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

/// Sync core for [`get_pr_mergeability`] — see [`get_repo_issues_blocking`].
pub(crate) fn get_pr_mergeability_blocking(
    mesh_id: i64,
    pr_number: i64,
) -> Result<PrMergeability, String> {
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let (owner, repo) = resolve_github_owner_repo(&mesh)?;

    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    let (mergeable, mergeable_state) = client
        .pull_request_mergeability(&owner, &repo, pr_number)
        .map_err(|e| e.to_string())?;

    Ok(PrMergeability {
        mergeable,
        mergeable_state,
    })
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

/// Sync core for [`get_prs_mergeability`] — see [`get_repo_issues_blocking`].
pub(crate) fn get_prs_mergeability_blocking(
    mesh_id: i64,
    pr_numbers: Vec<i64>,
) -> Result<Vec<PrMergeabilityEntry>, String> {
    // Short-circuit before client construction: an empty PR list is the
    // common case after a list reload that returned zero rows, and the
    // `GitHubClient::new()` → `resolve_token()` chain is non-trivial
    // (keyring fallback spawns `gh auth token`). Pin this so a future
    // refactor that drops the early return surfaces as a slow no-op,
    // not a silent behavioural change.
    if pr_numbers.is_empty() {
        return Ok(Vec::new());
    }

    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    // Mirrors `get_repo_pulls` / `get_repo_issues`: meshes whose `origin`
    // isn't a GitHub URL (GitLab, Bitbucket, self-hosted) get an empty
    // result with a `warn!` instead of a propagated error. The frontend
    // enrichment then leaves every PR row in "Checking…" — the panel
    // gracefully degrades rather than failing the batch on a non-GitHub
    // repo.
    let (owner, repo) = match resolve_github_owner_repo(&mesh) {
        Ok(pair) => pair,
        Err(reason) => {
            tracing::warn!("get_prs_mergeability: {} — returning empty result", reason);
            return Ok(Vec::new());
        }
    };

    // ONE token resolution, then cheapest-first lookups: open summaries
    // (O(pages)), closed summaries for stragglers, then one REST detail
    // request per STILL-missing PR (e.g. older than the 100-row summary
    // cap). A missing entry never fails the batch — see
    // [`mergeability_entries`] for the per-PR sentinel.
    let client = GitHubClient::new().map_err(|e| e.to_string())?;

    mergeability_from_summaries(&client, &owner, &repo, pr_numbers).map_err(|e| e.to_string())
}

/// Resolve a batch of PR numbers to mergeability entries without a DB or
/// mesh lookup, so the lookup strategy is unit-testable against a fake
/// server (issue #1529): open summaries first, closed summaries for numbers
/// still missing, then the single-PR REST detail endpoint per remaining
/// number (correct past the 100-row summary cap, where a summaries-only
/// lookup would falsely report "not found"). Whole-query transport failures
/// propagate as `Err`; per-PR detail failures become the `"error: ..."`
/// sentinel via [`mergeability_entries`].
pub(crate) fn mergeability_from_summaries(
    client: &GitHubClient,
    owner: &str,
    repo: &str,
    pr_numbers: Vec<i64>,
) -> Result<Vec<PrMergeabilityEntry>, GitHubError> {
    let mut by_number: std::collections::HashMap<i64, (Option<bool>, String)> =
        std::collections::HashMap::new();
    for s in client.list_pr_summaries(owner, repo, "open")? {
        by_number.insert(s.number, (s.mergeable, s.mergeable_state));
    }
    let mut missing: Vec<i64> = pr_numbers
        .iter()
        .copied()
        .filter(|n| !by_number.contains_key(n))
        .collect();
    if !missing.is_empty() {
        match client.list_pr_summaries(owner, repo, "closed") {
            Ok(closed) => {
                for s in closed {
                    by_number.insert(s.number, (s.mergeable, s.mergeable_state));
                }
                missing.retain(|n| !by_number.contains_key(n));
            }
            Err(e) => {
                tracing::warn!(
                    "get_prs_mergeability: closed-summaries fetch failed: {} — falling back to per-PR detail",
                    e
                );
            }
        }
    }

    // Whatever is still missing (older than the summary cap, or a partial
    // page) gets one direct detail request each — the pre-#1529 endpoint,
    // now only a fallback rather than the loop. Failures stay per-PR.
    if !missing.is_empty() {
        for entry in mergeability_entries(missing, |n| {
            client.pull_request_mergeability(owner, repo, n)
        }) {
            by_number.insert(entry.number, (entry.mergeable, entry.mergeable_state));
        }
    }

    // Request order out: every requested number resolves through the map
    // now (summaries hit or detail fallback/sentinel), so a missing key
    // here is unreachable — the debug_assert documents the invariant for
    // test builds without changing release behaviour. `.get`, not
    // `.remove`: a caller passing a duplicate number must resolve it twice,
    // not trip the assert on the second occurrence.
    Ok(pr_numbers
        .into_iter()
        .map(|n| match by_number.get(&n) {
            Some((mergeable, mergeable_state)) => PrMergeabilityEntry {
                number: n,
                mergeable: *mergeable,
                mergeable_state: mergeable_state.clone(),
            },
            None => {
                debug_assert!(false, "mergeability map must cover every requested PR");
                PrMergeabilityEntry {
                    number: n,
                    mergeable: None,
                    mergeable_state: format!("error: PR #{} has no mergeability result", n),
                }
            }
        })
        .collect())
}

/// Pure per-PR iterator that maps a list of PR numbers onto
/// `PrMergeabilityEntry` values, threading each through a probe closure.
///
/// Since #1529 this is the per-PR REST-detail fallback inside
/// [`mergeability_from_summaries`] (numbers older than the summary cap),
/// not the loop it used to be; its unit tests pin the per-PR
/// error-sentinel mapping the fallback preserves.
/// The closure indirection is the test seam: without a trait on
/// `GitHubClient`, a unit test can't easily stub
/// `pull_request_mergeability`. The closure accepts the per-PR probe
/// function as data, so a test passes its own closure and asserts on
/// the helper's mapping logic in isolation.
fn mergeability_entries<F>(pr_numbers: Vec<i64>, probe: F) -> Vec<PrMergeabilityEntry>
where
    F: Fn(i64) -> Result<(Option<bool>, String), GitHubError>,
{
    pr_numbers
        .into_iter()
        .map(|n| match probe(n) {
            Ok((mergeable, mergeable_state)) => PrMergeabilityEntry {
                number: n,
                mergeable,
                mergeable_state,
            },
            Err(e) => {
                tracing::warn!(
                    "get_prs_mergeability: PR #{} failed: {} — row will stay in 'Checking…' until next reload",
                    n, e
                );
                PrMergeabilityEntry {
                    number: n,
                    mergeable: None,
                    mergeable_state: format!("error: {}", e),
                }
            }
        })
        .collect()
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

/// Sync core for [`get_pr_files`] — see [`get_repo_issues_blocking`].
pub(crate) fn get_pr_files_blocking(mesh_id: i64, pr_number: i64) -> Result<PrFileFeed, String> {
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let (owner, repo) = resolve_github_owner_repo(&mesh)?;

    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    let page = client
        .list_pr_files_paged(&owner, &repo, pr_number)
        .map_err(|e| e.to_string())?;

    let items = page
        .items
        .into_iter()
        .map(|f| PrFileEntry {
            filename: f.filename,
            status: f.status,
            additions: f.additions,
            deletions: f.deletions,
            patch: f.patch,
            previous_filename: f.previous_filename,
        })
        .collect();

    Ok(PrFileFeed {
        items,
        completeness: page.completeness,
    })
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

/// Sync core for [`create_pr`] — see [`get_repo_issues_blocking`].
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

/// Sync core for [`create_pr_for_mesh`] — see [`get_repo_issues_blocking`].
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

/// HTTP-facing twin of [`create_pr_for_node_source`].
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

/// Sync core for [`create_pr_for_node_source`] — see
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

/// Sync core for [`merge_pr`] — see [`get_repo_issues_blocking`].
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

/// Sync core for [`get_current_branch`].
pub(crate) fn get_current_branch_blocking(session_id: i64) -> Result<String, String> {
    let node = db::get_agent_node_by_id(session_id).map_err(|e| e.to_string())?;

    // Route through the worktree (if any) — the mesh root is on the base
    // branch for worktree nodes. See `node_git_path` for the rationale.
    Ok(repo_info(&env::node_git_path(&node).host_path)?.branch)
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

/// Sync core for [`get_open_pr_for_node`] — see [`get_repo_issues_blocking`].
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
    /// Names of every configured remote. A `base_ref` like `origin/main` is
    /// only unambiguous because we know `origin` is a remote and not the
    /// first segment of a branch named `feature/…` — see
    /// [`local_base_branch`].
    remotes: Vec<String>,
}

/// Return the URL for a configured remote, preserving the optional-metadata
/// behavior used by repo information and GitHub-link callers.
fn configured_remote_url(repo: &Repository, name: &str) -> Option<String> {
    let remote = match repo.find_remote(name) {
        Ok(remote) => remote,
        Err(error) if error.code() == git2::ErrorCode::NotFound => return None,
        Err(_) => {
            tracing::warn!(
                remote = name,
                "Could not read Git remote; treating it as unavailable"
            );
            return None;
        }
    };

    match remote.url() {
        Ok(url) => Some(url.to_string()),
        Err(_) => {
            tracing::warn!(
                remote = name,
                "Could not read Git remote URL; treating it as unavailable"
            );
            None
        }
    }
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
fn resolve_open_pr(info: &RepoInfo, client: &GitHubClient) -> Result<Option<PullRequest>, String> {
    if info.branch.is_empty() {
        return Ok(None);
    }
    let (owner, repo) = info.owner_repo()?;
    client
        .find_open_pr_for_branch(&owner, &repo, &info.branch)
        .map_err(|e| e.to_string())
}

fn safe_directory_command(host_path: &str, windows: bool) -> String {
    let quoted = if windows {
        // PowerShell accepts smart apostrophes as string delimiters too, so the
        // shared literal is the only escaping that survives such a path.
        env::powershell_literal(&host_path.replace('\\', "/"))
    } else {
        format!("'{}'", host_path.replace('\'', "'\\''"))
    };
    format!("git config --global --add safe.directory {quoted}")
}

fn open_github_repo(path: &str) -> Result<Repository, String> {
    crate::git::primitives::open_from_host_path(path).map_err(|error| {
        if error.code() == git2::ErrorCode::Owner {
            let command = safe_directory_command(&env::to_host_path(path), cfg!(windows));
            let runtime = if cfg!(windows) { "Windows" } else { "host" };
            format!("git error: {error}. If you trust this repository, add its exact path to {runtime} Git configuration: {command}")
        } else {
            format!("git error: {error}")
        }
    })
}

/// Open the repo once and extract both the current branch and origin URL.
fn repo_info(path: &str) -> Result<RepoInfo, String> {
    let repo = open_github_repo(path)?;

    let branch = match repo.head() {
        Ok(head) => {
            if head.is_branch() {
                match head.shorthand() {
                    Ok(name) => name.to_string(),
                    Err(_) => {
                        tracing::warn!(
                            "Could not read Git branch name; treating it as unavailable"
                        );
                        String::new()
                    }
                }
            } else {
                head.target()
                    .map(|oid| oid.to_string()[..8].to_string())
                    .unwrap_or_default()
            }
        }
        Err(_) => String::new(),
    };

    let remote_url = configured_remote_url(&repo, "origin");
    let owner_repo = remote_url.as_deref().and_then(github::parse_owner_repo);
    let remotes = match repo.remotes() {
        Ok(names) => (0..names.len())
            .filter_map(|index| match names.get(index) {
                Ok(Some(name)) => Some(name.to_string()),
                Ok(None) => None,
                Err(_) => {
                    tracing::warn!(index, "Could not read a Git remote name; omitting it");
                    None
                }
            })
            .collect(),
        Err(_) => {
            tracing::warn!("Could not list Git remotes; returning an empty remote list");
            Vec::new()
        }
    };

    Ok(RepoInfo {
        branch,
        remote_url,
        owner_repo,
        remotes,
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
    let repo = open_github_repo(path)?;
    let url = configured_remote_url(&repo, "origin");
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

/// Return the `https://github.com/{owner}/{repo}` URL for a mesh's
/// `origin` remote, or `None` if the origin isn't a GitHub URL (or the
/// mesh has no origin at all). Thin wrapper around
/// [`github_url_for_path`] for the contexts that only need the web URL
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
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    github_url_for_path(&mesh.path)
}

/// Parse a GitHub PR URL into (owner, repo, pr_number).
/// Validate the merge-method vocabulary at the wire seam. An absent or
/// unrecognised value falls back to `"squash"` (the historical
/// behaviour) so older clients keep working; the panel's dropdown sends
/// one of the three literals GitHub's REST merge endpoint understands.
fn normalise_merge_method(merge_method: Option<String>) -> String {
    merge_method
        .filter(|m| matches!(m.as_str(), "squash" | "merge" | "rebase"))
        .unwrap_or_else(|| "squash".to_string())
}

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
#[path = "pr_tests.rs"]
mod tests;
