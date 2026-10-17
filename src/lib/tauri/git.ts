import { _invoke } from './_invoke';
import type { BranchInfo } from '../../types/generated/BranchInfo';
import type { DiffHunk } from '../../types/generated/DiffHunk';
import type { DiffLine } from '../../types/generated/DiffLine';
import type { DiffResult } from '../../types/generated/DiffResult';
import type { FileDiff } from '../../types/generated/FileDiff';
import type { FreeResult } from '../../types/generated/FreeResult';
import type { GitBranchStatus } from '../../types/generated/GitBranchStatus';
import type { GitRepoPruneInfo } from '../../types/generated/GitRepoPruneInfo';
import type { GitSummary } from '../../types/generated/GitSummary';
import type { GitSyncResult } from '../../types/generated/GitSyncResult';
import type { HoldingWorktree } from '../../types/generated/HoldingWorktree';
import type { MeshGitStatic } from '../../types/generated/MeshGitStatic';
import type { MeshHealth } from '../../types/generated/MeshHealth';
import type { PendingWorktreeRemoval } from '../../types/generated/PendingWorktreeRemoval';
import type { BlockingProcess } from '../../types/generated/BlockingProcess';
import type { WorktreeCleanupRetryResult } from '../../types/generated/WorktreeCleanupRetryResult';
import type { WorktreeCleanupDismissalResult } from '../../types/generated/WorktreeCleanupDismissalResult';
import type { RestoreResult } from '../../types/generated/RestoreResult';
import type { WorktreeInfo } from '../../types/generated/WorktreeInfo';
import type { GitStatus } from '../../types/generated/GitStatus';

export type DiffLineType = 'context' | 'add' | 'remove';

// Diff types — generated from the Rust structs in `src-tauri/src/models/mod.rs`
// (issue #404). The Rust `status` / `line_type` fields are plain `String`, so
// the generated types emit `string` (a wider union than the hand-written
// versions used to carry); consumers that switch on them still compare fine.
export type { DiffLine, DiffHunk, FileDiff, DiffResult };

/** Change kind vocabulary, shared with `GitStatus.status`. Subset of the
 *  `FileDiff.status` string union the generated type uses. Kept as a hand-typed
 *  alias because it documents the closed set consumers can rely on. */
export type FileDiffStatus =
  | 'added'
  | 'modified'
  | 'deleted'
  | 'renamed'
  | 'untracked';

// Diff
export const diffFiles = (oldPath: string, newPath: string) =>
  _invoke<DiffResult>('diff_files', { oldPath, newPath });

export const diffFileAgainstHead = (sessionPath: string, filePath: string) =>
  _invoke<DiffResult>('diff_file_against_head', { sessionPath, filePath });

// Every file an agent changed since branching (merge-base with mesh base_ref;
// see ADR 0005). One call returns the whole change set for the review panel.
//
// Issue #1181 — cancellation seam: `signal` is accepted so a component
// that issues overlapping `fetchDiff` calls can pass a per-call
// `AbortSignal` and have the local `.then` short-circuit if a newer
// request has superseded it. Tauri 2's `invoke` doesn't yet forward the
// signal to the Rust command, so the *actual* backend cancellation
// (pool-pressure ≤1 per node_id) happens on the Rust side via the
// `DIFF_NODE_CANCEL` map — see `commands::diff::acquire_diff_cancel`.
// The frontend signal exists to (a) drop stale local results so the UI
// doesn't flicker, and (b) be the seam a future Tauri signal-aware
// invoke can plug into without touching call sites.
export const diffNodeAgainstBase = (
  nodeId: number,
  signal?: AbortSignal,
): Promise<DiffResult> => {
  if (signal?.aborted) {
    // Don't even kick off an IPC for a request the caller has already
    // superseded — a freshly aborted controller has nothing to wait
    // for, and starting the network round-trip would just produce a
    // promise we'd discard.
    return Promise.reject(new DOMException('aborted', 'AbortError'));
  }
  return _invoke<DiffResult>('diff_node_against_base', { nodeId });
};

export const diffNodeFileAgainstBase = (
  nodeId: number,
  filePath: string,
  signal?: AbortSignal,
): Promise<DiffResult> => {
  // Issue #1181 — see `diffNodeAgainstBase` for the rationale. Per-file
  // diffs share the same pool-pressure concern (the overlay's rapid
  // file-switching + `git-changed` bursts pile up the same way the
  // review panel does).
  if (signal?.aborted) {
    return Promise.reject(new DOMException('aborted', 'AbortError'));
  }
  return _invoke<DiffResult>('diff_node_file_against_base', { nodeId, filePath });
};

/** Lightweight base-relative file list for an Agent Node. The command
 * returns paths, statuses, and line counts without building or highlighting
 * hunks; the centre diff overlay loads a single file only after the user
 * chooses it. */
export const nodeChangedFiles = (nodeId: number) =>
  _invoke<GitStatus[]>('node_changed_files', { nodeId });

export type { GitStatus };

export const getGitStatus = (path: string) =>
  _invoke<GitStatus[]>('get_git_status', { path });

export type { GitBranchStatus };

export const getGitBranchStatus = (path: string) =>
  _invoke<GitBranchStatus | null>('get_git_branch_status', { path });

export type { GitSummary };

export const getGitSummary = (path: string) =>
  _invoke<GitSummary>('get_git_summary', { path });

// Issue #1374 — per-file quick actions for the center diff overlay's
// header. Both are repo-relative-path operations on the diff's `rootPath`.
export const stageFile = (repoPath: string, filePath: string) =>
  _invoke<void>('stage_file', { repoPath, filePath });

export const revertFile = (repoPath: string, filePath: string) =>
  _invoke<void>('revert_file', { repoPath, filePath });

export const getDefaultBranch = (path: string) =>
  _invoke<string>('get_default_branch', { path });

/** One-shot static snapshot for the git-status panel: repo-ness, GitHub
 *  auth, and the default branch. Replaces the three parallel IPCs
 *  (`check_is_git_repo` + `check_gh_auth` + `get_default_branch`) that
 *  `useMeshGitStatus` used to fan out (issue #348). */
export const getMeshGitStatic = (path: string) =>
  _invoke<MeshGitStatic>('get_mesh_git_static', { path });

export type { GitSyncResult };

export const gitSync = (path: string) =>
  _invoke<GitSyncResult>('git_sync', { path });

// ── Mesh health & recovery (issue #231) ─────────────────────────────────────

// Generated from the Rust structs in `src-tauri/src/models/mod.rs` (issue #404).
// Doc-comments from the hand-written interfaces now live on the Rust side and
// are picked up by the generated `.ts` files.
export type { HoldingWorktree, MeshHealth };

export const getMeshHealth = (meshId: number) =>
  _invoke<MeshHealth>('get_mesh_health', { meshId });

export type { RestoreResult };

export const restoreMeshToBase = (meshId: number) =>
  _invoke<RestoreResult>('restore_mesh_to_base', { meshId });

export type { FreeResult };

export const freeBaseBranch = (meshId: number, worktreePath: string) =>
  _invoke<FreeResult>('free_base_branch', { meshId, worktreePath });

// ── Git prune (branches & worktrees) ────────────────────────────────────────
//
// Generated from the Rust structs in `src-tauri/src/models/mod.rs` (issue #404).
export type { BranchInfo, WorktreeInfo, GitRepoPruneInfo };

export const getGitPruneInfo = (meshId: number) =>
  _invoke<GitRepoPruneInfo[]>('get_git_prune_info', { meshId });

export const deleteBranches = (meshId: number, worktreePath: string, branchNames: string[]) =>
  _invoke<void>('delete_branches', { meshId, worktreePath, branchNames });

export const deleteWorktrees = (worktreePaths: string[]) =>
  _invoke<void>('delete_worktrees', { worktreePaths });

// Issue #657: returns the trimmed `git fetch --prune` stderr so the
// frontend can surface git's own output (or an empty string on a no-op).
export const pruneRemoteTracking = (worktreePath: string) =>
  _invoke<string>('prune_remote_tracking', { worktreePath });

// ── Blocked worktree cleanup (issue #2139) ──────────────────────────────────
//
// Closing a node defers its worktree removal to a durable queue. When a removal
// cannot complete, the row keeps the failed operation, the OS error, the attempt
// count and the backoff deadline — these calls are what the blocked-cleanup
// dialog reads and acts on.

/** Every blocked worktree cleanup, with its persisted evidence. */
export const listPendingWorktreeRemovals = () =>
  _invoke<PendingWorktreeRemoval[]>('list_pending_worktree_removals');

/** Retry one cleanup now, ignoring its backoff. The status says what actually
 *  happened to the folder; only `removed` means it was deleted. */
export const retryWorktreeCleanup = (worktreePath: string) =>
  _invoke<WorktreeCleanupRetryResult>('retry_worktree_cleanup', { worktreePath });

/** "Keep worktree" — cancel the cleanup intent for one path. The reply says
 *  what was done to the disk and whether the intent was cancelled (false when
 *  the staged copy could not be moved back, so the entry stays queued). */
export const dismissWorktreeCleanup = (worktreePath: string) =>
  _invoke<WorktreeCleanupDismissalResult>('dismiss_worktree_cleanup', { worktreePath });

/** Which processes are pinning a worktree directory. Read-only; nothing is
 *  terminated. */
export const diagnoseWorktreeCleanupBlockers = (worktreePath: string) =>
  _invoke<BlockingProcess[]>('diagnose_worktree_cleanup_blockers', { worktreePath });

/** Explicitly terminate one process the diagnosis named. Only ever called from
 *  a user action on a row the diagnosis returned; the backend re-diagnoses and
 *  refuses a pid that is not a current blocker of `worktreePath`. */
export const releaseWorktreeCleanupBlocker = (worktreePath: string, pid: number) =>
  _invoke<void>('release_worktree_cleanup_blocker', { worktreePath, pid });
