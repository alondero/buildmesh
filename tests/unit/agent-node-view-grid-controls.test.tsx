/**
 * Grid Controls (wayfinder #988 / #996) — the AgentNodeView must apply the
 * uiStore controls to the node sequence it hands to the grid, and manual
 * drag-and-drop must disappear while a non-custom sort is active.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { act, render, screen } from '@testing-library/react';
import type { AgentNode } from '../../src/stores/agentNodeStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';

vi.mock('../../src/components/AgentNodeView/NodeCard', () => ({
  NodeCard: ({ node, draggable = true }: { node: AgentNode; draggable?: boolean }) => (
    <div data-testid="grid-node" data-node-id={node.id} data-draggable={draggable}>
      {node.name}
    </div>
  ),
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
  useAgentNodeStore.setState({ agentNodes: NODES, activeNodeId: NODES[0].id });
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
  it('filters by the search query independently', () => {
    useUIStore.setState({ gridSearchQuery: 'runner' });

    render(<AgentNodeView />);

    expect(renderedNodeIds()).toEqual([1]);
  });

  it('filters by provider independently', () => {
    useUIStore.setState({ gridProviderFilter: 'anthropic' });

    render(<AgentNodeView />);

    expect(renderedNodeIds()).toEqual([1, 3, 4]);
  });

  it('filters by status independently', () => {
    useUIStore.setState({ gridStatusFilter: 'idle' });

    render(<AgentNodeView />);

    expect(renderedNodeIds()).toEqual([2, 4]);
  });

  it('composes search, provider, and status filters before rendering the grid', () => {
    useUIStore.setState({
      gridSearchQuery: 'a',
      gridProviderFilter: 'anthropic',
      gridStatusFilter: 'error',
    });

    render(<AgentNodeView />);

    expect(renderedNodeIds()).toEqual([3]);
  });

  it('applies the selected mesh scope before the other grid controls', () => {
    useAgentNodeStore.setState({
      agentNodes: [...NODES.slice(0, 3), { ...NODES[3], mesh_id: 2 }],
    });
    useMeshStore.setState({ selectedMeshId: 1 });
    useUIStore.setState({ viewMode: 'mesh' });

    render(<AgentNodeView />);

    expect(renderedNodeIds()).toEqual([1, 2, 3]);
  });

  it('preserves the source sequence when custom ordering is active', () => {
    useAgentNodeStore.setState({
      agentNodes: [NODES[2], NODES[0], NODES[3], NODES[1]],
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

  it('renders the normal empty state when filters remove every node', () => {
    useUIStore.setState({ gridSearchQuery: 'no such node' });

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
