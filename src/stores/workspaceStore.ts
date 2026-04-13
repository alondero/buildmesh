import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';

export interface Workspace {
  id: number;
  project_id: number;
  name: string;
  path: string;
  branch: string;
  env: 'windows' | 'wsl';
  provider: 'anthropic' | 'minimax';
  status: 'running' | 'idle' | 'error' | 'archived';
  created_at: string;
}

export interface Checkpoint {
  id: number;
  workspace_id: number;
  git_ref: string;
  turn_index: number;
  message: string;
  created_at: string;
}

interface WorkspaceState {
  workspaces: Workspace[];
  activeWorkspaceId: number | null;
  activeWorkspace: Workspace | null;
  checkpoints: Checkpoint[];
  loading: boolean;
  error: string | null;

  fetchWorkspaces: () => Promise<void>;
  createWorkspace: (projectId: number, name: string, path: string, branch: string) => Promise<void>;
  archiveWorkspace: (id: number) => Promise<void>;
  restoreWorkspace: (id: number) => Promise<void>;
  setActiveWorkspace: (id: number | null) => Promise<void>;
  fetchCheckpoints: (workspaceId: number) => Promise<void>;
  spawnAgent: (workspaceId: number, provider: string) => Promise<void>;
  killAgent: (workspaceId: number) => Promise<void>;
  createCheckpoint: (workspaceId: number, turnIndex: number, message?: string) => Promise<void>;
  revertCheckpoint: (checkpointId: number) => Promise<void>;
}

export const useWorkspaceStore = create<WorkspaceState>((set, get) => ({
  workspaces: [],
  activeWorkspaceId: null,
  activeWorkspace: null,
  checkpoints: [],
  loading: false,
  error: null,

  fetchWorkspaces: async () => {
    set({ loading: true, error: null });
    try {
      const workspaces = await invoke<Workspace[]>('list_workspaces');
      set({ workspaces, loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  createWorkspace: async (projectId, name, path, branch) => {
    try {
      const ws = await invoke<Workspace>('create_workspace', {
        projectId, name, path, branch,
      });
      set((state) => ({ workspaces: [ws, ...state.workspaces] }));
    } catch (e) {
      set({ error: String(e) });
    }
  },

  archiveWorkspace: async (id) => {
    try {
      await invoke('archive_workspace', { workspaceId: id });
      await get().fetchWorkspaces();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  restoreWorkspace: async (id) => {
    try {
      await invoke('restore_workspace', { workspaceId: id });
      await get().fetchWorkspaces();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  setActiveWorkspace: async (id) => {
    set({ activeWorkspaceId: id });
    if (id !== null) {
      try {
        const ws = await invoke<Workspace>('get_workspace', { workspaceId: id });
        set({ activeWorkspace: ws });
        await get().fetchCheckpoints(id);
      } catch (e) {
        set({ error: String(e) });
      }
    } else {
      set({ activeWorkspace: null, checkpoints: [] });
    }
  },

  fetchCheckpoints: async (workspaceId) => {
    try {
      const checkpoints = await invoke<Checkpoint[]>('list_checkpoints', { workspaceId });
      set({ checkpoints });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  spawnAgent: async (workspaceId, provider) => {
    try {
      await invoke('spawn_agent', { workspaceId, provider });
      await get().fetchWorkspaces();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  killAgent: async (workspaceId) => {
    try {
      await invoke('kill_agent', { workspaceId });
      await get().fetchWorkspaces();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  createCheckpoint: async (workspaceId, turnIndex, message) => {
    try {
      await invoke('create_checkpoint', { workspaceId, turnIndex, message });
      await get().fetchCheckpoints(workspaceId);
    } catch (e) {
      set({ error: String(e) });
    }
  },

  revertCheckpoint: async (checkpointId) => {
    try {
      await invoke('revert_to_checkpoint', { checkpointId });
      const { activeWorkspaceId } = get();
      if (activeWorkspaceId) await get().fetchCheckpoints(activeWorkspaceId);
    } catch (e) {
      set({ error: String(e) });
    }
  },
}));
