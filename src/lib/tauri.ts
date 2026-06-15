import { invoke as _rawInvoke } from '@tauri-apps/api/core';
import { logFrontend } from './frontendLog';
import { shapeArgs } from './ipcShape';
import type { AgentNode } from '../stores/agentNodeStore';
import type { Mesh } from '../stores/meshStore';
import type { GitHubIssue } from '../types/generated/GitHubIssue';
import type { GitHubPullRequest } from '../types/generated/GitHubPullRequest';
import type { PrMergeability } from '../types/generated/PrMergeability';
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

export interface DiffLine {
  line_type: DiffLineType;
  content: string;
  old_num: number | null;
  new_num: number | null;
}

export interface DiffHunk {
  old_start: number;
  old_lines: number;
  new_start: number;
  new_lines: number;
  old_highlighted: string;
  new_highlighted: string;
  lines: DiffLine[];
  /** Per-line highlighted inline HTML, aligned 1:1 with `lines`. May be absent
   *  for producers that only feed the side-by-side view. */
  lines_highlighted?: string[];
}

/** Change kind, shared with `GitStatus.status`. */
export type FileDiffStatus =
  | 'added'
  | 'modified'
  | 'deleted'
  | 'renamed'
  | 'untracked';

export interface FileDiff {
  path: string;
  hunks: DiffHunk[];
  /** Empty string from older diff producers that don't set a status. */
  status: FileDiffStatus | '';
  /** Source path for renames; null otherwise. */
  old_path: string | null;
  additions: number;
  deletions: number;
  binary: boolean;
}

export interface DiffResult {
  files: FileDiff[];
}

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

export const getDefaultProvider = (meshId: number) =>
  _invoke<string>('get_default_provider', { meshId });

// Mesh properties / configuration (issue #283)
//
// `MeshConfig` is the wire shape of `commands::mesh_config::get_mesh_properties`.
// The Rust struct (`src-tauri/src/models/mod.rs`) only derives `serde::Serialize`,
// not `TS`, so no generated type exists — keep the hand-written interface in
// sync if the Rust struct changes (follow-up: derive `TS` once #359's
// hand-kept-types backlog is being worked).
export interface MeshConfig {
  name: string | null;
  build_command: string | null;
  run_command: string | null;
  model: string | null;
  effort: string | null;
  base_ref: string | null;
  use_worktree: boolean;
  worktree_mode?: string | null;
  default_provider: string | null;
}

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
export interface FileNode {
  name: string;
  path: string;
  is_dir: boolean;
  children: FileNode[];
}

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

export interface GitBranchStatus {
  name: string;
  ahead: number;
  behind: number;
  /**
   * Abbreviated HEAD OID (7 chars by default — matches `git rev-parse --short HEAD`).
   * Empty string when HEAD is unborn. Lets the UI render a stable identifier on
   * detached-HEAD worktrees (e.g. after `free_base_branch` recovery detaches
   * a branched worktree) where `name === 'HEAD'` would otherwise be uninformative.
   */
  short_sha: string;
}

export const getGitBranchStatus = (path: string) =>
  _invoke<GitBranchStatus | null>('get_git_branch_status', { path });

export interface GitSummary {
  total: number;
  added: number;
  modified: number;
  deleted: number;
}

export const getGitSummary = (path: string) =>
  _invoke<GitSummary>('get_git_summary', { path });

export const checkIsGitRepo = (path: string) =>
  _invoke<boolean>('check_is_git_repo', { path });

export const getDefaultBranch = (path: string) =>
  _invoke<string>('get_default_branch', { path });

export interface GitSyncResult {
  fetched: boolean;
  pulled: boolean;
  new_commits: number;
  message: string;
}

export const gitSync = (path: string) =>
  _invoke<GitSyncResult>('git_sync', { path });

// ── Mesh health & recovery (issue #231) ─────────────────────────────────────

/**
 * A worktree that currently has the Base Ref's branch checked out,
 * blocking `git checkout <base>` from the Mesh root. The `name` is
 * the worktree's basename for display. `is_active` is true when a
 * non-archived agent node currently points at `path`.
 */
export interface HoldingWorktree {
  path: string;
  name: string;
  is_active: boolean;
}

/**
 * Single-snapshot read of a Mesh's Git health. `local_base_branch` is
 * derived from `base_ref` (e.g. `origin/main` → `main`). `is_drifted`
 * is true when the current branch differs from the base (or HEAD is
 * detached on a non-base OID). `unpushed_ahead` counts local commits
 * that would be stranded by a restore-to-base.
 */
export interface MeshHealth {
  base_ref: string;
  local_base_branch: string | null;
  current_branch: string | null;
  current_short_sha: string;
  is_detached: boolean;
  is_dirty: boolean;
  unpushed_ahead: number;
  has_upstream: boolean;
  is_drifted: boolean;
  base_branch_holder: HoldingWorktree | null;
}

export const getMeshHealth = (meshId: number) =>
  _invoke<MeshHealth>('get_mesh_health', { meshId });

export interface RestoreResult {
  restored: boolean;
  message: string;
}

export const restoreMeshToBase = (meshId: number) =>
  _invoke<RestoreResult>('restore_mesh_to_base', { meshId });

export interface FreeResult {
  detached_at_sha: string;
}

export const freeBaseBranch = (meshId: number, worktreePath: string) =>
  _invoke<FreeResult>('free_base_branch', { meshId, worktreePath });

// ── Git prune (branches & worktrees) ────────────────────────────────────────
//
// Mirrors the Rust structs in `src-tauri/src/models/mod.rs`. None of the
// prune types derive `TS` (yet), so the wire shapes are hand-written here —
// keep in sync if a Rust field is added/renamed.

export interface BranchInfo {
  name: string;
  is_head: boolean;
  /** null when the repo has no main/master branch to compare against. */
  is_merged_into_main: boolean | null;
  is_orphan: boolean;
  has_uncommitted: boolean;
  last_commit_date: string | null;
  ahead: number;
  behind: number;
}

export interface WorktreeInfo {
  path: string;
  branch: string | null;
  is_active: boolean;
  is_stale: boolean;
}

export interface GitRepoPruneInfo {
  path: string;
  local_branches: BranchInfo[];
  worktrees: WorktreeInfo[];
  remote_tracking_branches: string[];
}

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

/** Open PR summary for an agent node — surfaces as the "PR #N" chip. */
export interface OpenPr {
  number: number;
  url: string;
  title: string;
  draft: boolean;
}

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
export interface IssueNodeDraft extends AgentNode {
  prefill: string;
}

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
export interface AiContextStatus {
  claude_md_exists: boolean;
  agents_md_exists: boolean;
  skills_dir_exists: boolean;
  skill_count: number;
  agents_skills_exists: boolean;
}

export const detectAiContext = (meshPath: string) =>
  _invoke<AiContextStatus>('detect_ai_context', { meshPath });

export const createAiContextPortabilityPr = (meshId: number) =>
  _invoke<string>('create_ai_context_portability_pr', { meshId });

export const listProviders = () =>
  _invoke<ProviderInfo[]>('list_providers');

export interface ProviderInfo {
  id: string;
  label: string;
  color: string;
  icon: string;
}

// Session Discovery
export interface DiscoveredSession {
  session_id: string;
  first_message: string;
  branch: string | null;
  cwd: string | null;
  timestamp: string | null;
  worktree_name: string | null;
}

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
// Hand-mirrored shape of `crate::preferences::AppPreferences` —
// `google_cloud_project` is included to match the Rust struct in full even
// though the current settings UI only reads two fields. Not generated; bump
// this if the Rust struct grows. (Follow-up: derive `TS` per #359.)
export interface AppPreferences {
  default_provider: string | null;
  minimax_api_key: string | null;
  google_cloud_project?: string | null;
}

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
// Mirrors `crate::services::usage::{ProviderUsage, UsageWindow}`. Rust uses
// `#[serde(rename = "...")]` on some fields, so the camelCase / snake_case
// mix is exact — `usedPercent`/`resetsAt`/`loggedIn` are camelCase on the
// wire, the rest are snake_case. Keep matching the Rust shape if it grows.
export interface UsageWindow {
  label: string;
  usedPercent: number | null;
  resetsAt: string | null;
}

export interface ProviderUsage {
  provider: string;
  loggedIn: boolean;
  windows: UsageWindow[];
  detail: string | null;
  error: string | null;
}

export const getAllProviderUsage = (forceRefresh: boolean) =>
  _invoke<ProviderUsage[]>('get_all_provider_usage', { forceRefresh });

// ── Coordinator read API control (ADR-0008) ────────────────────────────────
//
// `has_token` reports presence without ever leaking the token value — the
// token is only ever surfaced once, by `generateCoordinatorReadToken`.
export interface CoordinatorStatus {
  enabled: boolean;
  has_token: boolean;
}

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
