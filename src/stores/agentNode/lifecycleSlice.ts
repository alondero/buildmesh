import type { AgentNodeState, SetAgentNodeState, GetAgentNodeState, AgentNodeRuntime } from './types';
import type { AgentNode } from '../../types/generated/AgentNode';
import { formatError } from '../../lib/errorUtils';
import * as api from '../../lib/tauri/agent';
import { addToast } from '../toastStore';
import type { SpawnAgentIntent } from '../../types/generated/SpawnAgentIntent';
import type { SpawnAgentOptions } from './types';

function resolveSpawnAgentIntent(
  options: SpawnAgentOptions,
  node: AgentNode | undefined,
): SpawnAgentIntent {
  const trimmed = options.prefill?.trim() ?? '';
  if (trimmed !== '') {
    return { type: 'loop', initial_prompt: trimmed };
  }
  if (options.fresh) {
    return { type: 'fresh' };
  }
  if (node?.cli_session_id) {
    return { type: 'resume' };
  }
  return { type: 'fresh' };
}
export function createLifecycleSlice(set: SetAgentNodeState, get: GetAgentNodeState, runtime: AgentNodeRuntime): Pick<AgentNodeState, 'spawnAgent' | 'regenerateAgentNode' | 'restartFreshAgent' | 'spawnHandoverAgent' | 'killAgent' | 'sendToAgent' | 'writeToAgent' | 'handoverToNode' | 'clearAttention'> {
  return {
  spawnAgent: async (nodeId, provider, rowsOrOptions, maybeCols) => {
    const options: SpawnAgentOptions =
      typeof rowsOrOptions === 'object' && rowsOrOptions !== null
        ? rowsOrOptions
        : {
            rows: typeof rowsOrOptions === 'number' ? rowsOrOptions : undefined,
            cols: maybeCols,
          };
    const node = get().nodesById[nodeId];
    if (!node || node.status === 'archived') return;
    const request = Symbol('spawn');
    runtime.spawnRequests.set(nodeId, request);
    runtime.failedAutoSpawns.delete(nodeId);
    runtime.nodeRevision += 1;
    const intent = resolveSpawnAgentIntent(options, node);
    // Flip idle → spawning *before* the first await so Terminal's
    // auto-spawn effect (keyed on `status === 'idle'`) cannot start a
    // second spawn_agent while this one is in flight. Issue #1384 —
    // optimistic patch goes through `nodesById` so other entries keep
    // their identity.
    if (['idle', 'error', 'suspended'].includes(node.status)) {
      set((state) => ({
        nodesById: {
          ...state.nodesById,
          [nodeId]: { ...state.nodesById[nodeId]!, status: 'spawning' },
        },
      }));
    }
    try {
      await api.spawnAgent({
        sessionId: nodeId,
        provider,
        intent,
        rows: options.rows ?? null,
        cols: options.cols ?? null,
      });
      if (runtime.spawnRequests.get(nodeId) !== request) return;
    } catch (e) {
      // The central IPC wrapper (src/lib/tauri.ts → _invoke) already logs
      // every rejection to buildmesh.log as `[IPC:spawn_agent] args=… — <err>`,
      // so the prior `console.error` here produced a duplicate entry. The
      // store-side catch keeps doing two things the wrapper does not: it
      // surfaces the error on `state.error` for the UI to render, and it
      // re-throws so the caller's catch can react. Issue #1384 —
      // patch through `nodesById` so other entries keep their identity.
      const current = get().nodesById[nodeId];
      if (runtime.spawnRequests.get(nodeId) === request && current &&
          ['idle', 'spawning', 'error'].includes(current.status)) {
        runtime.failedAutoSpawns.add(nodeId);
        runtime.nodeRevision += 1;
        set((state) => ({
          nodesById: {
            ...state.nodesById,
            [nodeId]: { ...state.nodesById[nodeId]!, status: 'error' },
          },
          error: formatError(e),
        }));
      }
      throw e;
    } finally {
      if (runtime.spawnRequests.get(nodeId) === request) runtime.spawnRequests.delete(nodeId);
    }
    // List refresh is independent of launch success. A failed refresh must
    // never reclassify a provider that the backend has already launched.
    await get().fetchAgentNodes();
  },

  regenerateAgentNode: async (nodeId, newProviderId) => {
    try {
      // The backend's returned `AgentNode` is the pre-spawn snapshot
      // (services::agent_node::regenerate reloads after the provider
      // UPDATE but BEFORE `spawn_agent_inner` mutates status / captures
      // `cli_session_id` / fires the `agent-spawned` event). A local
      // patch from that snapshot would clobber the event-driven state
      // — match `spawnAgent`'s pattern below and refetch instead. The
      // refetch reads the DB, which `spawn_agent_inner` has already
      // written, so the new `provider` / `cli_session_id` / `status`
      // all line up.
      await api.regenerateAgentNode(nodeId, newProviderId);
      await get().fetchAgentNodes();
      const updated = get().nodesById[nodeId];
      if (!updated) {
        throw new Error(`regenerate_agent_node: node ${nodeId} not found after refetch`);
      }
      return updated;
    } catch (e) {
      set({ error: formatError(e) });
      throw e;
    }
  },

  restartFreshAgent: async (nodeId, options) => {
    const node = get().nodesById[nodeId];
    if (!node) {
      throw new Error(`Node ${nodeId} not found`);
    }
    return get().spawnAgent(nodeId, node.provider, { ...options, fresh: true });
  },

  spawnHandoverAgent: async (meshId: number, prefill: string, provider?: string) => {
    try {
      const node = await api.spawnHandoverAgent(meshId, prefill, provider);
      set((state) => ({
        nodesById: { ...state.nodesById, [node.id]: node },
        nodeIds: state.nodeIds.includes(node.id) ? state.nodeIds : [...state.nodeIds, node.id],
      }));
      await get().fetchAgentNodes();
      return node;
    } catch (e) {
      // Toast here rather than `state.error`: App.tsx already turns that
      // field into a generic "System" toast, and the terminal catch would
      // otherwise stack a second copy of the same sentence.
      addToast('Handover', formatError(e), 'error');
      throw e;
    }
  },

  killAgent: async (nodeId) => {
    try {
      await api.killAgent(nodeId);
      // Issue #1530 — the agent's PTY is gone, so anything the transport's
      // retry buffer is still holding for it can never be delivered. Cancel the
      // lane instead of leaving it to retry against a dead process: the
      // keystrokes settle as `closed` and the lane is released.
      api.cancelTerminalInput(nodeId);
      await get().fetchAgentNodes();
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  sendToAgent: async (nodeId, input) => {
    try {
      await api.sendToAgent(nodeId, input);
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  writeToAgent: async (nodeId, data) => {
    try {
      await api.writeToAgent(nodeId, data);
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  handoverToNode: async (nodeId, text) => {
    try {
      await api.handoverToAgent(nodeId, text);
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  clearAttention: async (nodeId) => {
    try { await api.clearAttentionNode(nodeId); } catch (e) { set({ error: formatError(e) }); }
  },

  };
}
