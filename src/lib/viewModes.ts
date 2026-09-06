import type { AgentNode } from '../types/generated/AgentNode';
import type { GridControls, NonSingleViewMode } from '../stores/uiStore';

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

/** The Grid Controls fields the 'filtered' scope narrows by. The sort pair
 *  is deliberately excluded — 'filtered' reuses the existing grid sorters,
 *  it doesn't need to re-own them. */
export type FilterControls = Pick<GridControls, 'gridSearchQuery' | 'gridProviderFilter' | 'gridStatusFilter'>;

// Neutral controls — the value `AgentNodeView` passes when it has no
// controls context (the old all-nodes default: no search, no filters), so
// the two call shapes share one predicate.
const NO_FILTERS: FilterControls = {
  gridSearchQuery: '',
  gridProviderFilter: null,
  gridStatusFilter: null,
};

/** The shared 'filtered' visibility predicate: free-text name match plus
 *  the provider/status dropdown filters. Exported so `gridFilterSort` and
 *  the traversal shortcut consume the same definition as the grid render —
 *  one predicate, three consumers (the repo's wayfinder #982 discipline). */
export function matchesGridControls(node: AgentNode, controls: FilterControls): boolean {
  const query = controls.gridSearchQuery.trim().toLowerCase();
  if (query && !node.name.toLowerCase().includes(query)) return false;
  if (controls.gridProviderFilter !== null && node.provider !== controls.gridProviderFilter) return false;
  if (controls.gridStatusFilter !== null && node.status !== controls.gridStatusFilter) return false;
  return true;
}

/**
 * The ordered nodes a grid View Mode renders. 'single' is not a grid mode —
 * use `resolveSingleNode` for it.
 *
 *   - 'mesh'     — the resolved mesh scope (see `resolveMeshScopeId`).
 *   - 'pinned'   — every node with `is_pinned`, across all meshes. Pinned
 *                  never touches `selectedMeshId` (ticket #983).
 *   - 'all'      — every loaded node.
 *   - 'filtered' — every loaded node narrowed by the Grid Controls (#1609):
 *                  the free-text search plus the provider/status filters.
 *                  Cross-mesh like Pinned — the sidebar selection is
 *                  irrelevant to the scope (the mesh filter can be
 *                  reproduced by combining it with the search text).
 */
export function scopeNodesForMode(
  mode: NonSingleViewMode,
  agentNodes: AgentNode[],
  selectedMeshId: number | null,
  activeNodeId: number | null,
  controls: FilterControls = NO_FILTERS,
): AgentNode[] {
  switch (mode) {
    case 'pinned':
      return agentNodes.filter(n => n.is_pinned);
    case 'all':
      return agentNodes;
    case 'filtered':
      return agentNodes.filter(n => matchesGridControls(n, controls));
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
 * any node at all, then null (the caller renders the empty state). When the
 * remembered scope is 'filtered' the controls narrow the fallback the same
 * way they narrow the grid, so an active node deleted out from under a solo
 * view falls back to another matching node rather than an unfiltered one.
 */
export function resolveSingleNode(
  agentNodes: AgentNode[],
  activeNodeId: number | null,
  lastNonSingleMode: NonSingleViewMode,
  selectedMeshId: number | null,
  controls: FilterControls = NO_FILTERS,
): AgentNode | null {
  const active = agentNodes.find(n => n.id === activeNodeId);
  if (active) return active;
  return (
    scopeNodesForMode(lastNonSingleMode, agentNodes, selectedMeshId, activeNodeId, controls)[0]
    ?? agentNodes[0]
    ?? null
  );
}
