import { invoke } from '@tauri-apps/api/core';
import type { AgentNode, Checkpoint } from '../stores/agentNodeStore';
import type { Mesh } from '../stores/meshStore';
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

// Mesh
export const addProject = () =>
  invoke<Mesh>('add_project');

export const createProject = (name: string, path: string) =>
  invoke<Mesh>('create_project', { name, path });

export const listProjects = () =>
  invoke<Mesh[]>('list_projects');

export const deleteProject = (projectId: number) =>
  invoke('delete_project', { projectId });

// Agent
export const spawnAgent = (sessionId: number, provider: string) =>
  invoke('spawn_agent', { sessionId, provider });

export const killAgent = (sessionId: number) =>
  invoke('kill_agent', { sessionId });

export const isAgentRunning = (sessionId: number) =>
  invoke<boolean>('is_agent_running', { sessionId });

export const sendToAgent = (sessionId: number, input: string) =>
  invoke('send_to_agent', { sessionId, input });

// Checkpoint
export const createCheckpoint = (sessionId: number, turnIndex: number, message?: string) =>
  invoke<Checkpoint>('create_checkpoint', { sessionId, turnIndex, message });

export const listCheckpoints = (sessionId: number) =>
  invoke<Checkpoint[]>('list_checkpoints', { sessionId });

export const revertToCheckpoint = (checkpointId: number) =>
  invoke('revert_to_checkpoint', { checkpointId });

export const diffCheckpoints = (checkpointAId: number, checkpointBId: number) =>
  invoke<string>('diff_checkpoints', { checkpointAId, checkpointBId });

// Diff
export const diffFiles = (oldPath: string, newPath: string) =>
  invoke<DiffResult>('diff_files', { oldPath, newPath });

export const diffSessionCheckpoint = (sessionId: number, checkpointId: number) =>
  invoke<DiffResult>('diff_session_checkpoint', { sessionId, checkpointId });

export const diffFileAgainstHead = (sessionPath: string, filePath: string) =>
  invoke<DiffResult>('diff_file_against_head', { sessionPath, filePath });

// Every file an agent changed since branching (merge-base with mesh base_ref;
// see ADR 0005). One call returns the whole change set for the review panel.
export const diffNodeAgainstBase = (nodeId: number) =>
  invoke<DiffResult>('diff_node_against_base', { nodeId });

export const diffNodeFileAgainstBase = (nodeId: number, filePath: string) =>
  invoke<DiffResult>('diff_node_file_against_base', { nodeId, filePath });

// Terminal
export const spawnPty = (command: string, args: string[], cwd: string, ptyId: string) =>
  invoke('spawn_pty', { command, args, cwd, ptyId });

export const writePty = (ptyId: string, data: string) =>
  invoke('write_pty', { ptyId, data });

export const closePty = (ptyId: string) =>
  invoke('close_pty', { ptyId });

export const spawnShell = (ptyId: string, isWsl: boolean, cwd: string) =>
  invoke('spawn_shell', { ptyId, isWsl, cwd });

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

// Git
export interface GitStatus {
  path: string;
  status: 'added' | 'modified' | 'deleted' | 'renamed' | 'untracked';
  additions: number;
  deletions: number;
}

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

// GitHub Issues
export interface GitHubIssue {
  number: number;
  title: string;
  body: string;
}

export const getRepoIssues = (meshId: number) =>
  invoke<GitHubIssue[]>('get_repo_issues', { meshId });

export const spawnIssueAgent = (meshId: number, issueNumber: number, issueTitle: string, provider?: string) =>
  invoke<AgentNode>('spawn_issue_agent', { meshId, issueNumber, issueTitle, provider });

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
