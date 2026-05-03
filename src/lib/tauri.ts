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

// Session
export const createSession = (projectId: number, name: string, path: string, branch: string) =>
  invoke<Session>('create_session', { projectId, name, path, branch });

export const listSessions = () =>
  invoke<Session[]>('list_sessions');

export const listSessionsByProject = (projectId: number) =>
  invoke<Session[]>('list_sessions_by_project', { projectId });

export const getSession = (sessionId: number) =>
  invoke<Session>('get_session', { sessionId });

export const archiveSession = (sessionId: number) =>
  invoke('archive_session', { sessionId });

export const restoreSession = (sessionId: number) =>
  invoke('restore_session', { sessionId });

export const updateSessionStatus = (sessionId: number, status: string) =>
  invoke('update_session_status', { sessionId, status });

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

// Git
export interface GitStatus {
  path: string;
  status: 'added' | 'modified' | 'deleted' | 'renamed' | 'untracked';
}

export const getGitStatus = (path: string) =>
  invoke<GitStatus[]>('get_git_status', { path });

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
