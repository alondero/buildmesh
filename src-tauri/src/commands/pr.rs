//! GitHub workflow via direct REST API calls (no `gh` CLI dependency)

use crate::db;
use crate::env;
use crate::models::SessionStatus;
use crate::services::github::{self, GitHubClient, GitHubError, PullRequest};
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
/// Mirrors `services::github::PrFile`; the panel's Center Diff Overlay
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
/// Returns an empty list if the mesh has no GitHub remote (or any error
/// resolving one), with a `warn!` capturing the reason. The modal degrades
/// gracefully — see [`resolve_github_owner_repo`] for the error wording the
/// sibling spawn endpoint surfaces directly.
#[command]
pub async fn get_repo_issues(mesh_id: i64) -> Result<Vec<GitHubIssue>, String> {
    crate::commands::run_blocking("get_repo_issues", move || get_repo_issues_blocking(mesh_id)).await
}

/// Sync core for [`get_repo_issues`]. Kept as a plain fn so the mobile HTTP
/// route (`http::routes::issues`) can call it directly; the Tauri command
/// wraps it in `spawn_blocking` (see [`crate::commands::run_blocking`]).
pub(crate) fn get_repo_issues_blocking(mesh_id: i64) -> Result<Vec<GitHubIssue>, String> {
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

    Ok(issues.into_iter().map(|issue| {
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
            // Downcast internal `i64` → wire `i32` (issue numbers fit
            // comfortably in i32's ~2.1B max; matches the existing
            // `#[ts(as = "i32")]` convention on the wire struct's other
            // integer fields).
            blocked_by,
        }
    }).collect())
}

/// Add or remove a label on a mesh's GitHub issue (issue #979).
///
/// Backs the Issues Probe's trigger-label toggle — a click on the green
/// `✓ buildmesh:run` chip removes the label (with confirm prompt), a
/// click on the neutral `+ buildmesh:run` slot adds it. The UI does the
/// optimistic update + rollback itself; the backend just needs to be a
/// thin pass-through to GitHub's `POST`/`DELETE /repos/{o}/{r}/issues/{n}/labels`
/// endpoints.
///
/// `action` is `"add"` or `"remove"` — kept as a string rather than an
/// enum so the wire shape mirrors the simplest possible JS call site
/// (`setIssueLabel(meshId, issueNumber, label, "add")`). Anything other
/// than `"remove"` is treated as add; this collapses typos to the safer
/// idempotent direction (add returns 200 for an already-present label,
/// so a misclick that re-applies is a no-op).
///
/// Errors are surfaced verbatim: `LabelNotFound` from the client becomes
/// the string `"Label \`buildmesh:run\` doesn't exist on the repo — create
/// it on GitHub first"` (via the `Display` impl), which the frontend toasts
/// directly. The frontend reverts its optimistic toggle on any error.
#[command]
pub async fn set_issue_label(
    mesh_id: i64,
    issue_number: i64,
    label: String,
    action: String,
) -> Result<(), String> {
    crate::commands::run_blocking("set_issue_label", move || {
        set_issue_label_blocking(mesh_id, issue_number, label, action)
    })
    .await
}

/// Sync core for [`set_issue_label`]. See [`get_repo_issues_blocking`] for
/// the split rationale (Tauri `#[command(async)]` wraps a sync core via
/// `spawn_blocking` — issue #762).
pub(crate) fn set_issue_label_blocking(
    mesh_id: i64,
    issue_number: i64,
    label: String,
    action: String,
) -> Result<(), String> {
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let (owner, repo) = resolve_github_owner_repo(&mesh)?;

    let client = GitHubClient::new().map_err(|e| e.to_string())?;

    // Collapse anything that isn't an explicit "remove" to add — see the
    // doc comment on `set_issue_label` for the rationale. The match is
    // case-insensitive (defensive) so `"Add"`/`"ADD"` work too.
    if action.eq_ignore_ascii_case("remove") {
        client
            .remove_issue_label(&owner, &repo, issue_number, &label)
            .map_err(|e| e.to_string())
    } else {
        client
            .add_issue_label(&owner, &repo, issue_number, &label)
            .map_err(|e| e.to_string())
    }
}

/// Get pull requests for a mesh, filtered by `state` (`"open"` or `"closed"`).
/// Mirrors [`get_repo_issues`]: degrades to an empty list (with a `warn!`) when
/// the mesh has no GitHub origin, so the panel renders an empty state rather
/// than an error.
///
/// Issue #1529: one cohesive summary query — list fields plus mergeability
/// ride inline via the GraphQL PR-summaries connection (O(pages), not O(PRs)).
/// The panel consumes this single call and never orchestrates per-row
/// enrichment.
#[command]
pub async fn get_repo_pulls(mesh_id: i64, state: String) -> Result<Vec<GitHubPullRequest>, String> {
    crate::commands::run_blocking("get_repo_pulls", move || get_repo_pulls_blocking(mesh_id, state)).await
}

/// Sync core for [`get_repo_pulls`] — see [`get_repo_issues_blocking`] for the
/// split rationale.
pub(crate) fn get_repo_pulls_blocking(mesh_id: i64, state: String) -> Result<Vec<GitHubPullRequest>, String> {
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
    let prs = client
        .list_pr_summaries(&owner, &repo, state)
        .map_err(|e| e.to_string())?;

    Ok(prs.into_iter().map(|pr| GitHubPullRequest {
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
        mergeable: pr.mergeable,
        mergeable_state: pr.mergeable_state,
    }).collect())
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
pub(crate) fn get_pr_mergeability_blocking(mesh_id: i64, pr_number: i64) -> Result<PrMergeability, String> {
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let (owner, repo) = resolve_github_owner_repo(&mesh)?;

    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    let (mergeable, mergeable_state) = client
        .pull_request_mergeability(&owner, &repo, pr_number)
        .map_err(|e| e.to_string())?;

    Ok(PrMergeability { mergeable, mergeable_state })
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
fn mergeability_entries<F>(
    pr_numbers: Vec<i64>,
    probe: F,
) -> Vec<PrMergeabilityEntry>
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
/// button on the PR tab (issue #421). One call returns the full file list
/// (≤ 100 — GitHub's default per_page) with each file's unified-diff
/// `patch`. The Center Diff Overlay parses the patch line-by-line to
/// colour +/−/context rows in place. Mirrors the other PR commands:
/// resolves owner/repo via the mesh's `origin` remote, hits the GitHub
/// REST API, and forwards the HTTP error verbatim.
#[command]
pub async fn get_pr_files(mesh_id: i64, pr_number: i64) -> Result<Vec<PrFileEntry>, String> {
    crate::commands::run_blocking("get_pr_files", move || get_pr_files_blocking(mesh_id, pr_number)).await
}

/// Sync core for [`get_pr_files`] — see [`get_repo_issues_blocking`].
pub(crate) fn get_pr_files_blocking(mesh_id: i64, pr_number: i64) -> Result<Vec<PrFileEntry>, String> {
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let (owner, repo) = resolve_github_owner_repo(&mesh)?;

    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    let files = client
        .list_pr_files(&owner, &repo, pr_number)
        .map_err(|e| e.to_string())?;

    Ok(files.into_iter().map(|f| PrFileEntry {
        filename: f.filename,
        status: f.status,
        additions: f.additions,
        deletions: f.deletions,
        patch: f.patch,
        previous_filename: f.previous_filename,
    }).collect())
}

/// Create a PR for the node
#[command]
pub async fn create_pr(
    session_id: i64,
    title: String,
    body: String,
) -> Result<String, String> {
    crate::commands::run_blocking("create_pr", move || create_pr_blocking(session_id, title, body)).await
}

/// Sync core for [`create_pr`] — see [`get_repo_issues_blocking`].
pub(crate) fn create_pr_blocking(
    session_id: i64,
    title: String,
    body: String,
) -> Result<String, String> {
    let node = db::get_agent_node_by_id(session_id)
        .map_err(|e| e.to_string())?;

    let base_branch = &node.branch;

    // Read the branch from the worktree (if any) — the mesh root is on the
    // Base Ref for worktree nodes, which would create a "main → main" PR.
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
#[command]
pub async fn create_pr_for_mesh(
    mesh_path: String,
    title: String,
    body: String,
    base_branch: String,
) -> Result<String, String> {
    crate::commands::run_blocking("create_pr_for_mesh", move || {
        create_pr_for_mesh_blocking(mesh_path, title, body, base_branch)
    })
    .await
}

/// Sync core for [`create_pr_for_mesh`] — see [`get_repo_issues_blocking`].
pub(crate) fn create_pr_for_mesh_blocking(
    mesh_path: String,
    title: String,
    body: String,
    base_branch: String,
) -> Result<String, String> {
    let info = repo_info(&mesh_path)?;
    if info.branch.is_empty() || info.branch == base_branch {
        return Err(format!(
            "Current branch '{}' is the same as Base Ref '{}' — nothing to compare",
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
pub async fn merge_pr(pr_url: String) -> Result<String, String> {
    crate::commands::run_blocking("merge_pr", move || merge_pr_blocking(pr_url)).await
}

/// Sync core for [`merge_pr`] — see [`get_repo_issues_blocking`].
pub(crate) fn merge_pr_blocking(pr_url: String) -> Result<String, String> {
    let (owner, repo, pr_number) = parse_pr_url(&pr_url)
        .ok_or_else(|| format!("Could not parse PR URL: {}", pr_url))?;

    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    client.merge_pull_request(&owner, &repo, pr_number)
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
#[command]
pub async fn get_open_pr_for_node(node_id: i64) -> Result<Option<OpenPr>, String> {
    crate::commands::run_blocking("get_open_pr_for_node", move || get_open_pr_for_node_blocking(node_id)).await
}

/// Sync core for [`get_open_pr_for_node`] — see [`get_repo_issues_blocking`].
pub(crate) fn get_open_pr_for_node_blocking(node_id: i64) -> Result<Option<OpenPr>, String> {
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
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

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
        assert!(issue.blocked_by.is_empty(), "missing blocked_by defaults to empty vec");
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
        };
        let json = serde_json::to_string(&original).expect("serialise");
        let parsed: GitHubIssue = serde_json::from_str(&json).expect("re-parse");
        assert_eq!(parsed.blocked_by, vec![481, 482, 483]);
        assert_eq!(parsed.number, 482);
        assert_eq!(parsed.labels, vec!["bug".to_string()]);
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
        assert_eq!(pr.mergeable, None, "missing mergeable defaults to None (unknown)");
        assert_eq!(pr.mergeable_state, "", "missing mergeable_state defaults to empty");
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
        let pr: GitHubPullRequest =serde_json::from_str(json).expect("partial wire shape parses");
        assert_eq!(pr.head_ref, "feat/legacy");
        assert_eq!(pr.head_repo_owner, "", "missing head_repo_owner defaults to empty");
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
        let entry: PrMergeabilityEntry =
            serde_json::from_str(json).expect("null mergeable parses");
        assert_eq!(entry.mergeable, None, "null must stay None, not coerce to Some(false)");
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
        let entry: PrMergeabilityEntry =
            serde_json::from_str(json).expect("error state parses");
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
        assert!(calls.borrow().is_empty(), "probe must not fire on empty input");
    }

    /// All-success path: each probe succeeds, the entry carries the
    /// probed `mergeable` + `mergeable_state` AND the request's PR
    /// number. The `number` round-trip is the load-bearing piece — a
    /// future refactor that drops the closure arg or hard-codes the
    /// number would silently desync the batched response from the
    /// frontend's PR list.
    #[test]
    fn mergeability_entries_preserves_pr_number_on_success() {
        let entries = mergeability_entries(
            vec![201, 202, 204],
            |n| match n {
                201 => Ok((Some(true), "clean".into())),
                202 => Ok((Some(false), "dirty".into())),
                204 => Ok((None, "unknown".into())),
                _ => unreachable!(),
            },
        );
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
        let entries = mergeability_entries(
            vec![1, 2, 3],
            |n| match n {
                1 => Ok((Some(true), "clean".into())),
                2 => Err(GitHubError::Api(404, "Not Found".into())),
                3 => Ok((Some(false), "dirty".into())),
                _ => unreachable!(),
            },
        );
        assert_eq!(entries.len(), 3, "batch must carry one entry per PR even when one fails");
        // The success entries round-trip unchanged.
        assert_eq!(entries[0].number, 1);
        assert_eq!(entries[0].mergeable, Some(true));
        // The failed entry becomes the "checking" sentinel.
        assert_eq!(entries[1].number, 2, "failed entry must still carry the PR number");
        assert_eq!(entries[1].mergeable, None, "failed entry must report mergeable: None");
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
        let (base, count, handle) =
            fake_server(vec![Scripted::Page(open, false, None)]);
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
        assert_eq!(url, Some("https://github.com/alondero/buildmesh".to_string()));
    }

    #[test]
    fn github_url_for_path_ssh_origin() {
        let (_guard, path) = init_repo_with_origin("git@github.com:alondero/buildmesh.git");
        let url = github_url_for_path(&path).expect("github_url_for_path should succeed");
        assert_eq!(url, Some("https://github.com/alondero/buildmesh".to_string()));
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
        assert_eq!(url, None, "GitLab URLs must collapse to None so the menu hides the item");
    }

    #[test]
    fn github_url_for_path_no_origin_is_none() {
        let (_guard, path) = init_repo_with_commit();
        let url = github_url_for_path(&path).expect("no origin should not error");
        assert_eq!(url, None, "repos with no origin remote must collapse to None");
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
            "PR chip looks up head=<branch>; the agent's working branch is the worktree name, not the mesh's Base Ref"
        );
        // And — for the bug regression — opening the MESH ROOT itself gives
        // `main`, not `agent-1`. This is the exact mismatch that hid the chip.
        let root_info = repo_info(&node.path).expect("mesh root must open");
        assert_eq!(
            root_info.branch, "main",
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
