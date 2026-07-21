import type { AgentNode } from '../types/generated/AgentNode';
import type { NonSingleViewMode } from '../stores/uiStore';

/**
 * View Mode visibility rules (wayfinder #982 — state model ticket #983,
 * rendering ticket #986). Pure helpers over the store arrays so the three
 * consumers — `AgentNodeView` rendering, keyboard grid traversal (#987),
 * and unit tests — all read the same definition of "the nodes visible in
 * the active ViewMode".
 *
 * Ordering: every scope returns nodes in the store's canonical order
 * (`agentNodes` is kept sorted by (mesh_id, position) — see
 * `persistPositions` / `list_agent_nodes`). Pinned deliberately reuses it:
 * pins group by mesh, position-ordered within each mesh. Custom
 * drag-reordering inside the Pinned Grid is map fog on #982 ("Not yet
 * specified"), so no pin-specific order exists here yet.
 */

/**
 * The mesh a Mesh Grid scope resolves to. The sidebar selection wins; with
 * no selection we fall back to the active node's mesh (ticket #983: "the
 * switcher's Mesh Grid segment uses `selectedMeshId`, falling back to the
 * active node's mesh"), then to the first loaded node's mesh so a persisted
 * 'mesh' boot never lands on an empty grid while nodes exist.
 */
export function resolveMeshScopeId(
  agentNodes: AgentNode[],
  selectedMeshId: number | null,
  activeNodeId: number | null,
): number | null {
  if (selectedMeshId !== null) return selectedMeshId;
  const activeMeshId = agentNodes.find(n => n.id === activeNodeId)?.mesh_id;
  return activeMeshId ?? agentNodes[0]?.mesh_id ?? null;
}

/**
 * The ordered nodes a grid View Mode renders. 'single' is not a grid mode —
 * use `resolveSingleNode` for it.
 *
 *   - 'mesh'   — the resolved mesh scope (see `resolveMeshScopeId`).
 *   - 'pinned' — every node with `is_pinned`, across all meshes. Pinned
 *                never touches `selectedMeshId` (ticket #983).
 *   - 'all'    — every loaded node.
 */
export function scopeNodesForMode(
  mode: NonSingleViewMode,
  agentNodes: AgentNode[],
  selectedMeshId: number | null,
  activeNodeId: number | null,
): AgentNode[] {
  switch (mode) {
    case 'pinned':
      return agentNodes.filter(n => n.is_pinned);
    case 'all':
      return agentNodes;
    case 'mesh': {
      const meshId = resolveMeshScopeId(agentNodes, selectedMeshId, activeNodeId);
      return meshId === null ? [] : agentNodes.filter(n => n.mesh_id === meshId);
    }
  }
}

/**
 * The node 'single' mode solos (ticket #983: "Single renders the active
 * node; fallback: first node of the current scope; empty state if none").
 * The active node wins regardless of any mesh scope — explicit focus is
 * cross-mesh by nature, like Pinned. With no active node (deleted while
 * soloed, or a boot straight into 'single'), "current scope" means the grid
 * `single` was entered from (`lastNonSingleMode` — the Escape target), then
 * any node at all, then null (the caller renders the empty state).
 */
export function resolveSingleNode(
  agentNodes: AgentNode[],
  activeNodeId: number | null,
  lastNonSingleMode: NonSingleViewMode,
  selectedMeshId: number | null,
): AgentNode | null {
  const active = agentNodes.find(n => n.id === activeNodeId);
  if (active) return active;
  return (
    scopeNodesForMode(lastNonSingleMode, agentNodes, selectedMeshId, activeNodeId)[0]
    ?? agentNodes[0]
    ?? null
  );
}
