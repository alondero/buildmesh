import type { AgentNode } from '../../stores/agentNodeStore';
import type { GridControls } from '../../stores/uiStore';
import type { DerivedScope } from '../../lib/viewModes';
import { groupActivityNodes, indexAgentNodes, type NodeOwnerships, type NodeGroups } from '../../lib/nodeActivities';

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
 * Derive the ordered grid sequence from the derived scope and the
 * persisted Grid Controls state.
 *
 * This function has exactly one job: ORDERING. Which nodes are candidates
 * — including the Mesh scope, and the Grid Controls narrowing that applies
 * only inside the 'filtered' View Mode — is owned entirely by
 * `deriveScope` (src/lib/viewModes.ts), which this function receives as
 * an argument rather than re-deriving (#2071). There must be no filter
 * logic and no control-dependent early return here: the search text
 * persists in the store across mode switches (#1609), so any guard on
 * it would run on every Mesh/Pinned/All render and, by skipping the sort
 * below, silently serve store-insertion order. Sorting runs
 * unconditionally on whatever the scope contains.
 */
export function deriveVisibleNodes(
  scope: DerivedScope,
  agentNodes: AgentNode[],
  controls: GridControlValues,
  ownerships: NodeOwnerships = {},
  groups: NodeGroups = [],
): AgentNode[] {
  if (scope.viewMode === 'single') return [];

  const nodes = groupActivityNodes(scope.visibleNodes, indexAgentNodes(agentNodes), ownerships, groups);

  if (controls.gridSortBy === 'custom') return nodes;

  const direction = controls.gridSortDirection === 'desc' ? -1 : 1;
  return [...nodes].sort((a, b) => {
    const primary = compareBySort(a, b, controls.gridSortBy);
    return primary === 0 ? compareStableOrder(a, b) : primary * direction;
  });
}
