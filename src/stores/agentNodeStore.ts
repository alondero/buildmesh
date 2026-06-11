import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { disposeTerminal } from '../components/Terminal/Terminal';
import { hasWorktreeCloseRisk, type WorktreeCloseAction, type WorktreeCloseSafety } from '../lib/worktreeClose';
import { requestWorktreeCloseAction } from './worktreeClosePromptStore';

export interface AgentNode {
  id: number;
  mesh_id: number;
  name: string;
  path: string;
  branch: string;
  env: 'windows' | 'wsl';
  provider: 'anthropic' | 'minimax' | 'kimi' | 'agy' | 'opencode' | 'codex' | 'terminal';
  status: 'running' | 'idle' | 'awaiting_input' | 'error' | 'suspended';
  cli_session_id?: string;
  worktree_name?: string;
  use_worktree: boolean;
  created_at: string;
}

type WorktreeCloseActionResolver = (
  node: AgentNode,
  safety: WorktreeCloseSafety,
) => Promise<WorktreeCloseAction>;

const defaultWorktreeCloseActionResolver: WorktreeCloseActionResolver = (node, safety) =>
  requestWorktreeCloseAction(node.name, safety);

let worktreeCloseActionResolver = defaultWorktreeCloseActionResolver;

export function setWorktreeCloseActionResolverForTests(resolver?: WorktreeCloseActionResolver) {
  worktreeCloseActionResolver = resolver ?? defaultWorktreeCloseActionResolver;
}

interface AgentNodeState {
  agentNodes: AgentNode[];
  activeNodeId: number | null;
  loading: boolean;
  error: string | null;

  // Derived getters
  getActiveNode: () => AgentNode | null;
  getActiveMeshId: () => number | null;

  fetchAgentNodes: () => Promise<void>;
  createAgentNode: (meshId: number, name: string, path: string, branch: string, provider?: string, useWorktree?: boolean) => Promise<AgentNode>;
  deleteAgentNode: (id: number) => Promise<void>;
  renameAgentNode: (id: number, name: string) => Promise<void>;
  setActiveNode: (id: number | null) => void;
  spawnAgent: (nodeId: number, provider: string, rows?: number, cols?: number) => Promise<void>;
  spawnHandoverAgent: (meshId: number, prefill: string, provider?: string) => Promise<AgentNode>;
  killAgent: (nodeId: number) => Promise<void>;
  sendToAgent: (nodeId: number, input: string) => Promise<void>;
  writeToAgent: (nodeId: number, data: string) => Promise<void>;
  initAttentionListeners: () => Promise<void>;
}

export const useAgentNodeStore = create<AgentNodeState>((set, get) => ({
  agentNodes: [],
  activeNodeId: null,
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

  createAgentNode: async (meshId, name, path, branch, provider?: string, useWorktree?: boolean): Promise<AgentNode> => {
    try {
      const node = await invoke<AgentNode>('create_session', {
        meshId, name, path, branch, provider, useWorktree
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
      const node = get().agentNodes.find(s => s.id === id);
      const safety = await invoke<WorktreeCloseSafety>('get_worktree_close_safety', { sessionId: id });
      let removeWorktree = Boolean(safety.worktree_path);

      if (safety.worktree_path && hasWorktreeCloseRisk(safety)) {
        if (!node) {
          throw new Error(`Agent node ${id} is not loaded, cannot confirm worktree removal`);
        }
        const action = await worktreeCloseActionResolver(node, safety);
        if (action === 'cancel') return;
        removeWorktree = action === 'remove';
      }

      // Optimistic close: drop the node from the UI now so closing feels
      // instant. The backend kills the agent and removes the row in a fast
      // Phase 1, then reclaims the worktree directory in the background; a
      // failed cleanup surfaces later via the 'worktree-cleanup-failed' event
      // rather than holding the node on screen while the slow delete runs.
      disposeTerminal(id);
      set((state) => ({
        agentNodes: state.agentNodes.filter(s => s.id !== id),
        activeNodeId: state.activeNodeId === id ? null : state.activeNodeId,
      }));

      try {
        // kill_agent tears the process down before its bookkeeping (a DB status
        // update) can fail, and the node is being deleted anyway — so never let
        // a kill_agent rejection skip delete_session, or the node would vanish
        // from the UI while its row and worktree survive and resurrect on the
        // next fetch.
        try {
          await invoke('kill_agent', { sessionId: id });
        } catch (e) {
          console.warn('[agentNodeStore] kill_agent failed during close, continuing', e);
        }
        await invoke('delete_session', { sessionId: id, removeWorktree });
      } catch (e) {
        set({ error: String(e) });
      }
    } catch (e) {
      set({ error: String(e) });
    }
  },

  renameAgentNode: async (id, name) => {
    const prior = get().agentNodes.find(s => s.id === id);
    if (!prior) return;
    // Optimistic update so the UI reflects the new name before the
    // round-trip. The backend emits `session-renamed` on success, which
    // is a no-op for us (already matches) and keeps other windows in sync.
    set((state) => ({
      agentNodes: state.agentNodes.map(s =>
        s.id === id ? { ...s, name } : s
      ),
    }));
    try {
      await invoke('rename_session', { sessionId: id, name });
    } catch (e) {
      // Roll back the optimistic update so the UI shows the prior name
      // again. The user can retry or fix the input.
      set((state) => ({
        agentNodes: state.agentNodes.map(s =>
          s.id === id ? { ...s, name: prior.name } : s
        ),
        error: String(e),
      }));
      throw e;
    }
  },

  setActiveNode: (id) => {
    // A plain synchronous state write — the active-node highlight, terminal
    // focus, and file-watch all key off activeNodeId, so the switch must feel
    // instant with no backend round-trip in the way.
    set({ activeNodeId: id });
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

  spawnHandoverAgent: async (meshId: number, prefill: string, provider?: string) => {
    try {
      const node = await invoke<AgentNode>('spawn_handover_agent', { meshId, prefill, provider });
      set((state) => ({ agentNodes: [...state.agentNodes, node] }));
      await get().fetchAgentNodes();
      return node;
    } catch (e) {
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
}));
