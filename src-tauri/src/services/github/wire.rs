//! The GitHub issue / pull-request wire shapes.
//!
//! These types are the IPC contract of the Issues and Pull Requests probes:
//! the desktop Tauri commands and the mobile HTTP routes serialise them,
//! and the TypeScript bindings in `src/types/generated/` are generated from
//! them (issue #359).
//!
//! They live here, beside the workflows that build them, because the
//! workflows are the ones that need to name them: `feeds.rs` maps GitHub's
//! payloads onto them and `pr_workflow.rs` returns them. Keeping the
//! declarations in the command layer made `services/` import
//! `commands::pr` — a service reaching upward into the boundary that is
//! supposed to be above it (issue #2236). The command module imports them
//! from here, and so do the mobile wire fixtures; the routes and the
//! companion CLI reach the workflows in this module.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::pagination::GitHubPageCompleteness;

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
///
/// One feed is one page of 100 PRs. `next_cursor` is the opaque cursor for the
/// page after it (`None` when this was the last page); the client passes it
/// back as `get_repo_pulls`'s `cursor` to append older PRs. The field is the
/// lazy-list contract: without it, callers could only get the whole history
/// at once, which is why a long closed list used to block the first render.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "GitHubPullRequestFeed.ts")]
pub struct GitHubPullRequestFeed {
    pub items: Vec<GitHubPullRequest>,
    pub completeness: GitHubPageCompleteness,
    /// Cursor for the next page, or `None` when the list is exhausted.
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// A PR file list plus its completeness — see [`GitHubIssueFeed`]. A PR with
/// more changed files than one page holds must not render as a whole diff.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "PrFileFeed.ts")]
pub struct PrFileFeed {
    pub items: Vec<PrFileEntry>,
    pub completeness: GitHubPageCompleteness,
}

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
