//! GitHub issue/PR feeds and batched mergeability queries using the shared GitHubClient.

use super::repository::*;
use crate::commands::pr::{
    GitHubIssue, GitHubIssueFeed, GitHubPullRequest, GitHubPullRequestFeed, PrFileEntry,
    PrFileFeed, PrMergeability, PrMergeabilityEntry,
};
use crate::db;

use crate::services::github::{self, GitHubClient, GitHubError, GitHubPageCompleteness};
/// A feed we never had to read: the mesh has no GitHub remote, so there is
/// nothing that *could* be incomplete. Reported complete so the panel keeps
/// its existing "no issues" empty state rather than showing a truncation
/// warning about a read that never happened.
pub(crate) fn empty_feed_completeness() -> GitHubPageCompleteness {
    GitHubPageCompleteness {
        returned: 0,
        pages_fetched: 0,
        complete: true,
        incomplete_reason: None,
        reported_total: None,
    }
}

/// Sync core for [`get_repo_issues`](crate::commands::pr::get_repo_issues). Kept as a plain fn so the mobile HTTP
/// route (`http::routes::issues`) can call it directly; the Tauri command
/// wraps it in `spawn_blocking` (see [`crate::commands::run_blocking`]).
pub(crate) fn get_repo_issues_blocking(mesh_id: i64) -> Result<GitHubIssueFeed, String> {
    get_repo_issues_filtered_blocking(mesh_id, Some(true))
}

pub(crate) fn get_repo_issues_filtered_blocking(
    mesh_id: i64,
    open: Option<bool>,
) -> Result<GitHubIssueFeed, String> {
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;

    let Some((owner, repo)) = resolve_owner_repo(&mesh.path)? else {
        return Ok(GitHubIssueFeed {
            items: Vec::new(),
            completeness: empty_feed_completeness(),
        });
    };

    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    let page = if open == Some(true) {
        client.list_issues_only_paged(&owner, &repo)
    } else {
        client.list_issues_filtered_paged(&owner, &repo, open)
    }
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

/// Sync core for [`get_repo_pulls`](crate::commands::pr::get_repo_pulls) — see [`get_repo_issues_blocking`](crate::services::github::feeds::get_repo_issues_blocking) for the
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

/// Sync core for [`get_pr_mergeability`](crate::commands::pr::get_pr_mergeability) — see [`get_repo_issues_blocking`](crate::services::github::feeds::get_repo_issues_blocking).
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

/// Sync core for [`get_prs_mergeability`](crate::commands::pr::get_prs_mergeability) — see [`get_repo_issues_blocking`](crate::services::github::feeds::get_repo_issues_blocking).
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
pub(crate) fn mergeability_entries<F>(pr_numbers: Vec<i64>, probe: F) -> Vec<PrMergeabilityEntry>
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

/// Sync core for [`get_pr_files`](crate::commands::pr::get_pr_files) — see [`get_repo_issues_blocking`](crate::services::github::feeds::get_repo_issues_blocking).
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
