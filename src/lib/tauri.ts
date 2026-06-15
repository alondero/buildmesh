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
import type { DiscoveredSession } from '../types/generated/DiscoveredSession';
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
import type { MeshConfig } from '../types/generated/MeshConfig';
import type { MeshGitStatic } from '../types/generated/MeshGitStatic';
import type { MeshHealth } from '../types/generated/MeshHealth';
import type { OpenPr } from '../types/generated/OpenPr';
import type { PrMergeability } from '../types/generated/PrMergeability';
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
    // argument count, e.g. `toHaveBeenCalledWith('list_projects')`).
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

// Agent Node
export const createSession = (meshId: number, name: string, path: string, branch: string, provider?: string, useWorktree?: boolean) =>
  _invoke<AgentNode>('create_session', { meshId, name, path, branch, provider, useWorktree });

export const listSessions = () =>
  _invoke<AgentNode[]>('list_sessions');

export const getSession = (sessionId: number) =>
  _invoke<AgentNode>('get_session', { sessionId });

export const getWorktreeCloseSafety = (sessionId: number) =>
  _invoke<WorktreeCloseSafety>('get_worktree_close_safety', { sessionId });

export const deleteSession = (sessionId: number, removeWorktree = false) =>
  _invoke('delete_session', { sessionId, removeWorktree });

export const renameSession = (sessionId: number, name: string) =>
  _invoke('rename_session', { sessionId, name });

/** Persist new grid positions for a set of nodes: `[nodeId, position]` pairs. */
export const updateSessionPositions = (updates: [number, number][]) =>
  _invoke('update_session_positions', { updates });

// Mesh
export const addProject = () =>
  _invoke<Mesh>('add_project');

export const createProject = (name: string, path: string) =>
  _invoke<Mesh>('create_project', { name, path });

export const createTestProject = (name: string) =>
  _invoke<Mesh>('create_test_project', { name });

export const listProjects = () =>
  _invoke<Mesh[]>('list_projects');

export const deleteProject = (projectId: number) =>
  _invoke('delete_project', { projectId });

export const updateProjectLayout = (projectId: number, layout: 'grid' | 'single') =>
  _invoke('update_project_layout', { projectId, layout });

/** Persist new sidebar positions for a set of meshes: `[meshId, position]` pairs. */
export const updateProjectPositions = (updates: [number, number][]) =>
  _invoke('update_project_positions', { updates });

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
// `MeshConfig` is the wire shape of `commands::mesh_config::get_mesh_properties`.
// Generated from `src-tauri/src/models/mod.rs` (issue #404).
export type { MeshConfig };

export const getMeshProperties = (meshId: number) =>
  _invoke<MeshConfig>('get_mesh_properties', { meshId });

/** Generic mesh.toml field write: routes `(section, key, value)` to the
 *  backend's `update_mesh_field`. Use it for fields with no DB-side
 *  side-effects (model, effort, worktree_mode, default_provider, build /
 *  run command). Fields with side-effects have dedicated commands —
 *  `updateMeshUseWorktree` and `updateWorktreeBaseRef` below. */
export const updateMeshField = (
  meshId: number,
  section: 'agent' | 'build' | 'run',
  key: string,
  value: string,
) => _invoke<void>('update_mesh_field', { meshId, section, key, value });

export const updateMeshUseWorktree = (meshId: number, useWorktree: boolean) =>
  _invoke<void>('update_mesh_use_worktree', { meshId, useWorktree });

export const updateWorktreeBaseRef = (meshId: number, baseRef: string) =>
  _invoke<void>('update_worktree_base_ref', { meshId, baseRef });

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
export const watchSession = (sessionId: number) =>
  _invoke('watch_session', { sessionId });

export const unwatchSession = (sessionId: number) =>
  _invoke('unwatch_session', { sessionId });

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
export const registerAttentionSession = (sessionId: number) =>
  _invoke('register_attention_session', { sessionId });

export const clearAttentionSession = (sessionId: number) =>
  _invoke('clear_attention_session', { sessionId });

export const isAttentionPending = (sessionId: number) =>
  _invoke<boolean>('is_attention_pending', { sessionId });

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
export type { GitHubPullRequest, PrMergeability };

/** List PRs for a mesh's repo, filtered by `state` (`'open'` or `'closed'`). */
export const getRepoPulls = (meshId: number, state: 'open' | 'closed') =>
  _invoke<GitHubPullRequest[]>('get_repo_pulls', { meshId, state });

/// Per-PR mergeability enrichment — the `/pulls` list endpoint omits it, so the
/// panel fetches this once per open PR. `mergeable` is `null` while GitHub is
/// still computing the merge.
export const getPrMergeability = (meshId: number, prNumber: number) =>
  _invoke<PrMergeability>('get_pr_mergeability', { meshId, prNumber });

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

// Session Discovery — generated from the Rust struct (issue #359, re-exported
// here per #404 so call sites that import from `../lib/tauri` keep working).
export type { DiscoveredSession };

export const discoverSessions = (meshId: number, meshPath: string) =>
  _invoke<DiscoveredSession[]>('discover_sessions', { meshId, meshPath });

export const importDiscoveredSession = (
  meshId: number,
  meshPath: string,
  cliSessionId: string,
  branch: string,
  worktreeName: string | null,
  provider?: string,
) =>
  _invoke<AgentNode>('import_discovered_session', {
    meshId, meshPath, cliSessionId, branch, worktreeName, provider
  });

// ── App startup ────────────────────────────────────────────────────────────
//
// Re-attaches the in-process PTY for every node whose previous run was
// interrupted (status === 'suspended'). Returns the ids of the nodes that
// were actually resumed.
export const autoResumeSessions = () =>
  _invoke<number[]>('auto_resume_sessions');

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
