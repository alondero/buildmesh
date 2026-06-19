import { invoke as _rawInvoke } from '@tauri-apps/api/core';
import { logFrontend } from './frontendLog';
import { shapeArgs } from './ipcShape';
import type { AgentNode } from '../stores/agentNodeStore';
import type { Mesh } from '../stores/meshStore';
import type { AiContextStatus } from '../types/generated/AiContextStatus';
import type { AppPreferences } from '../types/generated/AppPreferences';
import type { BranchInfo } from '../types/generated/BranchInfo';
import type { CoordinatorStatus } from '../types/generated/CoordinatorStatus';
import type { DiffHunk } from '../types/generated/DiffHunk';
import type { DiffLine } from '../types/generated/DiffLine';
import type { DiffResult } from '../types/generated/DiffResult';
import type { ArchivedAgentNode } from '../types/generated/ArchivedAgentNode';
import type { FileDiff } from '../types/generated/FileDiff';
import type { FileNode } from '../types/generated/FileNode';
import type { FreeResult } from '../types/generated/FreeResult';
import type { GitBranchStatus } from '../types/generated/GitBranchStatus';
import type { GitHubIssue } from '../types/generated/GitHubIssue';
import type { GitHubPullRequest } from '../types/generated/GitHubPullRequest';
import type { GitRepoPruneInfo } from '../types/generated/GitRepoPruneInfo';
import type { GitSummary } from '../types/generated/GitSummary';
import type { GitSyncResult } from '../types/generated/GitSyncResult';
import type { HoldingWorktree } from '../types/generated/HoldingWorktree';
import type { IssueNodeDraft } from '../types/generated/IssueNodeDraft';
import type { MeshRow } from '../types/generated/MeshRow';
import type { MeshGitStatic } from '../types/generated/MeshGitStatic';
import type { MeshHealth } from '../types/generated/MeshHealth';
import type { OpenPr } from '../types/generated/OpenPr';
import type { PrMergeability } from '../types/generated/PrMergeability';
import type { PrFileEntry } from '../types/generated/PrFileEntry';
import type { ProviderInfo } from '../types/generated/ProviderInfo';
import type { ProviderUsage } from '../types/generated/ProviderUsage';
import type { RestoreResult } from '../types/generated/RestoreResult';
import type { UsageWindow } from '../types/generated/UsageWindow';
import type { WorktreeInfo } from '../types/generated/WorktreeInfo';
import type { WorktreeCloseSafety } from './worktreeClose';

/**
 * Central IPC chokepoint (issue #386). Every wrapper below calls through
 * `_invoke`, which delegates to Tauri's `invoke` and — on rejection —
 * forwards a single `frontendLog` entry (command name + sanitized arg
 * shape) and re-throws the original error. The re-throw preserves every
 * existing call-site behaviour (stores set error state, components show
 * toasts, the bridge still sees the original `Error` for `window.error` /
 * `unhandledrejection` plumbing). The arg shape is the sanitized form
 * produced by `ipcShape.shapeArgs` so PII (API keys) and unbounded
 * payloads (terminal scrollback) are never written to `buildmesh.log`.
 *
 * Tests stub `_rawInvoke` (the aliased Tauri binding) via
 * `vi.mock('@tauri-apps/api/core', …)` and mock `frontendLog.logFrontend`
 * to assert on the formatted shape — see `tests/unit/ipc-error-logging.test.ts`.
 */
// Function declaration (not const) so it hoists — the wrappers below
// reference `_invoke` and the function name is referenced before its
// declaration in source order.
const ERROR_TEXT_CAP = 200;

async function _invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    // Only pass `args` when defined so the call shape matches the
    // pre-wrapper `invoke('cmd')` form (some tests assert on
    // argument count, e.g. `toHaveBeenCalledWith('list_meshes')`).
    return args === undefined
      ? await _rawInvoke<T>(cmd)
      : await _rawInvoke<T>(cmd, args);
  } catch (err) {
    const shape = JSON.stringify(shapeArgs(args));
    const raw = String(err);
    const truncated = raw.length > ERROR_TEXT_CAP
      ? raw.slice(0, ERROR_TEXT_CAP) + '…'
      : raw;
    logFrontend('error', `[IPC:${cmd}] args=${shape} — ${truncated}`);
    throw err;
  }
}

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

// Agent Node — renamed from `*Session` to `*AgentNode` in issue #490.
export const createAgentNode = (meshId: number, name: string, path: string, branch: string, provider?: string, useWorktree?: boolean) =>
  _invoke<AgentNode>('create_agent_node', { meshId, name, path, branch, provider, useWorktree });

export const listAgentNodes = () =>
  _invoke<AgentNode[]>('list_agent_nodes');

export const getAgentNode = (nodeId: number) =>
  _invoke<AgentNode>('get_agent_node', { nodeId });

export const getWorktreeCloseSafety = (nodeId: number) =>
  _invoke<WorktreeCloseSafety>('get_worktree_close_safety', { nodeId });

export const deleteAgentNode = (nodeId: number, removeWorktree = false) =>
  _invoke('delete_agent_node', { nodeId, removeWorktree });

export const renameAgentNode = (nodeId: number, name: string) =>
  _invoke('rename_agent_node', { nodeId, name });

/** Persist new grid positions for a set of nodes: `[nodeId, position]` pairs. */
export const updateAgentNodePositions = (updates: [number, number][]) =>
  _invoke('update_agent_node_positions', { updates });

// Mesh — renamed from `*Project` to `*Mesh` in issue #490.
export const addMesh = () =>
  _invoke<Mesh>('add_mesh');

export const createMesh = (name: string, path: string) =>
  _invoke<Mesh>('create_mesh', { name, path });

export const createTestMesh = (name: string) =>
  _invoke<Mesh>('create_test_mesh', { name });

export const listMeshes = () =>
  _invoke<Mesh[]>('list_meshes');

export const deleteMesh = (meshId: number) =>
  _invoke('delete_mesh', { meshId });

export const updateMeshLayout = (meshId: number, layout: 'grid' | 'single') =>
  _invoke('update_mesh_layout', { meshId, layout });

/** Persist new sidebar positions for a set of meshes: `[meshId, position]` pairs. */
export const updateMeshPositions = (updates: [number, number][]) =>
  _invoke('update_mesh_positions', { updates });

export const updateMeshName = (meshId: number, name: string) =>
  _invoke('update_mesh_name', { meshId, name });

// Module-level memoisation for stable, scope-bounded reads (issue #405).
// `listProviders` is cached for the process lifetime; `getDefaultProvider`
// is cached per mesh. A rejected promise is evicted so the next caller
// retries rather than inheriting a permanently-failed read. See
// `tests/unit/tauri-provider-cache.test.ts` for the contract — concurrent
// callers de-dupe onto the in-flight promise, and a rejection evicts the
// slot for that mesh / for the global list.
let providerListPromise: Promise<ProviderInfo[]> | null = null;
const defaultProviderByMesh = new Map<number, Promise<string>>();

export const getDefaultProvider = (meshId: number): Promise<string> => {
  let p = defaultProviderByMesh.get(meshId);
  if (!p) {
    p = _invoke<string>('get_default_provider', { meshId });
    p.catch(() => { defaultProviderByMesh.delete(meshId); });
    defaultProviderByMesh.set(meshId, p);
  }
  return p;
};

// Mesh properties / configuration (issue #283)
//
// `MeshRow` is the wire shape of `commands::mesh_properties::get_mesh_properties`.
// It is a 1:1 mirror of the user-tunable columns on the `meshes` SQLite row
// (NOT a `mesh.toml` file — see `src-tauri/src/models/mod.rs::MeshRow` for the
// truth). Generated from `src-tauri/src/models/mod.rs` (issue #404 / issue #474).
export type { MeshRow };

export const getMeshProperties = (meshId: number) =>
  _invoke<MeshRow>('get_mesh_properties', { meshId });

/** Generic Mesh column write: writes `value` to the `meshes.<column>` row
 *  for `meshId` via the backend's `update_mesh_column`. **There is no
 *  `mesh.toml` file** — every field lives on the `meshes` SQLite row. The
 *  column parameter is validated against an allowlist on the backend; use
 *  it for fields with no settings.json side-effects (build_command,
 *  run_command, model, effort, worktree_mode, default_provider). Fields
 *  with side-effects have dedicated commands — `updateMeshUseWorktree` and
 *  `updateWorktreeBaseRef` below. */
export const updateMeshColumn = (
  meshId: number,
  column: 'build_command' | 'run_command' | 'model' | 'effort' | 'worktree_mode' | 'default_provider',
  value: string,
) => _invoke<void>('update_mesh_column', { meshId, column, value });

export const updateMeshUseWorktree = (meshId: number, useWorktree: boolean) =>
  _invoke<void>('update_mesh_use_worktree', { meshId, useWorktree });

/** Toggle whether this mesh's agent nodes run inside an OS process sandbox
 *  (Windows AppContainer #498 / macOS Seatbelt #497). Dedicated command (typed
 *  bool + zero-rows-is-an-error contract), like `updateMeshUseWorktree`. */
export const updateMeshSandbox = (meshId: number, sandbox: boolean) =>
  _invoke<void>('update_mesh_sandbox', { meshId, sandbox });

export const updateWorktreeBaseRef = (meshId: number, baseRef: string) =>
  _invoke<void>('update_worktree_base_ref', { meshId, baseRef });

// Scratch Pad (Probe Panel "📝 Scratch Pad" tab).
//
// Plain-text free-form notes per mesh. The empty string is a normal
// "no notes yet" state, not an error — `getMeshScratchpad` resolves
// to `""` for a fresh mesh so the editor mounts blank, and `setMeshScratchpad`
// accepts `""` as a clear-notes write. Debounced on the call site (~500ms)
// to keep the IPC chatter bounded while the user is mid-thought.
//
// The per-mesh promise cache mirrors the `getDefaultProvider` pattern
// (issue #405): concurrent callers de-dupe onto the in-flight promise,
// a rejection evicts the slot so the next caller retries, and `set`
// updates the cache so the editor sees its own writes without a
// round-trip. Notes are not cross-mesh shared, so the cache key is
// `meshId` (no need for a global slot).
const scratchpadByMesh = new Map<number, Promise<string>>();

export const getMeshScratchpad = (meshId: number): Promise<string> => {
  let p = scratchpadByMesh.get(meshId);
  if (!p) {
    p = _invoke<string>('get_mesh_scratchpad', { meshId });
    p.catch(() => { scratchpadByMesh.delete(meshId); });
    scratchpadByMesh.set(meshId, p);
  }
  return p;
};

export const setMeshScratchpad = (meshId: number, content: string): Promise<void> => {
  // Optimistic write: seed the cache with the new value so the next
  // `get` resolves to what the editor just typed, even if the
  // underlying IPC is in flight. The rejected-promise evicts the
  // slot so a failed write doesn't poison future reads.
  const p = _invoke<void>('set_mesh_scratchpad', { meshId, content }).then(() => undefined);
  scratchpadByMesh.set(meshId, Promise.resolve(content));
  p.catch(() => { scratchpadByMesh.delete(meshId); });
  return p;
};

import type { DetectedProject } from './projectPresets';

export const detectMeshProject = (meshPath: string) =>
  _invoke<DetectedProject>('detect_mesh_project', { meshPath });

// Agent
export const spawnAgent = (
  sessionId: number,
  provider: string,
  resume?: string | null,
  rows?: number,
  cols?: number,
) => _invoke('spawn_agent', { sessionId, provider, resume, rows, cols });

export const killAgent = (sessionId: number) =>
  _invoke('kill_agent', { sessionId });

export const isAgentRunning = (sessionId: number) =>
  _invoke<boolean>('is_agent_running', { sessionId });

export const sendToAgent = (sessionId: number, input: string) =>
  _invoke('send_to_agent', { sessionId, input });

/** Raw write to the agent's PTY (no submit/newline handling — cf. `sendToAgent`). */
export const writeToAgent = (sessionId: number, data: string) =>
  _invoke('write_to_agent', { sessionId, data });

// Diff
export const diffFiles = (oldPath: string, newPath: string) =>
  _invoke<DiffResult>('diff_files', { oldPath, newPath });

export const diffFileAgainstHead = (sessionPath: string, filePath: string) =>
  _invoke<DiffResult>('diff_file_against_head', { sessionPath, filePath });

// Every file an agent changed since branching (merge-base with mesh base_ref;
// see ADR 0005). One call returns the whole change set for the review panel.
export const diffNodeAgainstBase = (nodeId: number) =>
  _invoke<DiffResult>('diff_node_against_base', { nodeId });

export const diffNodeFileAgainstBase = (nodeId: number, filePath: string) =>
  _invoke<DiffResult>('diff_node_file_against_base', { nodeId, filePath });

// File watcher
export const watchAgentNode = (nodeId: number) =>
  _invoke('watch_agent_node', { nodeId });

export const unwatchAgentNode = (nodeId: number) =>
  _invoke('unwatch_agent_node', { nodeId });

// File tree
export type { FileNode };

export const listDirectory = (path: string, maxDepth?: number) =>
  _invoke<FileNode>('list_directory', { path, maxDepth });

export const openInEditor = (path: string) =>
  _invoke('open_in_editor', { path });

export const openInFileManager = (path: string) =>
  _invoke('open_in_file_manager', { path });

export const getUserConfigDir = () =>
  _invoke<string>('get_user_config_dir');

// Git — `GitStatus` is generated from the Rust struct (issue #359).
// The Rust `status` field is a plain `String`, so the generated type widens
// the old `'added' | 'modified' | ...` union to `string`; consumers that
// switch on it still compare fine.
import type { GitStatus } from '../types/generated/GitStatus';
export type { GitStatus };

export const getGitStatus = (path: string) =>
  _invoke<GitStatus[]>('get_git_status', { path });

export type { GitBranchStatus };

export const getGitBranchStatus = (path: string) =>
  _invoke<GitBranchStatus | null>('get_git_branch_status', { path });

export type { GitSummary };

export const getGitSummary = (path: string) =>
  _invoke<GitSummary>('get_git_summary', { path });

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

export const deleteBranches = (worktreePath: string, branchNames: string[]) =>
  _invoke<void>('delete_branches', { worktreePath, branchNames });

export const deleteWorktrees = (worktreePaths: string[]) =>
  _invoke<void>('delete_worktrees', { worktreePaths });

export const pruneRemoteTracking = (worktreePath: string) =>
  _invoke<void>('prune_remote_tracking', { worktreePath });

// Attention
export const registerAttentionNode = (nodeId: number) =>
  _invoke('register_attention_node', { nodeId });

export const clearAttentionNode = (nodeId: number) =>
  _invoke('clear_attention_node', { nodeId });

export const isAttentionPending = (nodeId: number) =>
  _invoke<boolean>('is_attention_pending', { nodeId });

// PR
export const createPr = (sessionId: number, title: string, body: string) =>
  _invoke<string>('create_pr', { sessionId, title, body });

export const mergePr = (prUrl: string) =>
  _invoke<string>('merge_pr', { prUrl });

export const getCurrentBranch = (sessionId: number) =>
  _invoke<string>('get_current_branch', { sessionId });

export const checkGhAuth = () =>
  _invoke<boolean>('check_gh_auth');

/** Open PR summary for an agent node — surfaces as the "PR #N" chip.
 *  Generated from the Rust struct in `src-tauri/src/commands/pr.rs` (issue #404). */
export type { OpenPr };

export const getOpenPrForNode = (nodeId: number) =>
  _invoke<OpenPr | null>('get_open_pr_for_node', { nodeId });

// GitHub Issues — `GitHubIssue` is generated from the Rust struct
// (src-tauri/src/commands/pr.rs) into src/types/generated/; see top import.
// Re-exported here so existing `import { GitHubIssue } from '../lib/tauri'`
// call sites keep working. Issue #359.
export type { GitHubIssue };

export const getRepoIssues = (meshId: number) =>
  _invoke<GitHubIssue[]>('get_repo_issues', { meshId });

// GitHub Pull Requests — `GitHubPullRequest` / `PrMergeability` are generated
// from the Rust structs (src-tauri/src/commands/pr.rs) into
// src/types/generated/; see top import. Re-exported here so the PR probe tab
// can `import { GitHubPullRequest } from '../lib/tauri'` alongside the issue
// types. Issue #359.
export type { GitHubPullRequest, PrMergeability, PrFileEntry };

/** List PRs for a mesh's repo, filtered by `state` (`'open'` or `'closed'`). */
export const getRepoPulls = (meshId: number, state: 'open' | 'closed') =>
  _invoke<GitHubPullRequest[]>('get_repo_pulls', { meshId, state });

/// Per-PR mergeability enrichment — the `/pulls` list endpoint omits it, so the
/// panel fetches this once per open PR. `mergeable` is `null` while GitHub is
/// still computing the merge.
export const getPrMergeability = (meshId: number, prNumber: number) =>
  _invoke<PrMergeability>('get_pr_mergeability', { meshId, prNumber });

/// List the files changed in a single PR (issue #421). Backed by GitHub's
/// `/pulls/{n}/files` endpoint; one call returns the whole PR with each
/// file's unified-diff `patch`. The Center Diff Overlay parses the patch
/// line-by-line to render +/−/context rows. Distinct from `getRepoPulls` /
/// `getPrMergeability` because the panel needs the diff payload, not just
/// the metadata.
export const getPrFiles = (meshId: number, prNumber: number) =>
  _invoke<PrFileEntry[]>('get_pr_files', { meshId, prNumber });

export const spawnIssueAgent = (meshId: number, issueNumber: number, issueTitle: string, provider?: string) =>
  _invoke<AgentNode>('spawn_issue_agent', { meshId, issueNumber, issueTitle, provider });

/// Two-stage spawn (issue flow) — stage 1 of 2.
///
/// `create_issue_node` is the fast DB-only half of the two-stage spawn
/// flow: it creates a `pending` agent node row and returns it with the
/// prefill string the caller must pass to `startNodeBackground`. Returns
/// in ~20ms (vs. 5-10s for the old synchronous `spawn_issue_agent`),
/// so the modal can close and the new node can appear almost
/// immediately. The original `spawnIssueAgent` is kept for the mobile
/// HTTP route, which has no interactive UI to keep responsive.
///
/// `IssueNodeDraft` is generated from the Rust struct in
/// `src-tauri/src/commands/agent.rs` (issue #404). The Rust struct uses
/// `#[serde(flatten)]` so the wire form is the flat merge of `AgentNode`
/// + `prefill`, matching the `extends AgentNode` shape the hand-written
/// TS used to carry.
export type { IssueNodeDraft };

export const createIssueNode = (meshId: number, issueNumber: number, issueTitle: string, provider?: string) =>
  _invoke<IssueNodeDraft>('create_issue_node', { meshId, issueNumber, issueTitle, provider });

/// Two-stage spawn (issue flow) — stage 2 of 2.
///
/// `start_node_background` runs the slow work (git fetch, worktree
/// create, PTY spawn, workspace-trust + attention-hook write) on a
/// background task. Fire-and-forget — the IPC returns immediately.
/// On completion the backend emits `node-spawn-completed`; on failure,
/// `node-spawn-failed` (with the node's status already flipped to
/// `error` in the DB).
export const startNodeBackground = (nodeId: number, prefill?: string) =>
  _invoke<void>('start_node_background', { nodeId, prefill });

export const spawnHandoverAgent = (meshId: number, prefill: string, provider?: string) =>
  _invoke<AgentNode>('spawn_handover_agent', { meshId, prefill, provider });

export const createPrForMesh = (meshPath: string, title: string, body: string, baseBranch: string) =>
  _invoke<string>('create_pr_for_mesh', { meshPath, title, body, baseBranch });

/// Two-stage spawn (PR flow) — stage 1 of 2 (issue #420, extended by #443
/// for fork PRs).
///
/// `create_pr_node` is the PR-flow mirror of `createIssueNode`: fast DB-only
/// IPC that returns a `pending` agent node row + the prefill string the
/// caller must pass to `startNodeBackground`. Returns in ~20ms; the slow
/// work (git fetch <remote> <head_ref>, worktree create off the head ref,
/// PTY spawn) runs on stage-2's background task.
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
/// non-fatal `pr_sha_drift` `mesh-sync-warning` if not (force-push /
/// rebase between click-time and spawn-time). An empty `headSha` skips
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
  });

// AI context portability
export type { AiContextStatus };

export const detectAiContext = (meshPath: string) =>
  _invoke<AiContextStatus>('detect_ai_context', { meshPath });

export const createAiContextPortabilityPr = (meshId: number) =>
  _invoke<string>('create_ai_context_portability_pr', { meshId });

export const listProviders = (): Promise<ProviderInfo[]> => {
  if (!providerListPromise) {
    providerListPromise = _invoke<ProviderInfo[]>('list_providers');
    providerListPromise.catch(() => { providerListPromise = null; });
  }
  return providerListPromise;
};

/** Provider UI metadata for the agent picker — generated from the Rust struct
 *  in `src-tauri/src/agent/provider/mod.rs` (issue #404). */
export type { ProviderInfo };

// Agent Node Discovery — generated from the Rust struct (issue #359 + #490,
// re-exported here per #404 so call sites that import from `../lib/tauri`
// keep working).
export type { ArchivedAgentNode };

export const discoverAgentNodes = (meshId: number, meshPath: string) =>
  _invoke<ArchivedAgentNode[]>('discover_agent_nodes', { meshId, meshPath });

export const importDiscoveredAgentNode = (
  meshId: number,
  meshPath: string,
  cliSessionId: string,
  branch: string,
  worktreeName: string | null,
  provider?: string,
) =>
  _invoke<AgentNode>('import_discovered_agent_node', {
    meshId, meshPath, cliSessionId, branch, worktreeName, provider
  });

// ── App startup ────────────────────────────────────────────────────────────
//
// Re-attaches the in-process PTY for every node whose previous run was
// interrupted (status === 'suspended'). Returns the ids of the nodes that
// were actually resumed.
export const autoResumeAgentNodes = () =>
  _invoke<number[]>('auto_resume_agent_nodes');

// ── Paths & clipboard ──────────────────────────────────────────────────────
//
// `to_host_path` is a no-op for native paths and normalises WSL/Git-Bash
// variants — used by the OS file-drop paste path.
export const toHostPath = (path: string) =>
  _invoke<string>('to_host_path', { path });

/** Native clipboard read. On macOS this bypasses the WKWebView
 *  clipboard-permission popup by shelling to `pbpaste`; on other platforms it
 *  may reject, and callers fall back to `navigator.clipboard.readText()`. */
export const readClipboard = () =>
  _invoke<string>('read_clipboard');

// ── Agent PTY transport ────────────────────────────────────────────────────
//
// `writeToAgent` is declared above next to the other agent IPCs; `resizeAgent`
// here completes the PTY-side surface used by `TerminalRegistry`. It rejects
// with the string `'Agent not running'` while the PTY isn't up yet — callers
// match on that exact value to ignore the expected race (see
// `TerminalRegistry.syncPtySize`).
export const resizeAgent = (sessionId: number, rows: number, cols: number) =>
  _invoke('resize_agent', { sessionId, rows, cols });

/** Reply to a remote-pane snapshot request from the HTTP server. The pair
 *  (`request_id`, `data`) is matched against an in-flight promise on the
 *  backend; the call returns immediately and has no result. */
export const submitTerminalSnapshot = (requestId: string, data: string) =>
  _invoke<void>('submit_terminal_snapshot', { requestId, data });

// ── Build/Run side-panel PTY ───────────────────────────────────────────────
//
// Separate PTY surface from the agent terminal — keeps a long-running build
// or `npm run dev` independent of the agent's PTY lifecycle. `build_run`
// returns once the child has been spawned; output flows via the
// `build-run-output-<nodeId>` event.
export const buildRun = (nodeId: number, mode: 'build' | 'run' | 'terminal') =>
  _invoke('build_run', { nodeId, mode });

export const closeBuildRun = (nodeId: number) =>
  _invoke('close_build_run', { nodeId });

export const writeToBuildRun = (nodeId: number, data: string) =>
  _invoke('write_to_build_run', { nodeId, data });

export const resizeBuildRun = (nodeId: number, rows: number, cols: number) =>
  _invoke('resize_build_run', { nodeId, rows, cols });

// ── App-wide preferences (`preferences.json`) ──────────────────────────────
//
// Generated from `crate::preferences::AppPreferences` (issue #404). The
// `google_cloud_project` field is included to match the Rust struct in full
// even though the current settings UI only reads two fields.
export type { AppPreferences };

export const getAppPreferences = () =>
  _invoke<AppPreferences>('get_app_preferences');

/** Pass `null` (or an empty string, which the backend filters out) to clear
 *  the override and fall back to the hardcoded `anthropic` default. */
export const setAppDefaultProvider = (provider: string | null) =>
  _invoke('set_app_default_provider', { provider });

export const setMinimaxApiKey = (key: string | null) =>
  _invoke('set_minimax_api_key', { key });

// ── Provider usage (Accounts & Usage panel) ────────────────────────────────
//
// Generated from `crate::services::usage::{ProviderUsage, UsageWindow}`
// (issue #404). Rust uses `#[serde(rename = "...")]` + matching
// `#[ts(rename = "...")]` on some fields, so the camelCase / snake_case
// mix is exact — `usedPercent` / `resetsAt` / `loggedIn` are camelCase on
// the wire, the rest are snake_case.
export type { UsageWindow, ProviderUsage };

export const getAllProviderUsage = (forceRefresh: boolean) =>
  _invoke<ProviderUsage[]>('get_all_provider_usage', { forceRefresh });

// ── Coordinator read API control (ADR-0008) ────────────────────────────────
//
// Generated from `commands::coordinator::CoordinatorStatus` (issue #404).
// `has_token` reports presence without ever leaking the token value — the
// token is only ever surfaced once, by `generateCoordinatorReadToken`.
export type { CoordinatorStatus };

export const getCoordinatorStatus = () =>
  _invoke<CoordinatorStatus>('get_coordinator_status');

export const setCoordinatorApiEnabled = (enabled: boolean) =>
  _invoke('set_coordinator_api_enabled', { enabled });

/** Mint (or replace) the read-scoped token and return it for the user to copy.
 *  Replacing invalidates the previously issued token; the value is shown once. */
export const generateCoordinatorReadToken = () =>
  _invoke<string>('generate_coordinator_read_token');

// ── Remote access (mobile QR) ──────────────────────────────────────────────
export const getLocalIp = () =>
  _invoke<string>('get_local_ip');

export const getRootToken = () =>
  _invoke<string>('get_root_token');

/** Test-only: clear the module-level provider caches between cases. Exported
 *  with a leading-underscore name so accidental production use is loud. */
export function __resetProviderCachesForTests(): void {
  providerListPromise = null;
  defaultProviderByMesh.clear();
}
