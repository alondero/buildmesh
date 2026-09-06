import type { AgentNode } from '../../stores/agentNodeStore';
import type { GridControls, ViewMode } from '../../stores/uiStore';
import { scopeNodesForMode } from '../../lib/viewModes';

type GridControlValues = Pick<
  GridControls,
  'gridSearchQuery' | 'gridProviderFilter' | 'gridStatusFilter' | 'gridSortBy' | 'gridSortDirection'
>;

function compareText(left: string, right: string): number {
  const a = left.toLowerCase();
  const b = right.toLowerCase();
  return a < b ? -1 : a > b ? 1 : 0;
}

function compareBySort(a: AgentNode, b: AgentNode, sortBy: GridControls['gridSortBy']): number {
  switch (sortBy) {
    case 'name':
      return compareText(a.name, b.name);
    case 'status':
      return compareText(a.status, b.status);
    case 'created':
      return compareText(a.created_at, b.created_at);
    case 'custom':
      return 0;
  }
}

function compareStableOrder(a: AgentNode, b: AgentNode): number {
  return a.position - b.position || a.id - b.id;
}

/**
 * Derive the ordered grid sequence from the active view scope and the
 * persisted Grid Controls state.
 *
 * Scope and filter are two different layers since #1609: the active View
 * Mode decides WHICH nodes are candidates (mesh / pinned / all — the pure
 * scopes in `src/lib/viewModes.ts`), and the Grid Controls narrow them only
 * inside the dedicated 'filtered' View Mode, which is where the Search
 * Nodes bar lives. An active search therefore never hides nodes behind the
 * user's back in Mesh / Pinned / All — the grid there is always the full
 * scope, sorted.
 */
export function deriveVisibleNodes(
  viewMode: ViewMode,
  agentNodes: AgentNode[],
  selectedMeshId: number | null,
  activeNodeId: number | null,
  controls: GridControlValues,
): AgentNode[] {
  if (viewMode === 'single') return [];

  const scoped = scopeNodesForMode(viewMode, agentNodes, selectedMeshId, activeNodeId, controls);
  const search = controls.gridSearchQuery.trim().toLowerCase();

  if (viewMode !== 'filtered' && (search || controls.gridProviderFilter !== null || controls.gridStatusFilter !== null)) {
    // Belt-and-braces: a filter active outside 'filtered' must not narrow
    // the grid — this branch can only be hit by direct store pokes in
    // tests, never by the shipped UI, which shows the controls only in
    // Filtered. Keeps the "other grids stay unfiltered" contract explicit.
    return scoped;
  }

  const nodes = scoped.filter((node) => {
    if (search && !node.name.toLowerCase().includes(search)) return false;
    if (controls.gridProviderFilter !== null && node.provider !== controls.gridProviderFilter) return false;
    if (controls.gridStatusFilter !== null && node.status !== controls.gridStatusFilter) return false;
    return true;
  });

  if (controls.gridSortBy === 'custom') return nodes;

  const direction = controls.gridSortDirection === 'desc' ? -1 : 1;
  return [...nodes].sort((a, b) => {
    const primary = compareBySort(a, b, controls.gridSortBy);
    return primary === 0 ? compareStableOrder(a, b) : primary * direction;
  });
}
