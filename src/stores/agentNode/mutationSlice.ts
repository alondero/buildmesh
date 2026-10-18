import type { AgentNodeState, SetAgentNodeState, GetAgentNodeState } from './types';
import type { AgentNode } from '../../types/generated/AgentNode';
import { formatError } from '../../lib/errorUtils';
import * as api from '../../lib/tauri/agentNode';
import { withOptimistic, type OptimisticSurface, type OptimisticFieldLedger } from '../../lib/optimistic';

/// Apply a mesh's re-positioned nodes optimistically and persist them. The
/// updated nodes replace that mesh's entries; the whole id list is re-sorted
/// by (mesh_id, position) so the in-memory order matches `list_agent_nodes`.
/// On a backend error we resync from the DB rather than leave the UI out of
/// step.
async function persistPositions(
  set: (partial: Partial<AgentNodeState>) => void,
  get: () => AgentNodeState,
  updatedMeshNodes: AgentNode[],
) {
  // Compute the new id ordering (full list, mesh-sorted, position-sorted).
  // We rebuild both the map and the id list so subscribers that read either
  // see a consistent snapshot — the alternative (mutating only the affected
  // mesh's nodes) would leave a half-sorted state during the optimistic
  // window, which is invisible because the id list still references the same
  // node objects.
  const byId = new Map(updatedMeshNodes.map(n => [n.id, n]));
  const mergedNodes = get().nodeIds
    .map(id => get().nodesById[id])
    .filter(n => n !== undefined)
    .map(n => byId.get(n.id) ?? n)
    .sort((x, y) => x.mesh_id - y.mesh_id || x.position - y.position);
  const mergedById: Record<number, AgentNode> = {};
  const mergedIds: number[] = [];
  for (const n of mergedNodes) {
    mergedById[n.id] = n;
    mergedIds.push(n.id);
  }
  set({ nodesById: mergedById, nodeIds: mergedIds });
  try {
    const updates = updatedMeshNodes.map(n => [n.id, n.position] as [number, number]);
    await api.updateAgentNodePositions(updates);
  } catch (e) {
    set({ error: formatError(e) });
    await get().fetchAgentNodes();
  }
}
export function createMutationSlice(set: SetAgentNodeState, get: GetAgentNodeState, optimisticSurface: OptimisticSurface, optimisticLedger: OptimisticFieldLedger): Pick<AgentNodeState, 'renameAgentNode' | 'setNodePinned' | 'toggleNodePinned' | 'reorderAgentNode' | 'swapAgentNodes'> {
  return {
  renameAgentNode: async (id, name) => {
    // Issue #1054 — precheck matches the pre-refactor behaviour: a
    // missing node is a silent no-op, not a throw. (The two pin
    // actions throw; the rename path was always a quiet return.)
    const prior = get().nodesById[id];
    if (!prior) return;
    // Optimistic update so the UI reflects the new name before the
    // round-trip. The backend emits `node-renamed` on success, which
    // keeps other windows in sync. We do NOT read the mutation's
    // resolved value (rename returns `void`): the name this operation
    // asked for IS the acknowledged value, so the ledger settles it
    // against the value already on screen and writes nothing. What the
    // ledger does provide is ownership — a rename that fails after a
    // newer rename succeeded no longer restores its own stale prior,
    // and the same holds when another window's `node-renamed` lands
    // while this one is in flight.
    await withOptimistic({
      surface: optimisticSurface,
      ledger: optimisticLedger,
      nodeId: id,
      optimisticPatch: { name },
      mutation: () => api.renameAgentNode(id, name),
    });
  },

  setNodePinned: async (nodeId, pinned) => {
    // Optimistic patch so the Pinned Grid view re-renders instantly on
    // a click. The backend's returned row is authoritative for `is_pinned`
    // and for nothing else (issue #2325): its `status`/`name`/session
    // columns are read at response time and are routinely older than the
    // lifecycle and rename observations that landed while the IPC was in
    // flight, so adopting the whole row erased them. On rejection the
    // ledger restores the last acknowledged pin value, and only when no
    // newer pin operation or event owns that column.
    return withOptimistic({
      surface: optimisticSurface,
      ledger: optimisticLedger,
      nodeId,
      optimisticPatch: { is_pinned: pinned },
      mutation: () => api.setNodePinned(nodeId, pinned),
      adoptResult: (updated) => updated,
    });
  },

  toggleNodePinned: async (nodeId) => {
    // Optimistic flip — the visible state is `!prior.is_pinned` until
    // the backend confirms. Same ownership and scoped-adoption policy as
    // `setNodePinned` (issue #2325). Compute the flipped value up front so
    // the ledger claims one value to reconcile against; the helper's
    // `prior` capture handles the "node not loaded" throw.
    const prior = get().nodesById[nodeId];
    if (!prior) {
      throw new Error(`toggleNodePinned: node ${nodeId} is not loaded`);
    }
    return withOptimistic({
      surface: optimisticSurface,
      ledger: optimisticLedger,
      nodeId,
      optimisticPatch: { is_pinned: !prior.is_pinned },
      mutation: () => api.toggleNodePinned(nodeId),
      adoptResult: (updated) => updated,
    });
  },

  // Drag-to-reorder: move `nodeId` to flat `insertIndex` within its own mesh's
  // ordered list, renumber positions 0..n-1, and persist. Optimistic so the
  // grid rearranges instantly; a backend failure resyncs from the DB. Order is
  // mesh-scoped — other meshes' nodes are never touched (matches the
  // same-mesh-only drag guard in the UI). The merged array is re-sorted by
  // (mesh_id, position) to mirror `list_agent_nodes`' ordering exactly.
  reorderAgentNode: async (nodeId, insertIndex) => {
    const dragged = get().nodesById[nodeId];
    if (!dragged) return;
    const meshId = dragged.mesh_id;

    const meshNodes = get().nodeIds
      .map(id => get().nodesById[id])
      .filter((n): n is AgentNode => n !== undefined && n.mesh_id === meshId)
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
    const a = get().nodesById[aId];
    const b = get().nodesById[bId];
    if (!a || !b || a.mesh_id !== b.mesh_id) return;

    const swapped = get().nodeIds
      .map(id => get().nodesById[id])
      .filter((n): n is AgentNode => n !== undefined && n.mesh_id === a.mesh_id)
      .map(n => n.id === aId ? { ...n, position: b.position }
                : n.id === bId ? { ...n, position: a.position }
                : n);
    await persistPositions(set, get, swapped);
  },

  };
}
