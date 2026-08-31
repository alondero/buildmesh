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
 * persisted Grid Controls state. Filtering always precedes sorting so every
 * sort applies to exactly the nodes the user can see.
 */
export function deriveVisibleNodes(
  viewMode: ViewMode,
  agentNodes: AgentNode[],
  selectedMeshId: number | null,
  activeNodeId: number | null,
  controls: GridControlValues,
): AgentNode[] {
  if (viewMode === 'single') return [];

  const query = controls.gridSearchQuery.trim().toLowerCase();
  const scoped = scopeNodesForMode(viewMode, agentNodes, selectedMeshId, activeNodeId);
  const filtered = scoped.filter((node) => {
    if (query && !node.name.toLowerCase().includes(query)) return false;
    if (controls.gridProviderFilter !== null && node.provider !== controls.gridProviderFilter) return false;
    if (controls.gridStatusFilter !== null && node.status !== controls.gridStatusFilter) return false;
    return true;
  });

  if (controls.gridSortBy === 'custom') return filtered;

  const direction = controls.gridSortDirection === 'desc' ? -1 : 1;
  return [...filtered].sort((a, b) => {
    const primary = compareBySort(a, b, controls.gridSortBy);
    return primary === 0 ? compareStableOrder(a, b) : primary * direction;
  });
}
