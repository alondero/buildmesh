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
    let info = repo_info(&env::node_working_path(&node).host_path)?;
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
// (`env::node_working_path`) and derives the base from the mesh's own
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
    let info = repo_info(&env::node_working_path(&node).host_path)?;
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
    // branch for worktree nodes. See `node_working_path` for the rationale.
    Ok(repo_info(&env::node_working_path(&node).host_path)?.branch)
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
        host_path.replace('\\', "/").replace('\'', "''")
    } else {
        host_path.replace('\'', "'\\''")
    };
    format!("git config --global --add safe.directory '{quoted}'")
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
                head.shorthand().unwrap_or("").to_string()
            } else {
                head.target()
                    .map(|oid| oid.to_string()[..8].to_string())
                    .unwrap_or_default()
            }
        }
        Err(_) => String::new(),
    };

    let remote_url = repo
        .find_remote("origin")
        .ok()
        .and_then(|r| r.url().map(|u| u.to_string()));
    let owner_repo = remote_url.as_deref().and_then(github::parse_owner_repo);
    let remotes = repo
        .remotes()
        .map(|names| names.iter().flatten().map(|n| n.to_string()).collect())
        .unwrap_or_default();

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
    let url = match repo.find_remote("origin") {
        Ok(remote) => remote.url().map(|u| u.to_string()),
        Err(_) => return Ok(None),
    };
    match url {
        Some(u) => Ok(github::parse_owner_repo(&u)),
        None => Ok(None),
    }
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
mod tests {
    use super::*;
    use crate::env::test_helpers::init_repo_with_commit as init_repo_for_test;
    use crate::git::worktree::create_git_worktree;
    use crate::models::AgentNode;
    use crate::services::github::tests::{fake_server, Scripted};
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn safe_directory_commands_preserve_shell_paths() {
        assert_eq!(
            safe_directory_command(r"C:\alice's repo", true),
            "git config --global --add safe.directory 'C:/alice''s repo'",
        );
        assert_eq!(
            safe_directory_command(r"/tmp/alice's\repo", false),
            r"git config --global --add safe.directory '/tmp/alice'\''s\repo'",
        );
    }

    #[test]
    fn github_feeds_report_unreadable_repository() {
        let _db = ensure_pr_blocking_db();
        let tmp = TempGitRepo::new();
        let mesh = db::create_mesh("unreadable-github-feed", tmp.path().to_str().unwrap()).unwrap();
        let issues = get_repo_issues_blocking(mesh.id);
        let pulls = get_repo_pulls_blocking(mesh.id, "open".to_string());
        db::delete_mesh(mesh.id).unwrap();
        assert!(issues.unwrap_err().contains("git error:"));
        assert!(pulls.unwrap_err().contains("git error:"));
    }

    #[test]
    fn github_feeds_without_github_origin_remain_empty() {
        let _db = ensure_pr_blocking_db();
        for origin in [None, Some("https://gitlab.com/example/repo.git")] {
            let (tmp, path) = init_repo_with_commit();
            if let Some(url) = origin {
                Repository::open(&path)
                    .unwrap()
                    .remote("origin", url)
                    .unwrap();
            }
            let mesh = db::create_mesh("non-github-feed", &path).unwrap();
            let issues = get_repo_issues_blocking(mesh.id).unwrap();
            let pulls = get_repo_pulls_blocking(mesh.id, "open".to_string()).unwrap();
            assert!(issues.items.is_empty());
            assert!(pulls.items.is_empty());
            // A read that never happened has nothing to be incomplete about,
            // and must not render as a truncation warning.
            assert!(issues.completeness.complete);
            assert!(pulls.completeness.complete);
            assert_eq!(issues.completeness.incomplete_reason, None);
            db::delete_mesh(mesh.id).unwrap();
            drop(tmp);
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires default WSL; changes process-global libgit2 config search path, run serially"]
    fn live_wsl_github_repository_trust() {
        let guest_home = env::wsl_home().expect("WSL home");
        let fixture = tempfile::Builder::new()
            .prefix("buildmesh-github-")
            .tempdir_in(env::to_host_path(&guest_home.to_string_lossy()))
            .unwrap();
        let host_path = fixture.path().to_str().unwrap();
        let guest_path = env::normalize_unc_to_wsl(host_path).into_owned();
        let mut command = crate::process_util::command_no_window("wsl.exe");
        command.args([
            "-d",
            &env::get_default_wsl_distro().unwrap(),
            "--exec",
            "git",
            "init",
            &guest_path,
        ]);
        let output = crate::process_util::run_command_with_timeout(
            command,
            "WSL fixture init",
            std::time::Duration::from_secs(15),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::write(fixture.path().join(".git/config"), "[core]\nrepositoryformatversion = 0\nbare = false\n[remote \"origin\"]\nurl = https://github.com/example/wsl-fixture.git\n").unwrap();

        let config_dir = tempfile::tempdir().unwrap();
        let mut config = git2::Config::open(&config_dir.path().join(".gitconfig")).unwrap();
        struct RestoreConfig(std::ffi::CString);
        impl Drop for RestoreConfig {
            fn drop(&mut self) {
                unsafe {
                    git2::opts::set_search_path(git2::ConfigLevel::Global, self.0.clone()).unwrap();
                }
            }
        }
        let _restore = RestoreConfig(unsafe {
            git2::opts::get_search_path(git2::ConfigLevel::Global).unwrap()
        });
        unsafe {
            git2::opts::set_search_path(git2::ConfigLevel::Global, config_dir.path()).unwrap();
        }
        assert_eq!(
            Repository::open(host_path).err().unwrap().code(),
            git2::ErrorCode::Owner
        );
        let error = resolve_owner_repo(host_path).unwrap_err();
        assert!(error.contains("safe.directory"), "{error}");
        assert!(error.contains("Windows"), "{error}");
        config
            .set_str("safe.directory", &host_path.replace('\\', "/"))
            .unwrap();
        let expected = Some(("example".to_string(), "wsl-fixture".to_string()));
        assert_eq!(resolve_owner_repo(host_path).unwrap(), expected);
        assert_eq!(resolve_owner_repo(&guest_path).unwrap(), expected);
        assert_eq!(repo_info(&guest_path).unwrap().owner_repo, expected);
        assert_eq!(
            github_url_for_path(&guest_path).unwrap(),
            Some("https://github.com/example/wsl-fixture".to_string())
        );
    }

    // ----- GitHubIssue wire shape (issue #481 follow-up: blocked_by) -----

    /// Full wire shape including the `blocked_by` field. Pins the field's
    /// JSON name and element type so the regenerated `src/types/generated/
    /// GitHubIssue.ts` keeps `blocked_by: Array<number>` and the frontend's
    /// strict typing continues to compile. The CI gate
    /// `git diff --exit-code src/types/generated/` (per CLAUDE.md) catches
    /// accidental drift here.
    #[test]
    fn github_issue_wire_shape_with_blocked_by() {
        let json = r#"{
            "number": 482,
            "url": "https://github.com/alondero/buildmesh/issues/482",
            "title": "Issue blocked by another",
            "body": "see Blocked by section",
            "state": "open",
            "labels": [],
            "blocked_by": [481, 999]
        }"#;
        let issue: GitHubIssue = serde_json::from_str(json).expect("full wire shape parses");
        assert_eq!(issue.number, 482);
        assert_eq!(issue.blocked_by, vec![481, 999]);
    }

    /// Missing `blocked_by` key (older frontend, partial payload, or a
    /// future test fixture that forgets it) must default to `vec![]`
    /// rather than failing to parse. The `#[serde(default)]` on the
    /// field is the safety net; this test pins the behaviour so a future
    /// refactor that flips the field to required surfaces as a test
    /// failure rather than a silent rollback.
    #[test]
    fn github_issue_wire_shape_with_missing_blocked_by_defaults() {
        let json = r#"{
            "number": 481,
            "url": "https://github.com/alondero/buildmesh/issues/481",
            "title": "No blockers",
            "body": "None - can start immediately.",
            "state": "open",
            "labels": []
        }"#;
        let issue: GitHubIssue = serde_json::from_str(json).expect("partial wire shape parses");
        assert!(
            issue.blocked_by.is_empty(),
            "missing blocked_by defaults to empty vec"
        );
    }

    /// Round-trip: serialise a populated `blocked_by` and re-parse it
    /// unchanged. Catches a class of bugs where serde rename / flatten
    /// rules accidentally drop the field on serialisation (which would
    /// only show up at the IPC seam, not at the parse-from-test-fixture
    /// site).
    #[test]
    fn github_issue_wire_shape_blocked_by_round_trips() {
        let original = GitHubIssue {
            number: 482,
            title: "Blocked issue".into(),
            body: "body".into(),
            url: "https://github.com/x/y/issues/482".into(),
            state: "open".into(),
            labels: vec!["bug".into()],
            blocked_by: vec![481, 482, 483],
            author: "contributor-jane".into(),
        };
        let json = serde_json::to_string(&original).expect("serialise");
        let parsed: GitHubIssue = serde_json::from_str(&json).expect("re-parse");
        assert_eq!(parsed.blocked_by, vec![481, 482, 483]);
        assert_eq!(parsed.number, 482);
        assert_eq!(parsed.labels, vec!["bug".to_string()]);
        assert_eq!(parsed.author, "contributor-jane");
    }

    /// The contributor pill on the Issues probe row reads `author`; an
    /// older cached payload without the key must still parse (additive
    /// field) and default to `\"\"` — the pill then doesn't render.
    #[test]
    fn github_issue_wire_shape_author_defaults_empty() {
        let json = r#"{
            "number": 7,
            "url": "https://github.com/x/y/issues/7",
            "title": "legacy",
            "body": "",
            "state": "open",
            "labels": [],
            "blocked_by": []
        }"#;
        let issue: GitHubIssue = serde_json::from_str(json).expect("partial wire shape parses");
        assert_eq!(issue.author, "", "missing author defaults to empty");

        let json_author = r#"{
            "number": 8,
            "url": "https://github.com/x/y/issues/8",
            "title": "with author",
            "body": "",
            "state": "open",
            "labels": [],
            "blocked_by": [],
            "author": "octocat"
        }"#;
        let issue: GitHubIssue =
            serde_json::from_str(json_author).expect("author wire shape parses");
        assert_eq!(issue.author, "octocat");
    }

    // ----- GitHubPullRequest wire shape (issue #420) ---------------------

    /// `head_ref` is optional with `#[serde(default)]` so a partial response
    /// (older / cached) still parses — the spawn path surfaces "missing head
    /// ref" as a user-facing error. Pin the wire shape so a future refactor
    /// that flips it to required surfaces as a test failure rather than a
    /// runtime panic. The Tauri-side wire struct uses the field name `url`
    /// (mapped from GitHub's `html_url` by the command-side mapper), not
    /// `html_url`.
    #[test]
    fn github_pull_request_wire_shape_with_head_ref() {
        let json = r#"{
            "number": 420,
            "url": "https://github.com/alondero/buildmesh/pull/420",
            "title": "spawn on PR",
            "body": "spawn an agent on a PR",
            "state": "open",
            "draft": false,
            "head_ref": "feat/420-pr-spawn",
            "head_sha": "0123456789abcdef0123456789abcdef01234567"
        }"#;
        let pr: GitHubPullRequest = serde_json::from_str(json).expect("full wire shape parses");
        assert_eq!(pr.head_ref, "feat/420-pr-spawn");
        // Issue #444 — the SHA must round-trip through the Tauri-side wire
        // shape unchanged so the spawn path can persist it for exact-pinning.
        assert_eq!(pr.head_sha, "0123456789abcdef0123456789abcdef01234567");
    }

    #[test]
    fn github_pull_request_wire_shape_with_missing_head_ref_defaults() {
        let json = r#"{
            "number": 7,
            "url": "https://github.com/x/y/pull/7",
            "title": "legacy",
            "body": "",
            "state": "open",
            "draft": false
        }"#;
        let pr: GitHubPullRequest = serde_json::from_str(json).expect("partial wire shape parses");
        assert_eq!(pr.head_ref, "", "missing head_ref defaults to empty");
        assert_eq!(pr.head_sha, "", "missing head_sha defaults to empty");
        assert_eq!(
            pr.mergeable, None,
            "missing mergeable defaults to None (unknown)"
        );
        assert_eq!(
            pr.mergeable_state, "",
            "missing mergeable_state defaults to empty"
        );
        assert_eq!(pr.author, "", "missing author defaults to empty");
    }

    /// The contributor pill on the PRs probe row reads `author` — pin the
    /// wire name so the regenerated `GitHubPullRequest.ts` keeps
    /// `author: string` and the frontend's strict typing keeps compiling.
    #[test]
    fn github_pull_request_wire_shape_with_author() {
        let json = r#"{
            "number": 9,
            "url": "https://github.com/x/y/pull/9",
            "title": "authored PR",
            "body": "",
            "state": "open",
            "draft": false,
            "head_ref": "feat/9",
            "author": "contributor-jane"
        }"#;
        let pr: GitHubPullRequest = serde_json::from_str(json).expect("author wire shape parses");
        assert_eq!(pr.author, "contributor-jane");
    }

    /// Issue #1529: mergeability rides inline on the list wire shape.
    /// `mergeable: true/false/null` round-trips with `mergeable_state`, so
    /// the panel renders mergeable/conflict/checking without a second call.
    /// `null` (UNKNOWN) must stay `None`, not coerce to `Some(false)`.
    #[test]
    fn github_pull_request_wire_shape_with_inline_mergeability() {
        let json = r#"{
            "number": 1529,
            "url": "https://github.com/alondero/buildmesh/pull/1529",
            "title": "perf summaries",
            "body": "",
            "state": "open",
            "draft": false,
            "head_ref": "perf/1529-summaries",
            "head_sha": "0123456789abcdef0123456789abcdef01234567",
            "mergeable": true,
            "mergeable_state": "clean"
        }"#;
        let pr: GitHubPullRequest = serde_json::from_str(json).expect("inline mergeability parses");
        assert_eq!(pr.mergeable, Some(true));
        assert_eq!(pr.mergeable_state, "clean");

        let json_null = r#"{
            "number": 1530,
            "url": "https://github.com/x/y/pull/1530",
            "title": "fresh",
            "body": "",
            "state": "open",
            "draft": false,
            "mergeable": null,
            "mergeable_state": "unknown"
        }"#;
        let pr_null: GitHubPullRequest =
            serde_json::from_str(json_null).expect("null mergeable parses");
        assert_eq!(
            pr_null.mergeable, None,
            "null must stay None, not coerce to Some(false)"
        );
        assert_eq!(pr_null.mergeable_state, "unknown");
    }

    /// Issue #443: a fork PR carries `head_repo_owner` (the fork's owner
    /// login) + `head_repo_clone_url` (the fork's clone URL) on the wire
    /// so the spawn path can register the fork as a remote. Both fields
    /// are `#[serde(default)]` so a partial response (older frontend, or
    /// an API hiccup that drops `head.repo`) still parses — the spawn
    /// path then treats empty values as "no fork info, use the #420
    /// same-repo path" and a head_ref-empty head as the refusal case.
    #[test]
    fn github_pull_request_wire_shape_with_fork_metadata() {
        let json = r#"{
            "number": 443,
            "url": "https://github.com/alondero/buildmesh/pull/443",
            "title": "fork PR",
            "body": "spawn on a fork",
            "state": "open",
            "draft": false,
            "head_ref": "feat/443-fork",
            "head_repo_owner": "alice",
            "head_repo_clone_url": "https://github.com/alice/buildmesh.git"
        }"#;
        let pr: GitHubPullRequest = serde_json::from_str(json).expect("fork wire shape parses");
        assert_eq!(pr.head_ref, "feat/443-fork");
        assert_eq!(pr.head_repo_owner, "alice");
        assert_eq!(
            pr.head_repo_clone_url,
            "https://github.com/alice/buildmesh.git"
        );
    }

    /// Partial response with no fork metadata: both fields default to "".
    /// This is the #420 same-repo case — the spawn path takes the
    /// `git fetch origin <head_ref>` branch.
    #[test]
    fn github_pull_request_wire_shape_with_missing_fork_metadata_defaults() {
        let json = r#"{
            "number": 8,
            "url": "https://github.com/x/y/pull/8",
            "title": "legacy",
            "body": "",
            "state": "open",
            "draft": false,
            "head_ref": "feat/legacy"
        }"#;
        let pr: GitHubPullRequest = serde_json::from_str(json).expect("partial wire shape parses");
        assert_eq!(pr.head_ref, "feat/legacy");
        assert_eq!(
            pr.head_repo_owner, "",
            "missing head_repo_owner defaults to empty"
        );
        assert_eq!(
            pr.head_repo_clone_url, "",
            "missing head_repo_clone_url defaults to empty"
        );
    }

    // ----- PrMergeabilityEntry wire shape (issue #418) ---------------------
    //
    // The batched endpoint returns one entry per requested PR number. Pins:
    //   - `number` is present and is the `i32` JS-number shape (matches the
    //     `#[ts(as = "i32")]` convention on the sibling wire types)
    //   - `mergeable: null` survives the round-trip (still computing or a
    //     per-PR error fallback)
    //   - `mergeable_state: ""` is the safe default for missing fields

    /// Full wire shape with a real merge result. Pins the JSON field names
    /// and types so the regenerated `src/types/generated/PrMergeabilityEntry.ts`
    /// stays in lock-step with the Rust struct. The CI drift gate
    /// `git diff --exit-code src/types/generated/` catches accidental drift
    /// here, but pinning it locally documents the intent.
    #[test]
    fn pr_mergeability_entry_wire_shape_with_clean_result() {
        let json = r#"{
            "number": 418,
            "mergeable": true,
            "mergeable_state": "clean"
        }"#;
        let entry: PrMergeabilityEntry = serde_json::from_str(json).expect("full shape parses");
        assert_eq!(entry.number, 418);
        assert_eq!(entry.mergeable, Some(true));
        assert_eq!(entry.mergeable_state, "clean");
    }

    /// `mergeable: null` (GitHub still computing) must round-trip as `None`,
    /// not coerce to `Some(false)` — the panel renders the row in "Checking…"
    /// for `None` and would falsely claim conflicts on `Some(false)`. Same
    /// invariant as the existing `PrMergeability` parse test, mirrored here
    /// so the batched endpoint can't drift from the per-PR contract.
    #[test]
    fn pr_mergeability_entry_wire_shape_with_null_mergeable() {
        let json = r#"{
            "number": 7,
            "mergeable": null,
            "mergeable_state": "unknown"
        }"#;
        let entry: PrMergeabilityEntry = serde_json::from_str(json).expect("null mergeable parses");
        assert_eq!(
            entry.mergeable, None,
            "null must stay None, not coerce to Some(false)"
        );
        assert_eq!(entry.mergeable_state, "unknown");
    }

    /// Per-PR error fallback — when the batched command catches an
    /// individual PR's HTTP error, it returns
    /// `{ mergeable: null, mergeable_state: "error: <reason>" }`. The
    /// frontend renders "Checking…" for this state (matches the existing
    /// per-PR failure semantics). Pin the error-state shape so a future
    /// refactor that drops `mergeable_state`'s "error: " prefix is caught
    /// here rather than at the UI fallback site.
    #[test]
    fn pr_mergeability_entry_wire_shape_with_error_state() {
        let json = r#"{
            "number": 42,
            "mergeable": null,
            "mergeable_state": "error: GitHub API error (404): Not Found"
        }"#;
        let entry: PrMergeabilityEntry = serde_json::from_str(json).expect("error state parses");
        assert_eq!(entry.mergeable, None);
        assert!(
            entry.mergeable_state.starts_with("error: "),
            "per-PR failures must carry the 'error: ' prefix so the panel can keep them in 'Checking…'"
        );
    }

    /// Round-trip: serialise a populated `PrMergeabilityEntry` and re-parse
    /// it unchanged. Catches a class of bugs where serde rename / flatten
    /// rules accidentally drop the `number` field on serialisation (which
    /// would only show up at the IPC seam, not at the parse-from-fixture
    /// site). The regenerated TS type derives the field name from the Rust
    /// struct name, so a drop would also fail the drift gate — this test
    /// makes the intent explicit.
    #[test]
    fn pr_mergeability_entry_round_trips_with_all_fields() {
        let original = PrMergeabilityEntry {
            number: 999,
            mergeable: Some(false),
            mergeable_state: "dirty".into(),
        };
        let json = serde_json::to_string(&original).expect("serialise");
        let parsed: PrMergeabilityEntry = serde_json::from_str(&json).expect("re-parse");
        assert_eq!(parsed.number, 999);
        assert_eq!(parsed.mergeable, Some(false));
        assert_eq!(parsed.mergeable_state, "dirty");
    }

    // ----- mergeability_entries helper (issue #418) -----------------------
    //
    // The helper is the per-PR mapping loop the batched command reuses for
    // every PR. Tested with a closure probe so we can pin the mapping
    // without a trait seam on GitHubClient or a wiremock dependency —
    // the closure accepts the probe function as data, and the helper
    // stays a pure iterator over (pr_number, probe_result).
    //
    // The "one token resolution" guarantee is enforced by construction
    // (the command constructs `GitHubClient::new()` exactly once); these
    // tests pin the per-PR mapping the helper applies on top of that
    // guarantee.

    /// Empty input → empty output, without ever invoking the probe. The
    /// empty-pr_numbers short-circuit in `get_prs_mergeability` builds on
    /// this: the public command returns `Ok(vec![])` before client
    /// construction, the helper itself iterates zero times here. Pin
    /// both pieces so a future refactor that drops the early return (or
    /// flips the helper to call the probe once for "initialisation")
    /// surfaces as a test failure rather than a slow no-op.
    #[test]
    fn mergeability_entries_empty_input_does_not_call_probe() {
        let calls: std::cell::RefCell<Vec<i64>> = std::cell::RefCell::new(Vec::new());
        let entries = mergeability_entries(Vec::new(), |n| {
            calls.borrow_mut().push(n);
            Ok((Some(true), "clean".into()))
        });
        assert!(entries.is_empty(), "empty input → empty output");
        assert!(
            calls.borrow().is_empty(),
            "probe must not fire on empty input"
        );
    }

    /// All-success path: each probe succeeds, the entry carries the
    /// probed `mergeable` + `mergeable_state` AND the request's PR
    /// number. The `number` round-trip is the load-bearing piece — a
    /// future refactor that drops the closure arg or hard-codes the
    /// number would silently desync the batched response from the
    /// frontend's PR list.
    #[test]
    fn mergeability_entries_preserves_pr_number_on_success() {
        let entries = mergeability_entries(vec![201, 202, 204], |n| match n {
            201 => Ok((Some(true), "clean".into())),
            202 => Ok((Some(false), "dirty".into())),
            204 => Ok((None, "unknown".into())),
            _ => unreachable!(),
        });
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].number, 201);
        assert_eq!(entries[0].mergeable, Some(true));
        assert_eq!(entries[0].mergeable_state, "clean");
        assert_eq!(entries[1].number, 202);
        assert_eq!(entries[1].mergeable, Some(false));
        assert_eq!(entries[1].mergeable_state, "dirty");
        assert_eq!(entries[2].number, 204);
        assert_eq!(entries[2].mergeable, None, "still-computing stays None");
        assert_eq!(entries[2].mergeable_state, "unknown");
    }

    /// Per-PR HTTP failure does NOT fail the whole batch. A failing PR
    /// becomes `{ mergeable: None, mergeable_state: "error: <reason>" }`
    /// and the surrounding PRs still surface their real results. The
    /// batched endpoint's value proposition is "transient failure on
    /// one PR doesn't kill the rest"; this test pins the fallback so a
    /// future refactor that re-raises `Err(_)` from the helper breaks
    /// loudly here rather than regressing to the old per-PR fan-out's
    /// "first failure drops the whole list" behaviour.
    #[test]
    fn mergeability_entries_per_pr_failure_does_not_fail_batch() {
        let entries = mergeability_entries(vec![1, 2, 3], |n| match n {
            1 => Ok((Some(true), "clean".into())),
            2 => Err(GitHubError::Api(404, "Not Found".into())),
            3 => Ok((Some(false), "dirty".into())),
            _ => unreachable!(),
        });
        assert_eq!(
            entries.len(),
            3,
            "batch must carry one entry per PR even when one fails"
        );
        // The success entries round-trip unchanged.
        assert_eq!(entries[0].number, 1);
        assert_eq!(entries[0].mergeable, Some(true));
        // The failed entry becomes the "checking" sentinel.
        assert_eq!(
            entries[1].number, 2,
            "failed entry must still carry the PR number"
        );
        assert_eq!(
            entries[1].mergeable, None,
            "failed entry must report mergeable: None"
        );
        assert!(
            entries[1].mergeable_state.starts_with("error: "),
            "failed entry must carry the 'error: ' prefix; got: {}",
            entries[1].mergeable_state
        );
        assert!(entries[1].mergeable_state.contains("404"));
        // The follow-up PR after the failure is unaffected.
        assert_eq!(entries[2].number, 3);
        assert_eq!(entries[2].mergeable, Some(false));
    }

    /// `GitHubError::NoToken` (auth missing) maps the same way as any
    /// other per-PR error — a single probe failing for auth reasons does
    /// NOT fail the whole batch. (The whole batch would already have
    /// failed earlier in the public command via `GitHubClient::new()` —
    /// this test exercises the helper in isolation so the per-PR
    /// mapping is pinned across all error variants.)
    #[test]
    fn mergeability_entries_no_token_error_becomes_checking_sentinel() {
        let entries = mergeability_entries(vec![42], |_| Err(GitHubError::NoToken));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].number, 42);
        assert_eq!(entries[0].mergeable, None);
        assert!(
            entries[0].mergeable_state.starts_with("error: "),
            "NoToken must surface as 'error: ...' so the panel keeps the row in 'Checking…'"
        );
    }

    /// `mergeability_from_summaries` serves hits from the summary pages and
    /// falls back to one direct detail request per number the pages miss
    /// (older than the 100-row cap). Script: open page carries #1 only,
    /// closed page is empty, then one REST detail answers #2. Cost is 3
    /// requests for 2 PRs — O(pages) for the hits, one extra each only for
    /// genuine misses — and request order is preserved.
    #[test]
    fn mergeability_from_summaries_falls_back_to_detail_past_the_cap() {
        use crate::services::github::tests::{fake_node, fake_server, Scripted};
        use std::sync::atomic::Ordering;

        let open = serde_json::Value::Array(vec![fake_node(1)]);
        let closed = serde_json::Value::Array(vec![]);
        let (base, count, handle) = fake_server(vec![
            Scripted::Page(open, false, None),
            Scripted::Page(closed, false, None),
            Scripted::Detail,
        ]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");
        let entries = mergeability_from_summaries(&client, "acme", "demo", vec![1, 2])
            .expect("batch must not fail on a miss");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].number, 1);
        assert_eq!(entries[0].mergeable, Some(true), "summary hit");
        assert_eq!(entries[1].number, 2);
        assert_eq!(entries[1].mergeable, Some(true), "detail fallback hit");
        assert_eq!(entries[1].mergeable_state, "clean");
        handle.join().expect("server");
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    /// A caller passing a duplicate number (`[1, 1]`) must resolve it twice
    /// from the map: entries are read with `.get`, never drained, so the
    /// second occurrence cannot trip the missing-key sentinel.
    #[test]
    fn mergeability_from_summaries_resolves_duplicate_numbers_twice() {
        use crate::services::github::tests::{fake_node, fake_server, Scripted};
        use std::sync::atomic::Ordering;

        let open = serde_json::Value::Array(vec![fake_node(1)]);
        let (base, count, handle) = fake_server(vec![Scripted::Page(open, false, None)]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");
        let entries = mergeability_from_summaries(&client, "acme", "demo", vec![1, 1])
            .expect("batch must not fail on duplicates");
        assert_eq!(entries.len(), 2);
        for entry in &entries {
            assert_eq!(entry.number, 1);
            assert_eq!(entry.mergeable, Some(true));
            assert!(
                !entry.mergeable_state.starts_with("error: "),
                "duplicate must not become a sentinel; got: {}",
                entry.mergeable_state
            );
        }
        handle.join().expect("server");
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

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
            path: root.to_string_lossy().into_owned(),
            worktree_name: Some("agent-1".into()),
            use_worktree: true,
            ..Default::default()
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
            path: root.to_string_lossy().into_owned(),
            use_worktree: false,
            ..Default::default()
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
            // PID-prefixed: `NEXT_ID` is only unique inside one test process,
            // and the shard runner executes several binaries against the same
            // temp directory at once.
            let tmp = std::env::temp_dir().join(format!(
                "buildmesh_pr_test_{}_{}",
                std::process::id(),
                id
            ));
            Self(tmp)
        }
        fn path(&self) -> &Path {
            &self.0
        }
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
        let mut options = git2::RepositoryInitOptions::new();
        options.initial_head("main");
        let repo = git2::Repository::init_opts(tmp.path(), &options).unwrap();
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
        let mut options = git2::RepositoryInitOptions::new();
        options.initial_head("main");
        git2::Repository::init_opts(tmp.path(), &options).unwrap();
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
        assert!(
            info.remote_url.is_none(),
            "no origin configured in this test"
        );
        assert!(info.owner_repo.is_none(), "no origin → no owner_repo");
    }

    #[test]
    fn repo_info_parses_github_origin_https() {
        let (_guard, path) = init_repo_with_origin("https://github.com/alondero/buildmesh.git");
        let info = repo_info(&path).expect("repo_info");
        assert_eq!(info.branch, "main");
        assert_eq!(
            info.owner_repo,
            Some(("alondero".to_string(), "buildmesh".to_string()))
        );
    }

    #[test]
    fn repo_info_parses_github_origin_ssh() {
        let (_guard, path) = init_repo_with_origin("git@github.com:alondero/buildmesh.git");
        let info = repo_info(&path).expect("repo_info");
        assert_eq!(
            info.owner_repo,
            Some(("alondero".to_string(), "buildmesh".to_string()))
        );
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

    // ----- github_url_for_path (Issue: View on GitHub context-menu item) -----
    //
    // The helper is pure and takes a path string, so the tests don't need
    // a DB or Tauri runtime — they reuse the `init_repo_with_origin` /
    // `init_repo_unborn` helpers above. Three cases pin the wire
    // contract the frontend relies on:
    //   - GitHub origin (HTTPS) → `Some("https://github.com/owner/repo")`
    //   - Non-GitHub origin → `None` (the menu item is hidden)
    //   - No origin at all → `None` (the menu item is hidden)
    // Plus a sanity check on the `.git` suffix and SSH form, mirroring
    // the existing `parse_owner_repo` tests at lines 1092–1112.

    #[test]
    fn github_url_for_path_https_origin() {
        let (_guard, path) = init_repo_with_origin("https://github.com/alondero/buildmesh.git");
        let url = github_url_for_path(&path).expect("github_url_for_path should succeed");
        assert_eq!(
            url,
            Some("https://github.com/alondero/buildmesh".to_string())
        );
    }

    #[test]
    fn github_url_for_path_ssh_origin() {
        let (_guard, path) = init_repo_with_origin("git@github.com:alondero/buildmesh.git");
        let url = github_url_for_path(&path).expect("github_url_for_path should succeed");
        assert_eq!(
            url,
            Some("https://github.com/alondero/buildmesh".to_string())
        );
    }

    #[test]
    fn github_url_for_path_no_dot_git_suffix() {
        let (_guard, path) = init_repo_with_origin("https://github.com/foo/bar");
        let url = github_url_for_path(&path).expect("github_url_for_path should succeed");
        // The .git trim lives inside `parse_owner_repo`; pin the wire
        // shape so a future refactor that re-adds the suffix is caught.
        assert_eq!(url, Some("https://github.com/foo/bar".to_string()));
    }

    #[test]
    fn github_url_for_path_non_github_origin_is_none() {
        let (_guard, path) = init_repo_with_origin("https://gitlab.com/alondero/buildmesh.git");
        let url = github_url_for_path(&path).expect("non-github origin should not error");
        assert_eq!(
            url, None,
            "GitLab URLs must collapse to None so the menu hides the item"
        );
    }

    #[test]
    fn github_url_for_path_no_origin_is_none() {
        let (_guard, path) = init_repo_with_commit();
        let url = github_url_for_path(&path).expect("no origin should not error");
        assert_eq!(
            url, None,
            "repos with no origin remote must collapse to None"
        );
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
        assert_eq!(
            resolved, node.path,
            "non-worktree node must resolve to its own path"
        );
        assert!(
            !resolved.contains("worktrees"),
            "must NOT add a worktree subdir for root nodes"
        );
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
            "PR chip looks up head=<branch>; the agent's working branch is the worktree name, not the mesh's Base Ref"
        );
        // And — for the bug regression — opening the MESH ROOT itself gives
        // its base branch, not `agent-1`. The base branch name comes from
        // libgit2's host defaults (`master` or `main`).
        let root_info = repo_info(&node.path).expect("mesh root must open");
        assert_ne!(
            root_info.branch, "agent-1",
            "sanity: mesh root is on the Base Ref; this is what the bug used to read"
        );
    }

    /// `get_current_branch` is the mobile REST route's backing command; for a
    /// worktree node it must report the worktree's branch, not the mesh's
    /// Base Ref. Same bug class as the PR chip.
    #[test]
    fn get_current_branch_via_working_path_returns_worktree_branch() {
        let (_guard, _wt_path, node) = make_worktree_node();
        let branch = repo_info(&env::node_working_path(&node).host_path)
            .expect("worktree repo must open")
            .branch;
        assert_eq!(
            branch, "agent-1",
            "must read the worktree's HEAD, not the mesh root's"
        );
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

    // ----- create_pr_idempotency (issue #771) -----------------------------
    //
    // The optimistic-with-422-recovery pattern that closes the duplicate-PR
    // gap #762 opened (180s write timeout → slow POST succeeds server-side,
    // client times out, retry duplicates the PR). Tests drive the PRODUCTION
    // boundary (`create_pr_for_mesh_blocking_with_client` and
    // `create_pr_blocking_with_client`) with a real temp repo + a `for_test`
    // `GitHubClient` pointed at a fake server, so wire-level expectations
    // (request method, URL encoding, status code, body shape) are pinned
    // against the same code path the desktop/mobile frontends exercise.
    //
    // Covered cases:
    //   - duplicate-create recovery       → POST 422 → GET existing → return URL
    //   - happy path (no duplicate)       → POST 201 → return new URL
    //   - non-422 error (403, 404, 5xx)   → propagate verbatim, no recovery GET
    //   - URL encoding on the recovery GET → `:` and `/` in `owner:branch`
    //                                         must percent-encode
    //   - structured 422 detection        → "already exists" only via parsed
    //                                       `errors[].message`, not raw body
    //   - fork head (Finding 1)           → qualified `fork_user:branch` is
    //                                       passed through verbatim, not
    //                                       double-prefixed
    //   - session_id path (Finding 6)     → `create_pr_blocking_with_client`
    //                                       drives the worktree/DB branch

    /// PR JSON for the existing-PR recovery fixture. Only `number` and
    /// `html_url` are required; `PullRequest` carries `#[serde(default)]`
    /// on every other field.
    fn existing_pr_json(number: i64, html_url: &str) -> serde_json::Value {
        serde_json::json!([{
            "number": number,
            "html_url": html_url,
            "title": format!("fix: dup-PR guard (#{})", number),
            "state": "open",
            "draft": false,
        }])
    }

    /// PR JSON for the happy-path create fixture (POST 201 body).
    fn created_pr_json(number: i64, html_url: &str) -> serde_json::Value {
        serde_json::json!({
            "number": number,
            "html_url": html_url,
            "title": "fix: dup-PR guard",
            "body": "body",
            "state": "open",
            "draft": false,
        })
    }

    /// Init a temp repo with one commit, `origin` set to the given GitHub
    /// URL, and HEAD checked out to `branch`. Returns `(guard, path)` —
    /// caller MUST hold `guard` for the temp dir's lifetime.
    fn init_repo_on_branch(url: &str, branch: &str) -> (TempGitRepo, String) {
        let (guard, path) = init_repo_with_origin(url);
        let repo = git2::Repository::open(&path).expect("open fresh repo");
        let head_commit = repo
            .head()
            .expect("head exists after init_repo_with_origin")
            .peel_to_commit()
            .expect("head is a commit");
        repo.branch(branch, &head_commit, true)
            .expect("create branch");
        let refname = format!("refs/heads/{branch}");
        repo.set_head(&refname).expect("set HEAD to branch");
        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
            .expect("checkout branch");
        (guard, path)
    }

    /// Pin the issue's exact failure mode: an open PR already exists, the
    /// user retries, and the production boundary must return the existing
    /// URL via the 422-recovery path. End-to-end (real temp repo →
    /// `repo_info` → `info.owner_repo()` → `client.create_pull_request_idempotent`).
    /// Two requests fire (POST 422 + GET recovery); the load-bearing
    /// assertion is `count == 2` — a regression that took the optimistic
    /// path but never recovered would hit `count == 1` and short-circuit
    /// to a confusing error.
    #[test]
    fn create_pr_for_mesh_recovers_from_duplicate_create_422() {
        use std::sync::atomic::Ordering;

        let (_guard, mesh_path) =
            init_repo_on_branch("https://github.com/test-owner/test-repo.git", "feat/771");
        let existing = existing_pr_json(771, "https://github.com/test-owner/test-repo/pull/771");
        // Optimistic path: POST first, GitHub answers 422 ("already exists"),
        // we recover by GET'ing the existing PR.
        let (base, count, handle) = fake_server(vec![
            Scripted::CreatePrConflict(
                r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for test-owner:feat/771."}]}"#.to_string(),
            ),
            // `expected_head` is the percent-encoded form the client must
            // produce: `test-owner:feat/771` → `test-owner%3Afeat%2F771`.
            Scripted::ListPulls {
                body: existing,
                expected_head: "test-owner%3Afeat%2F771".to_string(),
            },
        ]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");

        let url = create_pr_for_mesh_blocking_with_client(
            &client,
            &mesh_path,
            "new title",
            "new body",
            "main",
        )
        .expect("must return the existing PR's URL on 422 recovery");

        assert_eq!(
            url, "https://github.com/test-owner/test-repo/pull/771",
            "must return the EXISTING PR's html_url, not the freshly-attempted one"
        );
        handle.join().expect("server");
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "POST (422) + GET (recovery) — count == 1 means the recovery GET was skipped"
        );
    }

    /// Happy path: no duplicate → POST returns 201 → return new URL. ONE
    /// round trip total (no recovery GET). A regression that always
    /// pre-checks would inflate this to 2 requests, paying latency for
    /// nothing on the 99% path.
    #[test]
    fn create_pr_for_mesh_happy_path_no_duplicate() {
        use std::sync::atomic::Ordering;

        let (_guard, mesh_path) =
            init_repo_on_branch("https://github.com/test-owner/test-repo.git", "feat/771");
        let created = created_pr_json(772, "https://github.com/test-owner/test-repo/pull/772");
        let (base, count, handle) = fake_server(vec![Scripted::CreatePullRequest(created)]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");

        let url =
            create_pr_for_mesh_blocking_with_client(&client, &mesh_path, "title", "body", "main")
                .expect("creates new PR");

        assert_eq!(
            url, "https://github.com/test-owner/test-repo/pull/772",
            "must return the CREATED PR's html_url"
        );
        handle.join().expect("server");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "exactly one POST — the optimistic path must not pre-check on the happy case"
        );
    }

    /// Negative control: a non-422 error (e.g. 403 forbidden, 422 with a
    /// different shape, 500 server error) must NOT trigger the recovery
    /// path. If it did, a permission error would silently mask as a
    /// successful create, and a 422 for a missing-branch would silently
    /// mask as a successful create.
    #[test]
    fn create_pr_for_mesh_propagates_non_422_errors() {
        use std::sync::atomic::Ordering;

        let (_guard, mesh_path) =
            init_repo_on_branch("https://github.com/test-owner/test-repo.git", "feat/771");
        let (base, count, handle) = fake_server(vec![Scripted::CreatePrError(
            403,
            r#"{"message":"Must have admin rights"}"#.to_string(),
        )]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");

        let err =
            create_pr_for_mesh_blocking_with_client(&client, &mesh_path, "title", "body", "main")
                .expect_err("403 must propagate, not silently recover");

        assert!(
            err.contains("403") || err.contains("admin"),
            "error must surface the real GitHub diagnostic, got: {err}"
        );
        handle.join().expect("server");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "exactly one POST — non-422 errors must not trigger a recovery GET"
        );
    }

    /// `same_branch` guard: production refuses to POST when the agent's
    /// current branch equals the base branch (would be a "main → main"
    /// PR). The optimistic helper doesn't need to fire — the guard fires
    /// first. Without this test, a regression that dropped the
    /// branch==base_branch check would only surface when a user
    /// accidentally ran Create PR with no feature branch checked out.
    #[test]
    fn create_pr_for_mesh_refuses_same_branch_as_base() {
        // init_repo_with_origin lands HEAD on the default branch; pass
        // that as the base_branch and watch the guard reject.
        let (_guard, mesh_path) =
            init_repo_with_origin("https://github.com/test-owner/test-repo.git");
        let (base, _count, _handle) = fake_server(vec![]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");

        // Detect the default branch name — git2's `Repository::head_branch_name`
        // isn't always set; fall back to inspecting the ref tree.
        let repo = git2::Repository::open(&mesh_path).expect("open");
        let head_name = repo
            .head()
            .ok()
            .and_then(|h| h.shorthand().map(str::to_string))
            .unwrap_or_else(|| "main".to_string());

        let err = create_pr_for_mesh_blocking_with_client(
            &client, &mesh_path, "title", "body", &head_name, // base == head → guard rejects
        )
        .expect_err("must refuse same-branch create");

        assert!(
            err.contains("nothing to compare"),
            "error must name the same-branch violation, got: {err}"
        );
    }

    /// URL-encoding pin for the recovery GET. The fake server's
    /// `Scripted::ListPulls { expected_head, .. }` asserts the request
    // line contains the EXACT percent-encoded head substring. A
    /// regression that drops encoding (or re-orders the encoding rules)
    /// would corrupt branches like `feat/771` → `feat/771` (unescaped
    /// `/` in a query value is technically tolerated by some HTTP
    /// parsers but not by `reqwest`'s strict builder) and fail this
    /// assertion. The branch name uses both `:` (owner separator) and
    /// `/` (nested ref) to exercise the two most common special chars.
    #[test]
    fn create_pr_for_mesh_url_encodes_recovery_head_param() {
        let (_guard, mesh_path) = init_repo_on_branch(
            "https://github.com/test-owner/test-repo.git",
            "feat/with/slashes",
        );
        let existing = existing_pr_json(773, "https://github.com/test-owner/test-repo/pull/773");
        // Both `:` (after owner) and `/` (in branch) must percent-encode:
        // `test-owner:feat/with/slashes` → `test-owner%3Afeat%2Fwith%2Fslashes`.
        let (base, _count, handle) = fake_server(vec![
            Scripted::CreatePrConflict(
                r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for test-owner:feat/with/slashes."}]}"#.to_string(),
            ),
            Scripted::ListPulls {
                body: existing,
                expected_head: "test-owner%3Afeat%2Fwith%2Fslashes".to_string(),
            },
        ]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");

        // The fake server's path-must-contain assertion fires inside the
        // request handler — if the client didn't produce the exact encoded
        // head, `handle.join()` would never return (or would panic). So
        // the success of this test IS the URL-encoding pin.
        let _url =
            create_pr_for_mesh_blocking_with_client(&client, &mesh_path, "title", "body", "main")
                .expect("recovery GET must use percent-encoded head");
        handle.join().expect("server");
    }

    // ----- session_id path (Finding 6) ----------------------------------
    //
    // `create_pr_blocking_with_client` is the documented test seam for the
    // production `create_pr` command. The PR only exercised the
    // `create_pr_for_mesh_blocking_with_client` boundary; this drives the
    // full DB → worktree → GitHub path so a regression in the session
    // translation is caught here rather than at e2e time.

    /// Install this test's private database and hand back the guard the test
    /// body holds for its remaining statements.
    fn ensure_pr_blocking_db() -> crate::db::test_support::IsolatedDbGuard {
        crate::db::test_support::isolated()
    }

    /// Insert a mesh row pointing at `path` and an agent_node row in
    /// `use_worktree` mode, then create the worktree the production path
    /// expects (`<root>/.claude/worktrees/<name>`). Returns
    /// `(tmp, session_id, branch)` — caller MUST hold `tmp` for the
    /// node's lifetime.
    fn make_session_node(mesh_name: &str, origin_url: &str, branch: &str) -> (TempGitRepo, i64) {
        let tmp = TempGitRepo::new();
        let root = tmp.path().to_path_buf();
        // Init a real git repo + commit + ensure HEAD sits on a named
        // `main` branch. `init_repo_for_test` commits via `Some("HEAD")`
        // on an unborn HEAD; the resulting ref shape depends on git2
        // version (sometimes symbolic to `refs/heads/<default>`, sometimes
        // a direct SHA ref). Belt-and-suspenders: read whatever `head()`
        // returns, then explicitly create+checkout a `main` branch
        // anchored at the commit so the worktree below has a real
        // branch ref to fork from.
        let repo = init_repo_for_test(&root, &[("README.md", "init\n")]);
        let head = repo.head().expect("head exists after commit");
        let head_commit = head.peel_to_commit().expect("head is a commit");
        // If HEAD is already on `main`, leave it. Otherwise create
        // `main` and re-anchor HEAD to it.
        let head_is_main = head.shorthand().map(|s| s == "main").unwrap_or(false);
        if !head_is_main {
            repo.set_head_detached(head_commit.id()).expect("detach");
            repo.branch("main", &head_commit, true)
                .expect("create main");
        }
        repo.set_head("refs/heads/main").expect("set HEAD to main");
        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
            .expect("checkout main");
        // Set the origin remote — required so `repo_info` can resolve
        // owner/repo for the GitHub call. Set on the main repo; the
        // worktree shares the same `.git` and inherits the remote.
        repo.remote_set_url("origin", origin_url)
            .expect("set origin");
        // Create the branched worktree at the production path.
        let wt_dir = root.join(".claude").join("worktrees").join("agent-1");
        create_git_worktree(
            root.to_str().unwrap(),
            wt_dir.to_str().unwrap(),
            branch,
            "branched", // worktree_mode
            "main",
        )
        .expect("worktree creation must succeed");
        // Insert a mesh + node row so `db::get_agent_node_by_id` resolves.
        let mesh = crate::db::create_mesh(mesh_name, root.to_str().unwrap()).expect("create_mesh");
        let node = crate::db::create_agent_node(
            mesh.id,
            "agent-1",
            root.to_str().unwrap(),
            "main", // base_branch
            crate::models::EnvType::Windows,
            "claude",
            Some("agent-1"),
            None,
            None,
            None,
            true,
            None,
            None,
            None,
        )
        .expect("create_agent_node");
        (tmp, node.id)
    }

    /// Pin the full session_id path end-to-end. A duplicate-create 422
    /// from GitHub must be recovered via `find_open_pr_for_branch` and
    /// return the existing PR's URL. The test proves:
    ///   - the DB row is found
    ///   - the worktree branch is read (`feat/771` is the agent's branch,
    ///     NOT `main`)
    ///   - the recovery GET uses the right `head=` parameter
    ///
    /// Without a test here, a regression that always used `node.branch`
    /// (the base, "main") would only surface in the production app.
    #[test]
    fn create_pr_blocking_recovers_from_duplicate_create_422() {
        use std::sync::atomic::Ordering;

        let _db = ensure_pr_blocking_db();

        let (_tmp, session_id) = make_session_node(
            "pr-blocking-mesh",
            "https://github.com/test-owner/test-repo.git",
            "feat/771",
        );
        let existing = existing_pr_json(771, "https://github.com/test-owner/test-repo/pull/771");
        let (base, count, handle) = fake_server(vec![
            Scripted::CreatePrConflict(
                r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for test-owner:feat/771."}]}"#.to_string(),
            ),
            Scripted::ListPulls {
                body: existing,
                expected_head: "test-owner%3Afeat%2F771".to_string(),
            },
        ]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");

        let url = create_pr_blocking_with_client(&client, session_id, "new title", "new body")
            .expect("must return the existing PR's URL on 422 recovery");

        assert_eq!(
            url, "https://github.com/test-owner/test-repo/pull/771",
            "must return the EXISTING PR's html_url, not the freshly-attempted one"
        );
        handle.join().expect("server");
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "POST (422) + GET (recovery) — count == 1 means the recovery GET was skipped"
        );
    }

    // ----- structured 422 detection (Finding 2) -------------------------

    /// A 422 whose body contains the literal phrase "already exists" in a
    /// non-error field (e.g. an echoed title) must NOT trigger the
    /// recovery path. The previous substring-on-raw-body check would
    /// falsely fire here; the structured `errors[].message` parse must
    /// distinguish real duplicate-create errors from validation echoes.
    #[test]
    fn create_pr_for_mesh_does_not_recover_on_unrelated_422() {
        use std::sync::atomic::Ordering;

        let (_guard, mesh_path) =
            init_repo_on_branch("https://github.com/test-owner/test-repo.git", "feat/echo");
        // 422 with a body that contains "already exists" only as a
        // echoed input field — GitHub's "Validation Failed" envelope
        // names a different field ("No commits between main and feat/echo")
        // and has no "already exists" in `errors[].message`.
        let (base, count, handle) = fake_server(vec![Scripted::CreatePrError(
            422,
            r#"{"message":"Validation Failed","errors":[{"message":"No commits between main and feat/echo."}]}"#.to_string(),
        )]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");

        let err = create_pr_for_mesh_blocking_with_client(
            &client,
            &mesh_path,
            "title with 'already exists' in it",
            "body",
            "main",
        )
        .expect_err("422 without duplicate-create message must propagate");

        assert!(
            err.contains("422") || err.contains("No commits"),
            "error must surface the real GitHub diagnostic, got: {err}"
        );
        handle.join().expect("server");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "exactly one POST — non-duplicate 422s must not trigger a recovery GET"
        );
    }

    // ----- fork head handling (Finding 1) -------------------------------

    /// `find_open_pr_for_branch` must NOT prepend `owner:` when the
    /// caller has already supplied a qualified `head` (e.g. a fork
    /// PR's `fork_user:branch`). The previous implementation
    /// unconditionally formatted `"{owner}:{branch}"`, which on a fork
    /// produced `upstream:fork_user:branch` — never matched anything,
    /// and the 422 recovery then surfaced the GitHub 422 as opaque
    /// error rather than the existing PR's URL.
    ///
    /// The fake server asserts the request line contains the EXACT
    /// qualified head (`fork-user%3Afeat%2Ffork`) and not
    /// `upstream-owner%3Afork-user%3Afeat%2Ffork`.
    #[test]
    fn find_open_pr_for_branch_does_not_double_prefix_fork_head() {
        use std::sync::atomic::Ordering;

        // We exercise the seam directly so the assertion is at the
        // exact layer the bug lived in — `find_open_pr_for_branch`
        // itself, not its `create_pull_request_idempotent` caller.
        let (base, count, handle) = fake_server(vec![Scripted::ListPulls {
            body: existing_pr_json(42, "https://github.com/upstream-owner/repo/pull/42"),
            // The exact percent-encoded form the client must produce.
            // NO `upstream-owner:` prefix — the head is pre-qualified.
            expected_head: "fork-user%3Afeat%2Ffork".to_string(),
        }]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");

        let pr = client
            .find_open_pr_for_branch("upstream-owner", "repo", "fork-user:feat/fork")
            .expect("must succeed")
            .expect("must find the existing fork PR");

        assert_eq!(pr.number, 42);
        handle.join().expect("server");
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    // ----- merge-method vocabulary (merge-strategy dropdown) -----------

    #[test]
    fn normalise_merge_method_accepts_the_three_github_verbs() {
        assert_eq!(normalise_merge_method(Some("squash".into())), "squash");
        assert_eq!(normalise_merge_method(Some("merge".into())), "merge");
        assert_eq!(normalise_merge_method(Some("rebase".into())), "rebase");
    }

    #[test]
    fn normalise_merge_method_falls_back_to_squash_for_absent_or_unknown() {
        // Older clients (PrPill menu, mobile HTTP route) don't send a
        // method at all; an unrecognised one must never reach GitHub's
        // API verbatim. Both collapse to the historical squash behaviour.
        assert_eq!(normalise_merge_method(None), "squash");
        assert_eq!(normalise_merge_method(Some("octopus".into())), "squash");
        assert_eq!(normalise_merge_method(Some(String::new())), "squash");
    }
    // ----- node-scoped create-PR (issue #2024 rank 4, issue #1567) ------
    //
    // The bug: the mobile create-PR request carried only a mesh id, and the
    // route resolved `mesh.path`. For a mesh whose root sits on `main` that
    // published `main -> main`; for a root sitting on an unrelated feature it
    // published the wrong branch entirely — while the sheet displayed the
    // NODE's branch.
    //
    // These tests drive the real production resolver against a real git repo
    // (mesh root + linked agent worktree), so a regression to mesh-root
    // resolution shows up as a wrong `head_branch`, not as a passing mock.

    /// Mesh root on `root_branch`, agent worktree on `node_branch`, mesh
    /// `base_ref` as given. Returns `(guard, mesh_id, node_id)`; the caller
    /// MUST hold `guard` — it owns both repositories.
    ///
    /// The worktree is cut from `main` BEFORE the root is moved onto
    /// `root_branch`, which is the production order: an agent branches from
    /// the base ref, not from wherever the mesh root happens to be parked.
    #[allow(clippy::too_many_arguments)]
    fn make_mesh_root_and_node(
        origin_url: &str,
        root_branch: &str,
        node_branch: &str,
        base_ref: &str,
    ) -> (TempGitRepo, i64, i64) {
        let tmp = TempGitRepo::new();
        let root = tmp.path().to_path_buf();
        let repo = init_repo_for_test(&root, &[("README.md", "init\n")]);
        let head = repo.head().expect("head exists after commit");
        let head_commit = head.peel_to_commit().expect("head is a commit");
        // Anchor a real `main` branch. `init_repo_for_test` commits via
        // `Some("HEAD")` on an unborn HEAD, and the resulting ref shape is
        // git2-version dependent, so `main` may or may not already exist.
        let head_is_main = head.shorthand().map(|s| s == "main").unwrap_or(false);
        if !head_is_main {
            repo.set_head_detached(head_commit.id()).expect("detach");
            repo.branch("main", &head_commit, true)
                .expect("create main");
        }
        repo.set_head("refs/heads/main").expect("set HEAD to main");
        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
            .expect("checkout main");
        repo.remote_set_url("origin", origin_url)
            .expect("set origin");

        let wt_dir = root.join(".claude").join("worktrees").join("agent-1");
        create_git_worktree(
            root.to_str().unwrap(),
            wt_dir.to_str().unwrap(),
            node_branch,
            "branched",
            "main",
        )
        .expect("worktree creation must succeed");

        // Park the mesh root on its own branch. This is the state that used
        // to decide what the PR published.
        if root_branch != "main" {
            repo.set_head_detached(head_commit.id()).expect("detach");
            repo.branch(root_branch, &head_commit, true)
                .expect("create root branch");
            repo.set_head(&format!("refs/heads/{root_branch}"))
                .expect("set root HEAD");
            repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
                .expect("checkout root branch");
        }

        let mesh = crate::db::create_mesh_with_base_ref(
            "pr-source-mesh",
            root.to_str().unwrap(),
            base_ref,
        )
        .expect("create_mesh_with_base_ref");
        let node = crate::db::create_agent_node(
            mesh.id,
            "agent-1",
            root.to_str().unwrap(),
            "main",
            crate::models::EnvType::Windows,
            "claude",
            Some("agent-1"),
            None,
            None,
            None,
            true,
            None,
            None,
            None,
        )
        .expect("create_agent_node");
        (tmp, mesh.id, node.id)
    }

    #[test]
    fn pr_source_uses_node_worktree_when_mesh_root_is_on_main() {
        let _db = ensure_pr_blocking_db();
        let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
            "https://github.com/test-owner/test-repo.git",
            "main",
            "agent/fix-x",
            "origin/main",
        );

        let resolved = resolve_pr_source_for_node(mesh_id, node_id).expect("resolution succeeds");

        assert_eq!(
            resolved.source.head_branch, "agent/fix-x",
            "the PR source is the agent worktree's branch, never the mesh root's `main`"
        );
        assert_eq!(resolved.source.base_branch, "main");
        assert_eq!(
            resolved.owner_repo,
            Some(("test-owner".to_string(), "test-repo".to_string())),
            "the node worktree inherits the root repo's origin remote"
        );
    }

    #[test]
    fn pr_source_uses_node_worktree_when_mesh_root_is_on_another_feature() {
        let _db = ensure_pr_blocking_db();
        let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
            "https://github.com/test-owner/test-repo.git",
            "unrelated/other",
            "agent/fix-x",
            "origin/main",
        );

        let resolved = resolve_pr_source_for_node(mesh_id, node_id).expect("resolution succeeds");

        assert_eq!(
            resolved.source.head_branch, "agent/fix-x",
            "a mesh root parked on `unrelated/other` must not become the PR source"
        );
        assert_eq!(resolved.source.base_branch, "main");
    }

    #[test]
    fn pr_source_derives_base_from_mesh_base_ref_not_a_hardcoded_main() {
        let _db = ensure_pr_blocking_db();
        let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
            "https://github.com/test-owner/test-repo.git",
            "trunk",
            "agent/fix-x",
            "origin/trunk",
        );

        let resolved = resolve_pr_source_for_node(mesh_id, node_id).expect("resolution succeeds");

        assert_eq!(
            resolved.source.base_branch, "trunk",
            "a mesh configured against origin/trunk must not be published into main"
        );
        assert_eq!(resolved.source.head_branch, "agent/fix-x");
    }

    #[test]
    fn pr_source_rejects_node_belonging_to_another_mesh() {
        let _db = ensure_pr_blocking_db();
        let (_tmp_a, mesh_a, node_in_a) = make_mesh_root_and_node(
            "https://github.com/test-owner/test-repo.git",
            "main",
            "agent/fix-x",
            "origin/main",
        );
        let (_tmp_b, mesh_b, _node_in_b) = make_mesh_root_and_node(
            "https://github.com/test-owner/other-repo.git",
            "main",
            "agent/fix-y",
            "origin/main",
        );

        let err = resolve_pr_source_for_node(mesh_b, node_in_a).expect_err("ownership must fail");
        // The status is the contract the HTTP route depends on: ownership is
        // the one case that is genuinely a 403 (#2190 review).
        assert_eq!(err.status(), "403 Forbidden");
        assert!(matches!(err, PrSourceError::NotOwned { .. }));
        assert!(
            err.to_string().contains("does not belong to mesh"),
            "error must name the ownership violation, got: {err}"
        );
        assert!(mesh_a != mesh_b, "the two meshes must actually differ");
    }

    #[test]
    fn pr_source_rejects_node_branch_equal_to_the_base_ref() {
        let _db = ensure_pr_blocking_db();
        // The node worktree sits on `agent/fix-x` and the mesh's base_ref
        // points at that same branch — the "nothing to compare" case a
        // misconfigured mesh produces. It must fail before GitHub is ever
        // called rather than opening a self-targeting PR. (Pointing the
        // base at `main` instead would fail earlier, in worktree creation:
        // the root already owns `main`.)
        let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
            "https://github.com/test-owner/test-repo.git",
            "main",
            "agent/fix-x",
            "origin/agent/fix-x",
        );

        let err = resolve_pr_source_for_node(mesh_id, node_id).expect_err("same-branch must fail");
        // A validation failure, NOT an auth failure. Reporting it as 403 was
        // a live bug: the mobile client treats 403 as an expired session and
        // wipes the credentials (#2190 review).
        assert_eq!(err.status(), "422 Unprocessable Entity");
        assert!(matches!(err, PrSourceError::SameBranch { .. }));
        assert!(
            err.to_string().contains("nothing to compare"),
            "error must explain the same-branch case, got: {err}"
        );
    }

    #[test]
    fn local_base_branch_strips_a_configured_remote_but_not_a_real_slash_branch() {
        // The prefix is stripped only when the repository actually has that
        // remote. There is no syntax that tells `origin/main` from
        // `feature/x`, so character-shape guessing would rewrite the latter
        // into `x` and target a branch that does not exist.
        let remotes = vec!["origin".to_string(), "upstream".to_string()];
        // The remote qualifier must go: GitHub's `base=` wants a branch name.
        assert_eq!(local_base_branch("origin/main", &remotes), "main");
        assert_eq!(local_base_branch("origin/trunk", &remotes), "trunk");
        assert_eq!(local_base_branch("upstream/release", &remotes), "release");
        assert_eq!(local_base_branch("  origin/main  ", &remotes), "main");
        // Already-local, or a branch whose own name contains a slash.
        assert_eq!(local_base_branch("main", &remotes), "main");
        assert_eq!(local_base_branch("feature/x", &remotes), "feature/x");
        // A prefix that is not a configured remote is left alone.
        assert_eq!(local_base_branch("origin/main", &[]), "origin/main");
        assert_eq!(local_base_branch("origin/", &remotes), "origin/");
        assert_eq!(local_base_branch("", &remotes), "");
    }

    /// The wire-level proof: the `head=` GitHub is asked about is the NODE's
    /// branch. The scripted fake asserts the exact percent-encoded head in
    /// the request line, so a regression to mesh-root resolution fails here
    /// instead of quietly publishing the root's branch.
    #[test]
    fn create_pr_for_node_source_asks_github_about_the_node_branch() {
        use std::sync::atomic::Ordering;

        let _db = ensure_pr_blocking_db();
        let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
            "https://github.com/test-owner/test-repo.git",
            "main",
            "agent/fix-x",
            "origin/main",
        );
        let existing = existing_pr_json(900, "https://github.com/test-owner/test-repo/pull/900");
        let (base, count, handle) = fake_server(vec![
            Scripted::CreatePrConflict(
                r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for test-owner:agent/fix-x."}]}"#.to_string(),
            ),
            Scripted::ListPulls {
                body: existing,
                // `:` -> %3A, `/` -> %2F. The fake asserts this exact value
                // appears in the recovery GET's request line.
                expected_head: "test-owner%3Aagent%2Ffix-x".to_string(),
            },
        ]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");

        let result = create_pr_for_node_source_blocking_with_client(
            &client, mesh_id, node_id, "title", "body", None, None,
        )
        .expect("duplicate-create recovery must succeed");

        assert_eq!(
            result.url,
            "https://github.com/test-owner/test-repo/pull/900"
        );
        assert_eq!(
            result.head_branch, "agent/fix-x",
            "the echoed source must be the node worktree's branch"
        );
        assert_eq!(result.base_branch, "main");
        handle.join().expect("server");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn create_pr_for_node_source_rejects_a_stale_expected_head() {
        let _db = ensure_pr_blocking_db();
        let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
            "https://github.com/test-owner/test-repo.git",
            "main",
            "agent/fix-x",
            "origin/main",
        );
        let client = GitHubClient::for_test("http://127.0.0.1:1", "fake-token").expect("client");

        let err = create_pr_for_node_source_blocking_with_client(
            &client,
            mesh_id,
            node_id,
            "title",
            "body",
            None,
            Some("agent/moved-on"),
        )
        .expect_err("a worktree that moved since preview must fail");

        assert!(
            err.to_string().contains("agent/fix-x") && err.to_string().contains("agent/moved-on"),
            "error must name both the actual and the previewed branch, got: {err}"
        );
        // The worktree moved after preview: a stale client view, not an
        // authorization problem (#2190 review).
        assert_eq!(err.status(), "422 Unprocessable Entity");
        assert!(matches!(err, PrSourceError::StaleHead { .. }));
    }

    #[test]
    fn create_pr_for_node_source_rejects_a_base_equal_to_the_node_branch() {
        let _db = ensure_pr_blocking_db();
        let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
            "https://github.com/test-owner/test-repo.git",
            "main",
            "agent/fix-x",
            "origin/main",
        );
        let client = GitHubClient::for_test("http://127.0.0.1:1", "fake-token").expect("client");

        let err = create_pr_for_node_source_blocking_with_client(
            &client,
            mesh_id,
            node_id,
            "title",
            "body",
            Some("agent/fix-x"),
            None,
        )
        .expect_err("base == source must fail");

        assert!(
            err.to_string().contains("nothing to compare"),
            "error must explain the same-branch case, got: {err}"
        );
        assert_eq!(err.status(), "422 Unprocessable Entity");
    }

    #[test]
    fn create_pr_for_node_source_rejects_cross_mesh_node_before_calling_github() {
        let _db = ensure_pr_blocking_db();
        let (_tmp_a, _mesh_a, node_in_a) = make_mesh_root_and_node(
            "https://github.com/test-owner/test-repo.git",
            "main",
            "agent/fix-x",
            "origin/main",
        );
        let (_tmp_b, mesh_b, _node_in_b) = make_mesh_root_and_node(
            "https://github.com/test-owner/other-repo.git",
            "main",
            "agent/fix-y",
            "origin/main",
        );
        // Port 1 refuses connections, so if ownership were not checked first
        // this test would fail with a connection error instead of the
        // ownership message.
        let client = GitHubClient::for_test("http://127.0.0.1:1", "fake-token").expect("client");

        let err = create_pr_for_node_source_blocking_with_client(
            &client, mesh_b, node_in_a, "title", "body", None, None,
        )
        .expect_err("cross-mesh node must be rejected");

        assert!(
            err.to_string().contains("does not belong to mesh"),
            "ownership must be checked before any GitHub call, got: {err}"
        );
        assert_eq!(err.status(), "403 Forbidden");
    }

    #[test]
    fn pr_source_missing_node_is_a_404_not_a_server_fault() {
        // A node id with no DB row used to surface rusqlite's "Query returned
        // no rows" and answer 500; `create` already answered 404. The typed
        // error keeps the two routes consistent (#2190 review).
        let _db = ensure_pr_blocking_db();
        let err = resolve_pr_source_for_node(1, 987_654_321).expect_err("unknown node must fail");
        assert_eq!(err.status(), "404 Not Found");
        assert!(matches!(err, PrSourceError::NotFound(987_654_321)));
    }
}
