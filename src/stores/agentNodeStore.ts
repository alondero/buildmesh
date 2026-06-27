import { create } from 'zustand';
import * as api from '../lib/tauri';
import { listen } from '@tauri-apps/api/event';
import { disposeTerminal } from '../components/Terminal/Terminal';
import { hasWorktreeCloseRisk, type WorktreeCloseAction, type WorktreeCloseSafety } from '../lib/worktreeClose';
import { requestWorktreeCloseAction } from './worktreeClosePromptStore';
import { useMeshStore } from './meshStore';

// `AgentNode` is generated from the Rust `models::AgentNode` struct (issue
// #359), along with the `EnvType`/`Provider`/`SessionStatus` unions it
// references. Imported for local use and re-exported so existing
// `import { AgentNode } from '../stores/agentNodeStore'` call sites keep
// working. The generated type is the full wire shape: nullable
// `cli_session_id`/`worktree_name` are `string | null` (not `?: string`), and
// it adds `env`, `source_issue`, and the `archived` status the hand-written
// interface omitted.
import type { AgentNode } from '../types/generated/AgentNode';
export type { AgentNode };

/// Apply a mesh's re-positioned nodes optimistically and persist them. The
/// updated nodes replace that mesh's entries; the whole array is re-sorted by
/// (mesh_id, position) so the in-memory order matches `list_agent_nodes`. On a
/// backend error we resync from the DB rather than leave the UI out of step.
async function persistPositions(
  set: (partial: Partial<AgentNodeState>) => void,
  get: () => AgentNodeState,
  updatedMeshNodes: AgentNode[],
) {
  const byId = new Map(updatedMeshNodes.map(n => [n.id, n]));
  const merged = get().agentNodes
    .map(n => byId.get(n.id) ?? n)
    .sort((x, y) => x.mesh_id - y.mesh_id || x.position - y.position);
  set({ agentNodes: merged });
  try {
    const updates = updatedMeshNodes.map(n => [n.id, n.position] as [number, number]);
    await api.updateAgentNodePositions(updates);
  } catch (e) {
    set({ error: String(e) });
    await get().fetchAgentNodes();
  }
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
  // Nodes whose close is in flight. The slow part of closing is the worktree
  // safety check (a git status + ref walk) that must run *before* we can drop
  // the row, so we flag the node here the instant the user clicks and let
  // NodeItem show a spinner instead of looking frozen.
  closingNodeIds: Set<number>;

  // Derived getters
  getActiveNode: () => AgentNode | null;
  getActiveMeshId: () => number | null;

  fetchAgentNodes: () => Promise<void>;
  createAgentNode: (meshId: number, name: string, path: string, branch: string, provider?: string, useWorktree?: boolean) => Promise<AgentNode>;
  /// Sidebar "click + or pick provider" entrypoint — creates a node on the
  /// mesh, sets it active, and selects the mesh. The three steps live behind
  /// one action (issue #283) so the invariant — "only switch active mesh/node
  /// if creation succeeded" — is enforced in one place, not re-derivable per
  /// click handler. On `createAgentNode` rejection the active mesh stays put.
  selectProviderForMesh: (meshId: number, meshName: string, meshPath: string, providerId: string, useWorktree?: boolean) => Promise<AgentNode>;
  deleteAgentNode: (id: number) => Promise<void>;
  renameAgentNode: (id: number, name: string) => Promise<void>;
  reorderAgentNode: (nodeId: number, insertIndex: number) => Promise<void>;
  swapAgentNodes: (aId: number, bId: number) => Promise<void>;
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
  closingNodeIds: new Set(),

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
      const agentNodes = await api.listAgentNodes();
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

        // `attention-needed` / `attention-cleared` are the external AttentionHook
        // protocol — the `session_id` payload key is the wire contract with
        // already-deployed agent hooks (CONTEXT.md ambiguity #1 says this stays
        // as "session" intentionally). Map to the internal `node_id` alias for
        // vocabulary consistency inside the store.
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

        // `node-renamed` is the internal event emitted by `rename_agent_node`
        // (renamed from `session-renamed` in issue #490). The payload key
        // follows: `node_id`, not `session_id`.
        await listen<{ node_id: number; name: string }>('node-renamed', (event) => {
          const { node_id: nodeId, name } = event.payload;
          set((state) => ({
            agentNodes: state.agentNodes.map((s) =>
              s.id === nodeId ? { ...s, name } : s
            ),
          }));
        });

        // Listen for node-created events. Two sources emit this:
        //   1. The two-stage desktop spawn flow: `create_issue_node` emits it
        //      after creating the `pending` node, so the sidebar picks up the
        //      new node before stage-2 (start_node_background) finishes.
        //   2. The HTTP-based E2E test server, which creates nodes out of band.
        // In both cases the cleanest recovery is to refetch the full list — the
        // row is already committed by the time the event fires, so we can never
        // lose a node by racing the IPC.
        await listen<{ id: number }>('node-created', async () => {
          await get().fetchAgentNodes();
        });

        // Listen for node-activated events from test server (HTTP-based E2E tests)
        await listen<{ node_id: number }>('node-activated', (event) => {
          const nodeId = event.payload.node_id;
          set({ activeNodeId: nodeId });
        });

        // Two-stage spawn completion: the backend emits this when stage-2
        // (`start_node_background`) finishes the slow work and the agent
        // process is up. Flips the node from 'pending' to 'running'.
        await listen<{ node_id: number }>('node-spawn-completed', (event) => {
          const nodeId = event.payload.node_id;
          set((state) => ({
            agentNodes: state.agentNodes.map((s) =>
              s.id === nodeId ? { ...s, status: 'running' as const } : s
            ),
          }));
        });

        // Two-stage spawn failure: backend already updated the node's DB
        // status to 'error' before emitting — we mirror it in the store so
        // the sidebar/title-bar renders the red badge without a refetch.
        await listen<{ node_id: number; error: string }>('node-spawn-failed', (event) => {
          const nodeId = event.payload.node_id;
          set((state) => ({
            agentNodes: state.agentNodes.map((s) =>
              s.id === nodeId ? { ...s, status: 'error' as const } : s
            ),
          }));
        });
      },
    };
  })(),

  createAgentNode: async (meshId, name, path, branch, provider?: string, useWorktree?: boolean): Promise<AgentNode> => {
    try {
      const node = await api.createAgentNode(meshId, name, path, branch, provider, useWorktree);
      set((state) => ({ agentNodes: [...state.agentNodes, node] }));
      return node;
    } catch (e) {
      set({ error: String(e) });
      throw e;
    }
  },

  selectProviderForMesh: async (meshId, meshName, meshPath, providerId, useWorktree?: boolean): Promise<AgentNode> => {
    // Create FIRST — only switch active mesh/node if creation succeeded.
    // The order is the invariant: pre-refactor this lived in three sequential
    // store calls in Sidebar.handleSelectProvider (#283), where a future hand
    // could re-arrange them and re-introduce the half-applied
    // "mesh selected but no node" state. Holding the order here makes the
    // invariant unit-testable and impossible to violate from a click handler.
    const node = await get().createAgentNode(meshId, meshName, meshPath, 'main', providerId, useWorktree);
    get().setActiveNode(node.id);
    useMeshStore.getState().selectMesh(meshId);
    return node;
  },

  deleteAgentNode: async (id) => {
    // A close already in flight owns this node's teardown; a second click
    // (common while the safety check makes the row look unresponsive) must not
    // fire a duplicate safety/kill/delete round-trip.
    if (get().closingNodeIds.has(id)) return;

    // Flag the node closing *synchronously* so the click registers instantly.
    // We can't drop the row yet — the safety check below decides whether to
    // prompt about uncommitted/unpushed work — so until it resolves NodeItem
    // shows a spinner over the still-present row instead of looking frozen.
    set((state) => ({ closingNodeIds: new Set(state.closingNodeIds).add(id) }));
    const clearClosing = () => set((state) => {
      const next = new Set(state.closingNodeIds);
      next.delete(id);
      return { closingNodeIds: next };
    });

    // Capture the row up-front so a Phase-2 (delete_commit) failure can
    // re-insert it. By the time the IPC rejects the row is already gone from
    // `agentNodes`, so reading from get() would not find it.
    const node = get().agentNodes.find(s => s.id === id);

    // Phase 1: safety check + worktree-confirmation prompt. Failures here
    // release the closing flag (the row is still on screen and retryable) and
    // surface `state.error` for the App.tsx toast pipeline. The promise
    // resolves — callers don't need to react to a transient safety-check
    // rejection any more than they do to the existing `kill_agent` warn-only.
    let removeWorktree = false;
    try {
      const safety = await api.getWorktreeCloseSafety(id);
      removeWorktree = Boolean(safety.worktree_path);

      if (safety.worktree_path && hasWorktreeCloseRisk(safety)) {
        if (!node) {
          throw new Error(`Agent node ${id} is not loaded, cannot confirm worktree removal`);
        }
        const action = await worktreeCloseActionResolver(node, safety);
        if (action === 'cancel') { clearClosing(); return; }
        removeWorktree = action === 'remove';
      }
    } catch (e) {
      clearClosing();
      set({ error: String(e) });
      return;
    }

    // Phase 2: optimistic close + backend commit. The row drops from the UI
    // here so closing feels instant; the backend kills the agent and removes
    // the row in a fast Phase 1, then reclaims the worktree directory in the
    // background (a failed cleanup surfaces later via the
    // 'worktree-cleanup-failed' event rather than holding the node on screen
    // while the slow delete runs).
    disposeTerminal(id);
    set((state) => {
      const closing = new Set(state.closingNodeIds);
      closing.delete(id);
      return {
        agentNodes: state.agentNodes.filter(s => s.id !== id),
        activeNodeId: state.activeNodeId === id ? null : state.activeNodeId,
        closingNodeIds: closing,
      };
    });

    // kill_agent tears the process down before its bookkeeping (a DB status
    // update) can fail, and the node is being deleted anyway — so never let
    // a kill_agent rejection skip delete_agent_node, or the node would vanish
    // from the UI while its row and worktree survive and resurrect on the
    // next fetch.
    try {
      await api.killAgent(id);
    } catch (e) {
      console.warn('[agentNodeStore] kill_agent failed during close, continuing', e);
    }

    try {
      await api.deleteAgentNode(id, removeWorktree);
    } catch (e) {
      // Issue #645: restore the optimistically-removed row so UI/DB stay in
      // sync. Without this the catch would silently swallow the rejection —
      // `state.error` alone is invisible to the user (no toast), and any
      // unrelated fetchAgentNodes (node-created, mesh switch, …) would
      // resurrect the row anyway, making it look like a "zombie" close.
      // Re-inserting here makes the resurrection immediate and the failure
      // visible via the App.tsx state.error → 'System' toast pipeline
      // (App.tsx:186-190). Re-throw to match createAgentNode / renameAgentNode
      // so callers awaiting the close can react.
      set((state) => ({
        agentNodes: node ? [...state.agentNodes, node] : state.agentNodes,
        error: String(e),
      }));
      throw e;
    }
  },

  renameAgentNode: async (id, name) => {
    const prior = get().agentNodes.find(s => s.id === id);
    if (!prior) return;
    // Optimistic update so the UI reflects the new name before the
    // round-trip. The backend emits `node-renamed` on success, which
    // is a no-op for us (already matches) and keeps other windows in sync.
    set((state) => ({
      agentNodes: state.agentNodes.map(s =>
        s.id === id ? { ...s, name } : s
      ),
    }));
    try {
      await api.renameAgentNode(id, name);
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

  // Drag-to-reorder: move `nodeId` to flat `insertIndex` within its own mesh's
  // ordered list, renumber positions 0..n-1, and persist. Optimistic so the
  // grid rearranges instantly; a backend failure resyncs from the DB. Order is
  // mesh-scoped — other meshes' nodes are never touched (matches the
  // same-mesh-only drag guard in the UI). The merged array is re-sorted by
  // (mesh_id, position) to mirror `list_agent_nodes`' ordering exactly.
  reorderAgentNode: async (nodeId, insertIndex) => {
    const dragged = get().agentNodes.find(n => n.id === nodeId);
    if (!dragged) return;
    const meshId = dragged.mesh_id;

    const meshNodes = get().agentNodes
      .filter(n => n.mesh_id === meshId)
      .sort((a, b) => a.position - b.position);
    const from = meshNodes.findIndex(n => n.id === nodeId);
    if (from === -1) return;

    let to = insertIndex;
    meshNodes.splice(from, 1);
    if (from < to) to -= 1; // removing an earlier element shifts the target left
    to = Math.max(0, Math.min(to, meshNodes.length));
    meshNodes.splice(to, 0, dragged);
    if (from === to) return; // no-op (e.g. dropped on its own boundary)

    const renumbered = meshNodes.map((n, i) => ({ ...n, position: i }));
    await persistPositions(set, get, renumbered);
  },

  // Swap the grid slots of two nodes in the same mesh by exchanging their
  // `position` values. Only those two rows change, so we send just two updates.
  swapAgentNodes: async (aId, bId) => {
    if (aId === bId) return;
    const a = get().agentNodes.find(n => n.id === aId);
    const b = get().agentNodes.find(n => n.id === bId);
    if (!a || !b || a.mesh_id !== b.mesh_id) return;

    const swapped = get().agentNodes
      .filter(n => n.mesh_id === a.mesh_id)
      .map(n => n.id === aId ? { ...n, position: b.position }
                : n.id === bId ? { ...n, position: a.position }
                : n);
    await persistPositions(set, get, swapped);
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
      await api.spawnAgent(nodeId, provider, node?.cli_session_id, rows, cols);
      await get().fetchAgentNodes();
    } catch (e) {
      // The central IPC wrapper (src/lib/tauri.ts → _invoke) already logs
      // every rejection to buildmesh.log as `[IPC:spawn_agent] args=… — <err>`,
      // so the prior `console.error` here produced a duplicate entry. The
      // store-side catch keeps doing two things the wrapper does not: it
      // surfaces the error on `state.error` for the UI to render, and it
      // re-throws so the caller's catch can react.
      set({ error: String(e) });
      throw e;
    }
  },

  spawnHandoverAgent: async (meshId: number, prefill: string, provider?: string) => {
    try {
      const node = await api.spawnHandoverAgent(meshId, prefill, provider);
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
      await api.killAgent(nodeId);
      await get().fetchAgentNodes();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  sendToAgent: async (nodeId, input) => {
    try {
      await api.sendToAgent(nodeId, input);
    } catch (e) {
      set({ error: String(e) });
    }
  },

  writeToAgent: async (nodeId, data) => {
    try {
      await api.writeToAgent(nodeId, data);
    } catch (e) {
      set({ error: String(e) });
    }
  },
}));
