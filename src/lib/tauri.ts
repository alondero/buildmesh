import { invoke } from '@tauri-apps/api/core';
import type { AgentNode, Checkpoint } from '../stores/agentNodeStore';
import type { Mesh } from '../stores/meshStore';

export interface DiffResult {
  files: Array<{
    path: string;
    hunks: Array<{
      old_start: number;
      old_lines: number;
      new_start: number;
      new_lines: number;
      old_highlighted: string;
      new_highlighted: string;
      lines: Array<{
        line_type: string;
        content: string;
        old_num: number | null;
        new_num: number | null;
      }>;
    }>;
  }>;
}

// Agent Node
export const createSession = (meshId: number, name: string, path: string, branch: string) =>
  invoke<AgentNode>('create_session', { meshId, name, path, branch });

export const listSessions = () =>
  invoke<AgentNode[]>('list_sessions');

export const listSessionsByMesh = (meshId: number) =>
  invoke<AgentNode[]>('list_sessions_by_mesh', { meshId });

export const getSession = (sessionId: number) =>
  invoke<AgentNode>('get_session', { sessionId });

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

export const getUserConfigDir = () =>
  invoke<string>('get_user_config_dir');

// Git
export interface GitStatus {
  path: string;
  status: 'added' | 'modified' | 'deleted' | 'renamed' | 'untracked';
}

export const getGitStatus = (path: string) =>
  invoke<GitStatus[]>('get_git_status', { path });

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

// MCP
export const listMcpServers = (sessionId: number) =>
  invoke('list_mcp_servers', { sessionId });

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

export const createPrForMesh = (meshPath: string, title: string, body: string, baseBranch: string) =>
  invoke<string>('create_pr_for_mesh', { meshPath, title, body, baseBranch });

export const listProviders = () =>
  invoke<ProviderInfo[]>('list_providers');

export interface ProviderInfo {
  id: string;
  label: string;
  color: string;
  icon: string;
}
