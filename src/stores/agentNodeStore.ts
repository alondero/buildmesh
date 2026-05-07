import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { disposeTerminal } from '../components/Terminal/Terminal';

export interface AgentNode {
  id: number;
  mesh_id: number;
  name: string;
  path: string;
  branch: string;
  env: 'windows' | 'wsl';
  provider: 'anthropic' | 'minimax' | 'gemini' | 'opencode';
  status: 'running' | 'idle' | 'awaiting_input' | 'error' | 'suspended' | 'archived';
  cli_session_id?: string;
  worktree_name?: string;
  created_at: string;
}

export interface Checkpoint {
  id: number;
  node_id: number;
  git_ref: string;
  turn_index: number;
  message: string;
  created_at: string;
}

interface AgentNodeState {
  agentNodes: AgentNode[];
  activeNodeId: number | null;
  checkpoints: Checkpoint[];
  loading: boolean;
  error: string | null;

  // Derived getters
  getActiveNode: () => AgentNode | null;
  getActiveMeshId: () => number | null;

  fetchAgentNodes: () => Promise<void>;
  createAgentNode: (meshId: number, name: string, path: string, branch: string, provider?: string) => Promise<AgentNode>;
  deleteAgentNode: (id: number) => Promise<void>;
  setActiveNode: (id: number | null) => Promise<void>;
  fetchCheckpoints: (nodeId: number) => Promise<void>;
  spawnAgent: (nodeId: number, provider: string, rows?: number, cols?: number) => Promise<void>;
  killAgent: (nodeId: number) => Promise<void>;
  sendToAgent: (nodeId: number, input: string) => Promise<void>;
  writeToAgent: (nodeId: number, data: string) => Promise<void>;
  createCheckpoint: (nodeId: number, turnIndex: number, message?: string) => Promise<void>;
  revertCheckpoint: (checkpointId: number) => Promise<void>;
  initAttentionListeners: () => Promise<void>;
}

export const useAgentNodeStore = create<AgentNodeState>((set, get) => ({
  agentNodes: [],
  activeNodeId: null,
  checkpoints: [],
  loading: false,
  error: null,

  getActiveNode: () => {
    const { agentNodes, activeNodeId } = get();
    if (activeNodeId === null) return null;
    return agentNodes.find(s => s.id === activeNodeId) || null;
  },

  getActiveMeshId: () => {
    const node = get().getActiveNode();
    return node?.mesh_id ?? null;
  },

  fetchAgentNodes: async () => {
    set({ loading: true, error: null });
    try {
      const agentNodes = await invoke<AgentNode[]>('list_sessions');
      set({ agentNodes, loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  ...(() => {
    let listenersAttached = false;
    return {
      initAttentionListeners: async () => {
        if (listenersAttached) return;
        listenersAttached = true;

        await listen<{ session_id: number }>('attention-needed', (event) => {
          const nodeId = event.payload.session_id;
          set((state) => ({
            agentNodes: state.agentNodes.map((s) =>
              s.id === nodeId ? { ...s, status: 'awaiting_input' as const } : s
            ),
          }));
        });

        await listen<{ session_id: number }>('attention-cleared', (event) => {
          const nodeId = event.payload.session_id;
          set((state) => ({
            agentNodes: state.agentNodes.map((s) =>
              s.id === nodeId ? { ...s, status: 'running' as const } : s
            ),
          }));
        });

        await listen<{ session_id: number; name: string }>('session-renamed', (event) => {
          const { session_id: nodeId, name } = event.payload;
          set((state) => ({
            agentNodes: state.agentNodes.map((s) =>
              s.id === nodeId ? { ...s, name } : s
            ),
          }));
        });

        // Listen for session-created events from test server (HTTP-based E2E tests)
        await listen<{ id: number }>('session-created', async () => {
          // Refetch agentNodes via invoke since they were created via HTTP test server
          await get().fetchAgentNodes();
        });

        // Listen for session-activated events from test server (HTTP-based E2E tests)
        await listen<{ session_id: number }>('session-activated', (event) => {
          const nodeId = event.payload.session_id;
          set({ activeNodeId: nodeId });
        });
      },
    };
  })(),

  createAgentNode: async (meshId, name, path, branch, provider?: string): Promise<AgentNode> => {
    try {
      const node = await invoke<AgentNode>('create_session', {
        meshId, name, path, branch, provider
      });
      set((state) => ({ agentNodes: [...state.agentNodes, node] }));
      return node;
    } catch (e) {
      set({ error: String(e) });
      throw e;
    }
  },

  deleteAgentNode: async (id) => {
    try {
      await invoke('kill_agent', { sessionId: id });
      disposeTerminal(id);
      // Remove node from local state — no fetch needed
      set((state) => ({
        agentNodes: state.agentNodes.filter(s => s.id !== id),
        activeNodeId: state.activeNodeId === id ? null : state.activeNodeId,
      }));
      // Also delete from backend so node doesn't reappear on refresh
      await invoke('delete_session', { sessionId: id });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  setActiveNode: async (id) => {
    if (id !== null) {
      await get().fetchCheckpoints(id);
    } else {
      set({ checkpoints: [] });
    }
    set({ activeNodeId: id });
  },

  fetchCheckpoints: async (nodeId) => {
    try {
      const checkpoints = await invoke<Checkpoint[]>('list_checkpoints', { sessionId: nodeId });
      set({ checkpoints });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  spawnAgent: async (nodeId, provider, rows?: number, cols?: number) => {
    try {
      const node = get().agentNodes.find(s => s.id === nodeId);
      await invoke('spawn_agent', {
        sessionId: nodeId,
        provider,
        resume: node?.cli_session_id,
        rows,
        cols
      });
      await get().fetchAgentNodes();
    } catch (e) {
      console.error('[agentNodeStore] spawnAgent failed:', e);
      set({ error: String(e) });
      throw e;
    }
  },

  killAgent: async (nodeId) => {
    try {
      await invoke('kill_agent', { sessionId: nodeId });
      await get().fetchAgentNodes();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  sendToAgent: async (nodeId, input) => {
    try {
      await invoke('send_to_agent', { sessionId: nodeId, input });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  writeToAgent: async (nodeId, data) => {
    try {
      await invoke('write_to_agent', { sessionId: nodeId, data });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  createCheckpoint: async (nodeId, turnIndex, message) => {
    try {
      await invoke('create_checkpoint', { sessionId: nodeId, turnIndex, message });
      await get().fetchCheckpoints(nodeId);
    } catch (e) {
      set({ error: String(e) });
    }
  },

  revertCheckpoint: async (checkpointId) => {
    try {
      await invoke('revert_to_checkpoint', { checkpointId });
      const { activeNodeId } = get();
      if (activeNodeId) await get().fetchCheckpoints(activeNodeId);
    } catch (e) {
      set({ error: String(e) });
    }
  },
}));