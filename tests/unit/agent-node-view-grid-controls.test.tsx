/**
 * Grid Controls (wayfinder #988 / #996; Filtered view #1609) — the
 * AgentNodeView must apply the uiStore controls to the node sequence it
 * hands to the grid, and manual drag-and-drop must disappear while a
 * non-custom sort is active. Since #1609 the search/filter narrowing is a
 * property of the dedicated Filtered view: the same store controls must
 * NOT narrow the other grids (a stale search never hides nodes behind the
 * user's back in Mesh/Pinned/All).
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { act, render, screen } from '@testing-library/react';
import type { AgentNode } from '../../src/stores/agentNodeStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';

vi.mock('../../src/components/AgentNodeView/NodeCard', () => ({
  // Issue #1384 — NodeCard now subscribes per-id via `state.nodesById[nodeId]`
  // and passes `nodeId` instead of `node`. The mock mirrors the new prop
  // shape so the test surface doesn't depend on the subscription
  // implementation.
  NodeCard: ({ nodeId, draggable = true }: { nodeId: number; draggable?: boolean }) => {
    // The mock factory is hoisted above the import, so the imported
    // `useAgentNodeStore` binding is in scope by render-time.
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const node = useAgentNodeStore.getState().nodesById[nodeId];
    if (!node) return null;
    return (
      <div data-testid="grid-node" data-node-id={node.id} data-draggable={draggable}>
        {node.name}
      </div>
    );
  },
}));

vi.mock('../../src/components/AgentNodeView/GridSplitter', () => ({
  GridSplitter: ({ nodes, draggable = true }: { nodes: AgentNode[]; draggable?: boolean }) => (
    <div data-testid="grid-output" data-draggable={draggable}>
      {nodes.map((node) => (
        <span key={node.id} data-testid="grid-node" data-node-id={node.id}>
          {node.name}
        </span>
      ))}
    </div>
  ),
}));

import { AgentNodeView } from '../../src/components/AgentNodeView/AgentNodeView';
import { seedAgentNodes } from './helpers/seedAgentNodes';

function makeNode(overrides: Partial<AgentNode>): AgentNode {
  return {
    id: 1,
    mesh_id: 1,
    name: 'Alpha Runner',
    path: '/repo/1',
    branch: 'main',
    env: 'windows',
    provider: 'anthropic',
    status: 'running',
    cli_session_id: null,
    worktree_name: null,
    use_worktree: false,
    is_pinned: false,
    source_issue: null,
    source_pr: null,
    head_repo_owner: null,
    head_repo_clone_url: null,
    source_pr_pinned_sha: null,
    position: 0,
    created_at: '2026-01-01T00:00:00Z',
    ...overrides,
  };
}

const NODES: AgentNode[] = [
  makeNode({ id: 1, name: 'Alpha Runner', provider: 'anthropic', status: 'running', position: 0, created_at: '2026-01-01T00:00:00Z' }),
  makeNode({ id: 2, name: 'Beta Builder', provider: 'minimax', status: 'idle', position: 1, created_at: '2026-01-03T00:00:00Z' }),
  makeNode({ id: 3, name: 'Gamma Guard', provider: 'anthropic', status: 'error', position: 2, created_at: '2026-01-02T00:00:00Z' }),
  makeNode({ id: 4, name: 'Alpha Analyst', provider: 'anthropic', status: 'idle', position: 3, created_at: '2026-01-02T00:00:00Z' }),
];

function renderedNodeIds(): number[] {
  return screen.getAllByTestId('grid-node').map((node) => Number(node.getAttribute('data-node-id')));
}

beforeEach(() => {
  seedAgentNodes(NODES, NODES[0].id);
  useMeshStore.setState({ selectedMeshId: null });
  useUIStore.setState({
    viewMode: 'all',
    lastNonSingleMode: 'all',
    gridSearchQuery: '',
    gridProviderFilter: null,
    gridStatusFilter: null,
    gridSortBy: 'custom',
    gridSortDirection: 'asc',
    probeOpen: false,
    activeDiffFile: null,
  });
});

describe('AgentNodeView grid controls', () => {
  it('filters by the search query in the Filtered view', () => {
    useUIStore.setState({ viewMode: 'filtered', lastNonSingleMode: 'filtered', gridSearchQuery: 'runner' });

    render(<AgentNodeView />);

    expect(renderedNodeIds()).toEqual([1]);
  });

  it('filters by provider in the Filtered view', () => {
    useUIStore.setState({ viewMode: 'filtered', lastNonSingleMode: 'filtered', gridProviderFilter: 'anthropic' });

    render(<AgentNodeView />);

    expect(renderedNodeIds()).toEqual([1, 3, 4]);
  });

  it('filters by status in the Filtered view', () => {
    useUIStore.setState({ viewMode: 'filtered', lastNonSingleMode: 'filtered', gridStatusFilter: 'idle' });

    render(<AgentNodeView />);

    expect(renderedNodeIds()).toEqual([2, 4]);
  });

  it('composes search, provider, and status filters before rendering the Filtered grid', () => {
    useUIStore.setState({
      viewMode: 'filtered',
      lastNonSingleMode: 'filtered',
      gridSearchQuery: 'a',
      gridProviderFilter: 'anthropic',
      gridStatusFilter: 'error',
    });

    render(<AgentNodeView />);

    expect(renderedNodeIds()).toEqual([3]);
  });

  it('does NOT let a stale search narrow the All view (#1609)', () => {
    // The controls are the Filtered view's property now — All renders its
    // full scope even when the store still carries search text.
    useUIStore.setState({ gridSearchQuery: 'runner' });

    render(<AgentNodeView />);

    expect(renderedNodeIds()).toEqual([1, 2, 3, 4]);
  });

  it('does NOT let stale filters narrow the Mesh view (#1609)', () => {
    useAgentNodeStore.setState({
      nodesById: Object.fromEntries(NODES.map(n => [n.id, { ...n, mesh_id: 1 }])),
      nodeIds: NODES.map(n => n.id),
    });
    useMeshStore.setState({ selectedMeshId: 1 });
    useUIStore.setState({
      viewMode: 'mesh',
      gridSearchQuery: 'runner',
      gridProviderFilter: 'anthropic',
    });

    render(<AgentNodeView />);

    expect(renderedNodeIds()).toEqual([1, 2, 3, 4]);
  });

  // Regression guards (#1609 review): the search text persists across mode
  // switches by design, so any control-dependent early return between
  // scoping and sorting would run on EVERY non-Filtered render and silently
  // serve store-insertion order instead of the configured sort.
  it('sorts the All view while a stale search query is active (#1609)', () => {
    useUIStore.setState({ gridSearchQuery: 'runner', gridSortBy: 'name', gridSortDirection: 'asc' });

    render(<AgentNodeView />);

    // Full scope (the stale query narrows nothing), still name-sorted.
    expect(renderedNodeIds()).toEqual([4, 1, 2, 3]);
  });

  it('sorts the Mesh view while a stale search query is active (#1609)', () => {
    useAgentNodeStore.setState({
      nodesById: Object.fromEntries(NODES.map(n => [n.id, { ...n, mesh_id: 1 }])),
      nodeIds: NODES.map(n => n.id),
    });
    useMeshStore.setState({ selectedMeshId: 1 });
    useUIStore.setState({
      viewMode: 'mesh',
      gridSearchQuery: 'runner',
      gridSortBy: 'created',
      gridSortDirection: 'desc',
    });

    render(<AgentNodeView />);

    // Newest first: 2 (Jan 3), then 3 & 4 (Jan 2, position tie-break), 1 (Jan 1).
    expect(renderedNodeIds()).toEqual([2, 3, 4, 1]);
  });

  it('sorts the Pinned view while a stale search query is active (#1609)', () => {
    useAgentNodeStore.setState({
      nodesById: Object.fromEntries(
        NODES.map((n, i) => [n.id, { ...n, is_pinned: i % 2 === 0 }]),
      ),
      nodeIds: NODES.map(n => n.id),
    });
    useUIStore.setState({
      viewMode: 'pinned',
      lastNonSingleMode: 'pinned',
      gridSearchQuery: 'runner',
      gridSortBy: 'name',
      gridSortDirection: 'asc',
    });

    render(<AgentNodeView />);

    // Pinned set {1 'Alpha Runner', 3 'Gamma Guard'} — still name-sorted.
    expect(renderedNodeIds()).toEqual([1, 3]);
  });

  it('applies the selected mesh scope before the other grid controls', () => {
    useAgentNodeStore.setState({
      // Issue #1384 — normalised state; NODES slice is reshaped to map+ids.
      nodesById: Object.fromEntries(
        [...NODES.slice(0, 3), { ...NODES[3], mesh_id: 2 }].map(n => [n.id, n])
      ),
      nodeIds: [...NODES.slice(0, 3), { ...NODES[3], mesh_id: 2 }].map(n => n.id),
    });
    useMeshStore.setState({ selectedMeshId: 1 });
    useUIStore.setState({ viewMode: 'mesh' });

    render(<AgentNodeView />);

    expect(renderedNodeIds()).toEqual([1, 2, 3]);
  });

  it('preserves the source sequence when custom ordering is active', () => {
    useAgentNodeStore.setState({
      nodesById: Object.fromEntries(
        [NODES[2], NODES[0], NODES[3], NODES[1]].map(n => [n.id, n])
      ),
      nodeIds: [NODES[2], NODES[0], NODES[3], NODES[1]].map(n => n.id),
    });

    render(<AgentNodeView />);

    expect(renderedNodeIds()).toEqual([3, 1, 4, 2]);
  });

  it('sorts the derived sequence by name with a deterministic position tie-breaker', () => {
    useUIStore.setState({ gridSortBy: 'name', gridSortDirection: 'asc' });

    render(<AgentNodeView />);

    expect(renderedNodeIds()).toEqual([4, 1, 2, 3]);
  });

  it('supports status and created sorting in both directions', () => {
    useUIStore.setState({ gridSortBy: 'status', gridSortDirection: 'asc' });
    const { rerender } = render(<AgentNodeView />);
    expect(renderedNodeIds()).toEqual([3, 2, 4, 1]);

    act(() => useUIStore.setState({ gridSortBy: 'created', gridSortDirection: 'desc' }));
    rerender(<AgentNodeView />);
    expect(renderedNodeIds()).toEqual([2, 3, 4, 1]);
  });

  it('renders the Filtered empty state when no node matches (#1609)', () => {
    useUIStore.setState({ viewMode: 'filtered', lastNonSingleMode: 'filtered', gridSearchQuery: 'no such node' });

    render(<AgentNodeView />);

    expect(screen.getByText('No matching nodes')).toBeTruthy();
  });

  it('renders the normal splash when the ALL grid is empty of nodes', () => {
    seedAgentNodes([], null);

    render(<AgentNodeView />);

    expect(screen.queryByTestId('grid-output')).toBeNull();
    expect(screen.getByText('Buildmesh')).toBeTruthy();
  });

  it('disables manual drag-and-drop while a non-custom sort is active', () => {
    useUIStore.setState({ gridSortBy: 'name' });

    render(<AgentNodeView />);

    expect(screen.getByTestId('grid-output').getAttribute('data-draggable')).toBe('false');
  });
});
