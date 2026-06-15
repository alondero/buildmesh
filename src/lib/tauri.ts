import { invoke } from '@tauri-apps/api/core';
import type { AgentNode } from '../stores/agentNodeStore';
import type { Mesh } from '../stores/meshStore';
import type { GitHubIssue } from '../types/generated/GitHubIssue';
import type { WorktreeCloseSafety } from './worktreeClose';

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
  invoke<AgentNode>('create_session', { meshId, name, path, branch, provider, useWorktree });

export const listSessions = () =>
  invoke<AgentNode[]>('list_sessions');

export const getSession = (sessionId: number) =>
  invoke<AgentNode>('get_session', { sessionId });

export const getWorktreeCloseSafety = (sessionId: number) =>
  invoke<WorktreeCloseSafety>('get_worktree_close_safety', { sessionId });

export const deleteSession = (sessionId: number, removeWorktree = false) =>
  invoke('delete_session', { sessionId, removeWorktree });

export const renameSession = (sessionId: number, name: string) =>
  invoke('rename_session', { sessionId, name });

/** Persist new grid positions for a set of nodes: `[nodeId, position]` pairs. */
export const updateSessionPositions = (updates: [number, number][]) =>
  invoke('update_session_positions', { updates });

// Mesh
export const addProject = () =>
  invoke<Mesh>('add_project');

export const createProject = (name: string, path: string) =>
  invoke<Mesh>('create_project', { name, path });

export const createTestProject = (name: string) =>
  invoke<Mesh>('create_test_project', { name });

export const listProjects = () =>
  invoke<Mesh[]>('list_projects');

export const deleteProject = (projectId: number) =>
  invoke('delete_project', { projectId });

export const updateProjectLayout = (projectId: number, layout: 'grid' | 'single') =>
  invoke('update_project_layout', { projectId, layout });

/** Persist new sidebar positions for a set of meshes: `[meshId, position]` pairs. */
export const updateProjectPositions = (updates: [number, number][]) =>
  invoke('update_project_positions', { updates });

export const updateMeshName = (meshId: number, name: string) =>
  invoke('update_mesh_name', { meshId, name });

export const getDefaultProvider = (meshId: number) =>
  invoke<string>('get_default_provider', { meshId });

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
  invoke<MeshConfig>('get_mesh_properties', { meshId });

/** Generic mesh.toml field write: routes `(section, key, value)` to the
 *  backend's `update_mesh_field`. Use it for fields with no DB-side
 *  side-effects (model, effort, worktree_mode, default_provider, build /
 *  run command). Fields with side-effects have dedicated commands —
 *  `updateMeshUseWorktree` and `updateWorktreeBaseRef` below. */
export const updateMeshField = (meshId: number, section: string, key: string, value: string) =>
  invoke<void>('update_mesh_field', { meshId, section, key, value });

export const updateMeshUseWorktree = (meshId: number, useWorktree: boolean) =>
  invoke<void>('update_mesh_use_worktree', { meshId, useWorktree });

export const updateWorktreeBaseRef = (meshId: number, baseRef: string) =>
  invoke<void>('update_worktree_base_ref', { meshId, baseRef });

import type { DetectedProject } from './projectPresets';

export const detectMeshProject = (meshPath: string) =>
  invoke<DetectedProject>('detect_mesh_project', { meshPath });

// Agent
export const spawnAgent = (
  sessionId: number,
  provider: string,
  resume?: string | null,
  rows?: number,
  cols?: number,
) => invoke('spawn_agent', { sessionId, provider, resume, rows, cols });

export const killAgent = (sessionId: number) =>
  invoke('kill_agent', { sessionId });

export const isAgentRunning = (sessionId: number) =>
  invoke<boolean>('is_agent_running', { sessionId });

export const sendToAgent = (sessionId: number, input: string) =>
  invoke('send_to_agent', { sessionId, input });

/** Raw write to the agent's PTY (no submit/newline handling — cf. `sendToAgent`). */
export const writeToAgent = (sessionId: number, data: string) =>
  invoke('write_to_agent', { sessionId, data });

// Diff
export const diffFiles = (oldPath: string, newPath: string) =>
  invoke<DiffResult>('diff_files', { oldPath, newPath });

export const diffFileAgainstHead = (sessionPath: string, filePath: string) =>
  invoke<DiffResult>('diff_file_against_head', { sessionPath, filePath });

// Every file an agent changed since branching (merge-base with mesh base_ref;
// see ADR 0005). One call returns the whole change set for the review panel.
export const diffNodeAgainstBase = (nodeId: number) =>
  invoke<DiffResult>('diff_node_against_base', { nodeId });

export const diffNodeFileAgainstBase = (nodeId: number, filePath: string) =>
  invoke<DiffResult>('diff_node_file_against_base', { nodeId, filePath });

// File watcher
export const watchSession = (sessionId: number) =>
  invoke('watch_session', { sessionId });

export const unwatchSession = (sessionId: number) =>
  invoke('unwatch_session', { sessionId });

// File tree
export interface FileNode {
  name: string;
  path: string;
  is_dir: boolean;
  children: FileNode[];
}

export const listDirectory = (path: string, maxDepth?: number) =>
  invoke<FileNode>('list_directory', { path, maxDepth });

export const openInEditor = (path: string) =>
  invoke('open_in_editor', { path });

export const openInFileManager = (path: string) =>
  invoke('open_in_file_manager', { path });

export const getUserConfigDir = () =>
  invoke<string>('get_user_config_dir');

// Git — `GitStatus` is generated from the Rust struct (issue #359).
// The Rust `status` field is a plain `String`, so the generated type widens
// the old `'added' | 'modified' | ...` union to `string`; consumers that
// switch on it still compare fine.
import type { GitStatus } from '../types/generated/GitStatus';
export type { GitStatus };

export const getGitStatus = (path: string) =>
  invoke<GitStatus[]>('get_git_status', { path });

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
  invoke<GitBranchStatus | null>('get_git_branch_status', { path });

export interface GitSummary {
  total: number;
  added: number;
  modified: number;
  deleted: number;
}

export const getGitSummary = (path: string) =>
  invoke<GitSummary>('get_git_summary', { path });

export const checkIsGitRepo = (path: string) =>
  invoke<boolean>('check_is_git_repo', { path });

export const getDefaultBranch = (path: string) =>
  invoke<string>('get_default_branch', { path });

export interface GitSyncResult {
  fetched: boolean;
  pulled: boolean;
  new_commits: number;
  message: string;
}

export const gitSync = (path: string) =>
  invoke<GitSyncResult>('git_sync', { path });

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
  invoke<MeshHealth>('get_mesh_health', { meshId });

export interface RestoreResult {
  restored: boolean;
  message: string;
}

export const restoreMeshToBase = (meshId: number) =>
  invoke<RestoreResult>('restore_mesh_to_base', { meshId });

export interface FreeResult {
  detached_at_sha: string;
}

export const freeBaseBranch = (meshId: number, worktreePath: string) =>
  invoke<FreeResult>('free_base_branch', { meshId, worktreePath });

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
  invoke<GitRepoPruneInfo[]>('get_git_prune_info', { meshId });

export const deleteBranches = (worktreePath: string, branchNames: string[]) =>
  invoke<void>('delete_branches', { worktreePath, branchNames });

export const deleteWorktrees = (worktreePaths: string[]) =>
  invoke<void>('delete_worktrees', { worktreePaths });

export const pruneRemoteTracking = (worktreePath: string) =>
  invoke<void>('prune_remote_tracking', { worktreePath });

// Attention
export const registerAttentionSession = (sessionId: number) =>
  invoke('register_attention_session', { sessionId });

export const clearAttentionSession = (sessionId: number) =>
  invoke('clear_attention_session', { sessionId });

export const isAttentionPending = (sessionId: number) =>
  invoke<boolean>('is_attention_pending', { sessionId });

// PR
export const createPr = (sessionId: number, title: string, body: string) =>
  invoke<string>('create_pr', { sessionId, title, body });

export const mergePr = (prUrl: string) =>
  invoke<string>('merge_pr', { prUrl });

export const getCurrentBranch = (sessionId: number) =>
  invoke<string>('get_current_branch', { sessionId });

export const checkGhAuth = () =>
  invoke<boolean>('check_gh_auth');

/** Open PR summary for an agent node — surfaces as the "PR #N" chip. */
export interface OpenPr {
  number: number;
  url: string;
  title: string;
  draft: boolean;
}

export const getOpenPrForNode = (nodeId: number) =>
  invoke<OpenPr | null>('get_open_pr_for_node', { nodeId });

// GitHub Issues — `GitHubIssue` is generated from the Rust struct
// (src-tauri/src/commands/pr.rs) into src/types/generated/; see top import.
// Re-exported here so existing `import { GitHubIssue } from '../lib/tauri'`
// call sites keep working. Issue #359.
export type { GitHubIssue };

export const getRepoIssues = (meshId: number) =>
  invoke<GitHubIssue[]>('get_repo_issues', { meshId });

export const spawnIssueAgent = (meshId: number, issueNumber: number, issueTitle: string, provider?: string) =>
  invoke<AgentNode>('spawn_issue_agent', { meshId, issueNumber, issueTitle, provider });

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
  invoke<IssueNodeDraft>('create_issue_node', { meshId, issueNumber, issueTitle, provider });

/// Two-stage spawn (issue flow) — stage 2 of 2.
///
/// `start_node_background` runs the slow work (git fetch, worktree
/// create, PTY spawn, workspace-trust + attention-hook write) on a
/// background task. Fire-and-forget — the IPC returns immediately.
/// On completion the backend emits `node-spawn-completed`; on failure,
/// `node-spawn-failed` (with the node's status already flipped to
/// `error` in the DB).
export const startNodeBackground = (nodeId: number, prefill?: string) =>
  invoke<void>('start_node_background', { nodeId, prefill });

export const spawnHandoverAgent = (meshId: number, prefill: string, provider?: string) =>
  invoke<AgentNode>('spawn_handover_agent', { meshId, prefill, provider });

export const createPrForMesh = (meshPath: string, title: string, body: string, baseBranch: string) =>
  invoke<string>('create_pr_for_mesh', { meshPath, title, body, baseBranch });

// AI context portability
export interface AiContextStatus {
  claude_md_exists: boolean;
  agents_md_exists: boolean;
  skills_dir_exists: boolean;
  skill_count: number;
  agents_skills_exists: boolean;
}

export const detectAiContext = (meshPath: string) =>
  invoke<AiContextStatus>('detect_ai_context', { meshPath });

export const createAiContextPortabilityPr = (meshId: number) =>
  invoke<string>('create_ai_context_portability_pr', { meshId });

export const listProviders = () =>
  invoke<ProviderInfo[]>('list_providers');

export interface ProviderInfo {
  id: string;
  label: string;
  color: string;
  icon: string;
}

// Mesh properties (the ⚙️ Probe tab — issue #375)
export interface MeshProperties {
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
  invoke<MeshProperties>('get_mesh_properties', { meshId });

export const updateMeshField = (
  meshId: number,
  section: 'agent' | 'build' | 'run',
  key: string,
  value: string,
) => invoke<void>('update_mesh_field', { meshId, section, key, value });

export const detectMeshProject = (meshPath: string) =>
  invoke<{
    preset_id: string | null;
    label: string | null;
    node_scripts: { build: string | null; run: string | null; has_tauri_cli: boolean } | null;
  }>('detect_mesh_project', { meshPath });

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
  invoke<DiscoveredSession[]>('discover_sessions', { meshId, meshPath });

export const importDiscoveredSession = (
  meshId: number,
  meshPath: string,
  cliSessionId: string,
  branch: string,
  worktreeName: string | null,
  provider?: string,
) =>
  invoke<AgentNode>('import_discovered_session', {
    meshId, meshPath, cliSessionId, branch, worktreeName, provider
  });
