import { invoke } from '@tauri-apps/api/core';
import type { Session, Checkpoint } from '../stores/sessionStore';
import type { Project } from '../stores/projectStore';

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

// Session (backend still uses workspace naming)
export const createWorkspace = (projectId: number, name: string, path: string, branch: string) =>
  invoke<Session>('create_workspace', { projectId, name, path, branch });

export const listWorkspaces = () =>
  invoke<Session[]>('list_workspaces');

export const listWorkspacesByProject = (projectId: number) =>
  invoke<Session[]>('list_workspaces_by_project', { projectId });

export const getWorkspace = (workspaceId: number) =>
  invoke<Session>('get_workspace', { workspaceId });

export const archiveWorkspace = (workspaceId: number) =>
  invoke('archive_workspace', { workspaceId });

export const restoreWorkspace = (workspaceId: number) =>
  invoke('restore_workspace', { workspaceId });

export const updateWorkspaceStatus = (workspaceId: number, status: string) =>
  invoke('update_workspace_status', { workspaceId, status });

// Project
export const addProject = () =>
  invoke<Project>('add_project');

export const createProject = (name: string, path: string) =>
  invoke<Project>('create_project', { name, path });

export const listProjects = () =>
  invoke<Project[]>('list_projects');

export const deleteProject = (projectId: number) =>
  invoke('delete_project', { projectId });

// Agent
export const spawnAgent = (workspaceId: number, provider: string) =>
  invoke('spawn_agent', { workspaceId, provider });

export const killAgent = (workspaceId: number) =>
  invoke('kill_agent', { workspaceId });

export const isAgentRunning = (workspaceId: number) =>
  invoke<boolean>('is_agent_running', { workspaceId });

export const sendToAgent = (workspaceId: number, input: string) =>
  invoke('send_to_agent', { workspaceId, input });

// Checkpoint
export const createCheckpoint = (workspaceId: number, turnIndex: number, message?: string) =>
  invoke<Checkpoint>('create_checkpoint', { workspaceId, turnIndex, message });

export const listCheckpoints = (workspaceId: number) =>
  invoke<Checkpoint[]>('list_checkpoints', { workspaceId });

export const revertToCheckpoint = (checkpointId: number) =>
  invoke('revert_to_checkpoint', { checkpointId });

export const diffCheckpoints = (checkpointAId: number, checkpointBId: number) =>
  invoke<string>('diff_checkpoints', { checkpointAId, checkpointBId });

// Diff
export const diffFiles = (oldPath: string, newPath: string) =>
  invoke<DiffResult>('diff_files', { oldPath, newPath });

export const diffWorkspaceCheckpoint = (workspaceId: number, checkpointId: number) =>
  invoke<DiffResult>('diff_workspace_checkpoint', { workspaceId, checkpointId });

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
export const watchWorkspace = (workspaceId: number) =>
  invoke('watch_workspace', { workspaceId });

export const unwatchWorkspace = (workspaceId: number) =>
  invoke('unwatch_workspace', { workspaceId });

// File tree
export interface FileNode {
  name: string;
  path: string;
  is_dir: boolean;
  children: FileNode[];
}

export const listDirectory = (path: string, maxDepth?: number) =>
  invoke<FileNode>('list_directory', { path, maxDepth });

// Git
export interface GitStatus {
  path: string;
  status: 'added' | 'modified' | 'deleted' | 'renamed' | 'untracked';
}

export const getGitStatus = (path: string) =>
  invoke<GitStatus[]>('get_git_status', { path });

// MCP
export const listMcpServers = (workspaceId: number) =>
  invoke('list_mcp_servers', { workspaceId });

// Attention
export const registerAttentionSession = (workspaceId: number) =>
  invoke('register_attention_session', { workspaceId });

export const clearAttentionSession = (workspaceId: number) =>
  invoke('clear_attention_session', { workspaceId });

export const isAttentionPending = (workspaceId: number) =>
  invoke<boolean>('is_attention_pending', { workspaceId });

// PR
export const createPr = (workspaceId: number, title: string, body: string) =>
  invoke<string>('create_pr', { workspaceId, title, body });

export const mergePr = (prUrl: string) =>
  invoke<string>('merge_pr', { prUrl });

export const getCurrentBranch = (workspaceId: number) =>
  invoke<string>('get_current_branch', { workspaceId });
