import { _invoke } from './_invoke';
import type { AgentNode } from '../../types/generated/AgentNode';
import type { GitHubIssue } from '../../types/generated/GitHubIssue';
import type { GitHubIssueFeed } from '../../types/generated/GitHubIssueFeed';
import type { GitHubPullRequestFeed } from '../../types/generated/GitHubPullRequestFeed';
import type { GitHubPullRequest } from '../../types/generated/GitHubPullRequest';
import type { IssueNodeDraft } from '../../types/generated/IssueNodeDraft';
import type { OpenPr } from '../../types/generated/OpenPr';
import type { PrMergeability } from '../../types/generated/PrMergeability';
import type { PrMergeabilityEntry } from '../../types/generated/PrMergeabilityEntry';
import type { PrFileEntry } from '../../types/generated/PrFileEntry';
import type { PrFileFeed } from '../../types/generated/PrFileFeed';

// PR
export const createPr = (sessionId: number, title: string, body: string) =>
  _invoke<string>('create_pr', { sessionId, title, body });

export const mergePr = (prUrl: string, mergeMethod?: string) =>
  _invoke<string>('merge_pr', { prUrl, mergeMethod });

export const getCurrentBranch = (sessionId: number) =>
  _invoke<string>('get_current_branch', { sessionId });

export const checkGhAuth = () =>
  _invoke<boolean>('check_gh_auth');

/** Open PR summary for an agent node — surfaces as the "PR #N" chip.
 *  Generated from the Rust struct in `src-tauri/src/commands/pr.rs` (issue #404). */
export type { OpenPr };

export const getOpenPrForNode = (nodeId: number) =>
  _invoke<OpenPr | null>('get_open_pr_for_node', { nodeId });

/** Return the `https://github.com/{owner}/{repo}` web URL for a mesh's
 *  `origin` remote, or `null` when the origin isn't a GitHub URL (or the
 *  mesh has no origin at all). The return is intentionally a plain
 *  `string | null` — no generated wire type — so the IPC contract stays
 *  a single string (issue #359's "no hand-declared TS interface for a
 *  Rust wire type" rule is preserved trivially). Consumed by the mesh
 *  context menu's "View on GitHub" item and by the Issues / PRs probe
 *  headers' GitHub buttons. */
export const getGitHubUrlForMesh = (meshId: number) =>
  _invoke<string | null>('get_github_url_for_mesh', { meshId });

// GitHub Issues — `GitHubIssue` is generated from the Rust struct
// (src-tauri/src/commands/pr.rs) into src/types/generated/; see top import.
// Re-exported here so existing `import { GitHubIssue } from '../../lib/tauri'`
// call sites keep working. Issue #359.
export type { GitHubIssue };

export const getRepoIssues = (meshId: number) =>
/// Returns the feed wrapper, not a bare array: a paginated read must be able to
/// say it is incomplete instead of letting the panel imply that page 1 is the
/// whole repository (issue #2024 rank 6 / #1528).
  _invoke<GitHubIssueFeed>('get_repo_issues', { meshId });

export const getRepoLabels = (meshId: number) =>
  _invoke<string[]>('get_repo_labels', { meshId });

export const setIssueLabel = (meshId: number, issueNumber: number, label: string, present: boolean) =>
  _invoke<void>('set_issue_label', { meshId, issueNumber, label, present });

// GitHub Pull Requests — `GitHubPullRequest` / `PrMergeability` are generated
// from the Rust structs (src-tauri/src/commands/pr.rs) into
// src/types/generated/; see top import. Re-exported here so the PR probe tab
// can `import { GitHubPullRequest } from '../../lib/tauri'` alongside the issue
// types. Issue #359.
export type { GitHubPullRequest, PrMergeability, PrMergeabilityEntry, PrFileEntry };

/** List PRs for a mesh's repo, filtered by `state` (`'open'` or `'closed'`).
 * Issue #1529: cohesive summary query — list fields plus `mergeable` /
 * `mergeable_state` inline via the GraphQL summaries connection (O(pages)).
 * The panel consumes this single call and never orchestrates per-row
 * enrichment. */
export const getRepoPulls = (meshId: number, state: 'open' | 'closed') =>
  _invoke<GitHubPullRequestFeed>('get_repo_pulls', { meshId, state });

/// Per-PR mergeability enrichment — the `/pulls` list endpoint omits it, so
/// the panel fetches this once per open PR. `mergeable` is `null` while
/// GitHub is still computing the merge. **Deprecated on desktop** — use
/// [`getPrsMergeability`] for the batched call (issue #418); the per-PR
/// shape survives for the mobile HTTP route at
/// `GET /api/meshes/{id}/pulls/{n}/mergeability`.
export const getPrMergeability = (meshId: number, prNumber: number) =>
  _invoke<PrMergeability>('get_pr_mergeability', { meshId, prNumber });

/// Batched PR mergeability (issue #418, reimplemented O(pages) for #1529).
/// **Deprecated on desktop** — the panel now reads `mergeable` inline from
/// [`getRepoPulls`] and never calls this. Survives for backward compat
/// (older/mobile callers): one GraphQL summaries fetch serves the whole
/// batch, missing numbers become the `"error: …"` sentinel.
export const getPrsMergeability = (meshId: number, prNumbers: number[]) =>
  _invoke<PrMergeabilityEntry[]>('get_prs_mergeability', { meshId, prNumbers });

/// List the files changed in a single PR (issue #421). Backed by GitHub's
/// `/pulls/{n}/files` endpoint; one call returns the whole PR with each
/// file's unified-diff `patch`. The Center Diff Overlay parses the patch
/// line-by-line to render +/−/context rows. Distinct from `getRepoPulls` /
/// `getPrMergeability` because the panel needs the diff payload, not just
/// the metadata.
export const getPrFiles = (meshId: number, prNumber: number) =>
  _invoke<PrFileFeed>('get_pr_files', { meshId, prNumber });

export const spawnIssueAgent = (meshId: number, issueNumber: number, issueTitle: string, provider?: string) =>
  _invoke<AgentNode>('spawn_issue_agent', { meshId, issueNumber, issueTitle, provider });

/// Fast acceptance of an issue spawn. The backend commits the `pending` row,
/// starts the slow intent-driven launch in the background, and later emits
/// `node-spawn-completed` or `node-spawn-failed`. The returned draft keeps the
/// existing wire shape for compatibility; callers no longer need to hand the
/// transient prefill to a second IPC command.
export type { IssueNodeDraft };

export const createIssueNode = (meshId: number, issueNumber: number, issueTitle: string, provider?: string, configurationId?: string) =>
  _invoke<IssueNodeDraft>('create_issue_node', { meshId, issueNumber, issueTitle, provider, configurationId });

export const createPrForMesh = (meshPath: string, title: string, body: string, baseBranch: string) =>
  _invoke<string>('create_pr_for_mesh', { meshPath, title, body, baseBranch });

/// One backend-owned acceptance call. The PR row's `+` button is on the
/// `SpawnButtonCluster`; the tab now relies on `create_pr_node` to
/// accept the row and start the intent-driven launch in the background.
///
/// The `headRef` field comes from the GitHub API's `head.ref` (now exposed
/// on `GitHubPullRequest` for this purpose). For fork PRs (issue #443) the
/// stage-2 path adds the fork as a remote (`fork-<login>`) and fetches the
/// head ref from there; the `headRepoOwner` + `headRepoCloneUrl` arguments
/// carry that info from the GitHub list response to the node row.
///
/// `headSha` (issue #444) is the PR's head commit SHA at click time, also
/// exposed on `GitHubPullRequest` via `head_sha`. The backend persists it
/// as `source_pr_pinned_sha` on the new node and verifies the local
/// `origin/<head_ref>` SHA matches it after `git fetch`, emitting a
/// non-fatal `pr_sha_drift` `mesh-sync-warning` on mismatch (force-push
/// / rebase between click-time and spawn-time). An empty `headSha` skips
/// the drift check (same fail-open semantics as `pr_head_unfetchable`).
///
/// Reuses the generated `IssueNodeDraft` type for the return value: the wire
/// shape is identical (flattened `AgentNode` + `prefill`), so no new TS
/// type is generated.
export const createPrNode = (
  meshId: number,
  prNumber: number,
  prTitle: string,
  headRef: string,
  headSha: string,
  provider?: string,
  headRepoOwner?: string,
  headRepoCloneUrl?: string,
  configurationId?: string,
  // Spawn a *reviewer sibling* for the PR (the PR pill's "Spawn reviewer
  // agent" row): a distinct name so it does not share the implementation
  // node's worktree — see `commands::agent::create_pr_node`'s `reviewer`
  // parameter for why sharing one is destructive. Omit for the probe's `+`.
  reviewer?: boolean,
) =>
  _invoke<IssueNodeDraft>('create_pr_node', {
    meshId,
    prNumber,
    prTitle,
    headRef,
    headSha,
    provider,
    headRepoOwner,
    headRepoCloneUrl,
    configurationId,
    reviewer,
  });
