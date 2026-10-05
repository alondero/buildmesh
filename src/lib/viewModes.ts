import type { AgentNode } from '../types/generated/AgentNode';
import type { Mesh } from '../types/generated/Mesh';
import type { GridControls, NonSingleViewMode, ViewMode } from '../stores/uiStore';

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

/** The Grid Controls fields the 'filtered' scope narrows by. The sort pair
 *  is deliberately excluded — 'filtered' reuses the existing grid sorters,
 *  it doesn't need to re-own them. `gridProviderFilter`/`gridStatusFilter`
 *  are PRE-EXISTING store fields (#998 era, no setter UI ships yet — see
 *  GridControls' header); #1609 carries them forward untouched rather than
 *  deleting a persisted public field, so the predicate is written once and
 *  lights up when #997 adds the popover. */
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
 *   - 'mesh'     — the sidebar-selected Mesh's nodes, and NOTHING else
 *                  (#2071). There is no fallback chain: with no selection
 *                  the Mesh Grid has no Mesh to scope to and renders its
 *                  "no Mesh selected" empty state, because silently
 *                  showing the focused node's Mesh (or the first loaded
 *                  Mesh) showed the user a scope they never chose.
 *   - 'pinned'   — every node with `is_pinned`, across all meshes. Pinned
 *                  never touches `selectedMeshId` (ticket #983).
 *   - 'all'      — every loaded node.
 *   - 'filtered' — every loaded node narrowed by the Grid Controls (#1609):
 *                  the free-text search plus the provider/status filters.
 *                  Cross-mesh like Pinned — the sidebar selection is
 *                  irrelevant to the scope. KNOWN CONSTRAINT (deliberate,
 *                  not an oversight): the search matches `node.name` only,
 *                  so "search inside one mesh" is NOT expressible — this
 *                  mode cannot combine a mesh scope with a query. That
 *                  would need a scope combinator or a mesh axis on the
 *                  controls, which wayfinder #982 leaves "Not yet
 *                  specified"; Ctrl+F from a mesh therefore lands in the
 *                  global pool (App.tsx `focus-grid-search`).
 */
export function scopeNodesForMode(
  mode: NonSingleViewMode,
  agentNodes: AgentNode[],
  selectedMeshId: number | null,
  // Retained for call-shape compatibility (resolveSingleNode and the other
  // callers pass it positionally) — but no scope reads it now that the
  // Mesh fallback chain is gone (#2071).
  _activeNodeId: number | null,
  controls: FilterControls = NO_FILTERS,
): AgentNode[] {
  switch (mode) {
    case 'pinned':
      return agentNodes.filter(n => n.is_pinned);
    case 'all':
      return agentNodes;
    case 'filtered':
      return agentNodes.filter(n => matchesGridControls(n, controls));
    case 'mesh':
      return selectedMeshId === null ? [] : agentNodes.filter(n => n.mesh_id === selectedMeshId);
  }
}

/** One Mesh in a derived scope. `name` is null when that Mesh isn't in the
 *  loaded list (deleted mid-session, or still loading) — a surface shows the
 *  id rather than inventing a label for a Mesh it has never seen. */
export interface ScopeMesh {
  id: number;
  name: string | null;
}

/** Everything the scope is derived from. `lastNonSingleMode` is the grid
 *  mode Single was entered from — Single is explicit focus, so it has no
 *  grid scope of its own and the scope it reports is the one Escape returns
 *  to. */
export interface ScopeInput {
  viewMode: ViewMode;
  lastNonSingleMode: NonSingleViewMode;
  agentNodes: AgentNode[];
  selectedMeshId: number | null;
  activeNodeId: number | null;
  /** The loaded Meshes, used only to resolve the display names the scope
   *  reports. Omit it from a surface that renders no name — the Mesh's
   *  identity in a scope is its id, never its label. */
  meshes?: readonly Mesh[];
  controls?: FilterControls;
}

export interface DerivedScope {
  /** The View Mode as the user set it — 'single' included. */
  viewMode: ViewMode;
  /** The View Mode that actually scopes the grid: `lastNonSingleMode`
   *  while Single is soloing a node, `viewMode` otherwise. */
  gridMode: NonSingleViewMode;
  /** The Mesh the scope is anchored to, or null when the View Mode is
   *  cross-Mesh ('all', 'pinned', 'filtered') or when Mesh Grid has no
   *  selection. */
  mesh: ScopeMesh | null;
  /** `mesh !== null` — whether the scope is Mesh-scoped at all. */
  isMeshScoped: boolean;
  /** The ordered nodes the scope shows right now (grouping into Node
   *  Activity cards and the Grid sorters happen downstream). */
  visibleNodes: AgentNode[];
  /** `visibleNodes.length`, for surfaces that render the number. */
  visibleNodeCount: number;
  /** How many nodes the scope holds BEFORE the Grid Controls narrow it —
   *  the empty state needs the two counts apart to tell "nothing in scope"
   *  from "the filters excluded everything" (#1536). */
  scopedNodeCount: number;
  /** The Mesh the focused Agent Node contributes when it differs from
   *  `mesh`. Single soloing another Mesh's node is the motivating case: the
   *  grid scope is one Mesh while the soloed node belongs to another, and
   *  a cross-Mesh scope has no Mesh of its own for the node to match. */
  focusedNodeMesh: ScopeMesh | null;
}

/**
 * The effective scope, derived once (#2071).
 *
 * Every surface that has to answer "what is the user looking at, and which
 * Mesh is it" — the grid render, the canvas empty state's counts, keyboard
 * grid traversal — reads this instead of re-deriving scope from the sidebar
 * selection and the focused node. One definition, several consumers: the
 * repo's documented discipline, with the fallback chain removed so no surface
 * can quietly disagree with another about the scope.
 */
export function deriveScope(input: ScopeInput): DerivedScope {
  const { viewMode, lastNonSingleMode, agentNodes, selectedMeshId, activeNodeId } = input;
  const meshes = input.meshes ?? [];
  const gridMode: NonSingleViewMode = viewMode === 'single' ? lastNonSingleMode : viewMode;
  const scopedNodes = scopeNodesForMode(gridMode, agentNodes, selectedMeshId, activeNodeId);
  // The scope above is deliberately un-narrowed, so the empty state can
  // tell "nothing is in scope" from "the filters excluded everything".
  // Only 'filtered' narrows, and it narrows through the same shared
  // predicate the grid and traversal use (#1609) — which is also what
  // keeps a stale search from narrowing Mesh/Pinned/All.
  const visibleNodes = gridMode === 'filtered'
    ? scopedNodes.filter(n => matchesGridControls(n, input.controls ?? NO_FILTERS))
    : scopedNodes;
  const meshId = gridMode === 'mesh' ? selectedMeshId : null;
  const focusedMeshId = agentNodes.find(n => n.id === activeNodeId)?.mesh_id ?? null;
  const meshOf = (id: number): ScopeMesh => ({ id, name: meshes.find(m => m.id === id)?.name ?? null });
  return {
    viewMode,
    gridMode,
    mesh: meshId === null ? null : meshOf(meshId),
    isMeshScoped: meshId !== null,
    visibleNodes,
    visibleNodeCount: visibleNodes.length,
    scopedNodeCount: scopedNodes.length,
    focusedNodeMesh: focusedMeshId !== null && focusedMeshId !== meshId ? meshOf(focusedMeshId) : null,
  };
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
