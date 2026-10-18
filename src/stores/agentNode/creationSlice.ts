import type { AgentNodeState, SetAgentNodeState, GetAgentNodeState, AgentNodeRuntime } from './types';
import type { AgentNode } from '../../types/generated/AgentNode';
import { formatError } from '../../lib/errorUtils';
import * as api from '../../lib/tauri/agentNode';


export function createCreationSlice(set: SetAgentNodeState, get: GetAgentNodeState, runtime: AgentNodeRuntime): Pick<AgentNodeState, 'createAgentNode' | 'adoptAgentNode' | 'selectProviderForMesh'> {
  return {
  createAgentNode: async (meshId, name, path, branch, provider?: string, useWorktree?: boolean, configurationId?: string, initialPrompt?: string): Promise<AgentNode> => {
    try {
      const node = await api.createAgentNode(meshId, name, path, branch, provider, useWorktree, configurationId, initialPrompt);
      set((state) => ({
        nodesById: { ...state.nodesById, [node.id]: node },
        nodeIds: [...state.nodeIds, node.id],
      }));
      return node;
    } catch (e) {
      set({ error: formatError(e) });
      throw e;
    }
  },

  adoptAgentNode: (node) => {
    if (get().nodesById[node.id]) return;
    runtime.nodeRevision += 1;
    // Insert-if-absent (never clobber): the `node-created` listener's fetch may
    // already have delivered a fresher row, and this is only a bridge so a
    // caller can act on the id before that fetch lands.
    set((state) => ({
      nodesById: { ...state.nodesById, [node.id]: node },
      nodeIds: [...state.nodeIds, node.id],
    }));
  },

  selectProviderForMesh: async (meshId, meshName, meshPath, providerId, useWorktree?: boolean, initialPrompt?: string, configurationId?: string): Promise<AgentNode> => {
    // Create FIRST — only switch active mesh/node if creation succeeded.
    // The order is the invariant: pre-refactor this lived in three sequential
    // store calls in Sidebar.handleSelectProvider (#283), where a future hand
    // could re-arrange them and re-introduce the half-applied
    // "mesh selected but no node" state. Holding the order here makes the
    // invariant unit-testable and impossible to violate from a click handler.
    const { focusNode } = await import('../../lib/navigation');
    const node = await get().createAgentNode(meshId, meshName, meshPath, 'main', providerId, useWorktree, configurationId, initialPrompt);
    focusNode(node.id, { view: 'mesh', scope: 'node' });
    // Explicit create-then-spawn (issue #1413 review): the prompt rides
    // on this call, not a store-side dictionary the Terminal mount
    // later peeks at. `spawnAgent` flips the row to `spawning`
    // synchronously so the idle auto-spawn effect cannot start a
    // parallel Fresh spawn. Whitespace-only is treated as "no prompt"
    // and Terminal auto-spawns as before.
    const trimmed = initialPrompt?.trim() ?? '';
    if (trimmed !== '') {
      await get().spawnAgent(node.id, providerId, { prefill: trimmed });
    }
    return node;
  },

  };
}
