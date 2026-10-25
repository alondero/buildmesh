import { describe, it, expect, beforeEach } from 'vitest';
import { act, renderHook } from '@testing-library/react';
import {
  diffLensDiverged,
  readScope,
  selectedActivityMember,
  useScope,
} from '../../src/hooks/useScope';
import { useMeshStore } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useUIStore } from '../../src/stores/uiStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 1,
    mesh_id: 1,
    name: 'bold-keen-brook',
    path: '/a',
    branch: 'main',
    env: 'windows',
    provider: 'anthropic',
    status: 'idle',
    created_at: '',
    use_worktree: true,
    worktree_name: 'bold-keen-brook',
    position: 0,
    is_pinned: false,
    source_issue: null,
    ...overrides,
  };
}

function makeMesh(id: number, path: string, overrides: Partial<{ name: string }> = {}) {
  return {
    id,
    name: overrides.name ?? `mesh-${id}`,
    path,
    layout: 'grid' as const,
    position: id - 1,
    created_at: '',
    build_command: null,
    run_command: null,
    model: null,
    effort: null,
    use_worktree: true,
    worktree_mode: null,
    default_provider: null,
    base_ref: 'origin/main',
    scratchpad: '',
    sandbox: false,
  };
}

describe('useScope (issue #2529)', () => {
  const alpha = makeNode({ id: 1, mesh_id: 1, name: 'alpha', position: 0 });
  const beta = makeNode({ id: 2, mesh_id: 1, name: 'beta', position: 1, is_pinned: true });
  const gamma = makeNode({ id: 3, mesh_id: 2, name: 'gamma', position: 0, provider: 'codex' });

  beforeEach(() => {
    useMeshStore.setState({
      meshes: [makeMesh(1, '/a', { name: 'Alpha' }), makeMesh(2, '/b', { name: 'Beta' })],
      meshesById: new Map([
        [1, makeMesh(1, '/a', { name: 'Alpha' })],
        [2, makeMesh(2, '/b', { name: 'Beta' })],
      ]),
      selectedMeshId: 1,
      loading: false,
      error: null,
    });
    seedAgentNodes([alpha, beta, gamma], null);
    useUIStore.setState({
      viewMode: 'mesh',
      lastNonSingleMode: 'mesh',
      gridSearchQuery: '',
      gridProviderFilter: null,
      gridStatusFilter: null,
    });
  });

  it('scopes a Mesh grid to the selected Mesh and leaves the current node empty', () => {
    const { result } = renderHook(() => useScope());

    expect(result.current.viewMode).toBe('mesh');
    expect(result.current.scopeMeshId).toBe(1);
    expect(result.current.isMeshScoped).toBe(true);
    expect(result.current.scopedNodes.map((node) => node.id)).toEqual([1, 2]);
    expect(result.current.currentNodeId).toBeNull();
    expect(result.current.currentNode).toBeNull();
    expect(result.current.selectedMeshId).toBe(1);
    expect(result.current.activeNodeId).toBeNull();
    // Names stay off unless a surface asks. The canvas must not re-render on a rename.
    expect(result.current.mesh?.name).toBeNull();
  });

  it('resolves Mesh names only when the caller asks for them', () => {
    const { result } = renderHook(() => useScope({ meshNames: true }));
    expect(result.current.mesh).toEqual({ id: 1, name: 'Alpha' });
  });

  it('reports an empty Mesh grid when nothing is selected', () => {
    useMeshStore.setState({ selectedMeshId: null });
    const { result } = renderHook(() => useScope());
    expect(result.current.scopeMeshId).toBeNull();
    expect(result.current.isMeshScoped).toBe(false);
    expect(result.current.scopedNodes).toEqual([]);
    expect(result.current.scopedNodeCount).toBe(0);
  });

  it('returns every node for All Nodes and only pinned nodes for Pinned', () => {
    useUIStore.setState({ viewMode: 'all', lastNonSingleMode: 'all' });
    const all = renderHook(() => useScope());
    expect(all.result.current.scopeMeshId).toBeNull();
    expect(all.result.current.scopedNodes.map((node) => node.id)).toEqual([1, 2, 3]);

    useUIStore.setState({ viewMode: 'pinned', lastNonSingleMode: 'pinned' });
    const pinned = renderHook(() => useScope());
    expect(pinned.result.current.scopedNodes.map((node) => node.id)).toEqual([2]);
    expect(pinned.result.current.scopeMeshId).toBeNull();
  });

  it('solos the active node in Single mode even when that node is outside the Mesh grid', () => {
    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'mesh' });
    seedAgentNodes([alpha, beta, gamma], gamma.id);
    const { result } = renderHook(() => useScope());
    expect(result.current.currentNodeId).toBe(gamma.id);
    expect(result.current.currentNode?.name).toBe('gamma');
    expect(result.current.scopeMeshId).toBe(1);
    expect(result.current.scopedNodes.map((node) => node.id)).toEqual([1, 2]);
  });

  it('falls back to the remembered grid, then to any node, when Single has no active node', () => {
    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'mesh' });
    const remembered = renderHook(() => useScope());
    expect(remembered.result.current.currentNodeId).toBe(alpha.id);

    useMeshStore.setState({ selectedMeshId: 99 });
    const anyNode = renderHook(() => useScope());
    expect(anyNode.result.current.currentNodeId).toBe(alpha.id);

    seedAgentNodes([], null);
    const empty = renderHook(() => useScope());
    expect(empty.result.current.currentNode).toBeNull();
  });

  it('narrows Filtered scope, and the Single fallback, with the grid controls', () => {
    useUIStore.setState({
      viewMode: 'filtered',
      lastNonSingleMode: 'filtered',
      gridSearchQuery: 'gam',
    });
    const filtered = renderHook(() => useScope());
    expect(filtered.result.current.scopedNodes.map((node) => node.id)).toEqual([3]);
    expect(filtered.result.current.visibleNodeCount).toBe(1);
    expect(filtered.result.current.scopedNodeCount).toBe(3);

    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'filtered' });
    const solo = renderHook(() => useScope());
    expect(solo.result.current.currentNodeId).toBe(gamma.id);
  });

  it('follows a store update without remounting', () => {
    const { result } = renderHook(() => useScope());
    expect(result.current.viewMode).toBe('mesh');
    act(() => {
      useUIStore.setState({ viewMode: 'all', lastNonSingleMode: 'all' });
    });
    expect(result.current.viewMode).toBe('all');
    expect(result.current.scopedNodes).toHaveLength(3);
  });

  it('reads the same scope from the store snapshot as the hook', () => {
    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'mesh' });
    seedAgentNodes([alpha, beta, gamma], beta.id);
    const { result } = renderHook(() => useScope({ meshNames: true }));
    const snapshot = readScope({ meshNames: true });
    expect(snapshot.viewMode).toBe(result.current.viewMode);
    expect(snapshot.scopeMeshId).toBe(result.current.scopeMeshId);
    expect(snapshot.currentNodeId).toBe(result.current.currentNodeId);
    expect(snapshot.scopedNodes.map((node) => node.id)).toEqual(
      result.current.scopedNodes.map((node) => node.id),
    );
    expect(snapshot.mesh).toEqual(result.current.mesh);
  });
});

describe('diffLensDiverged', () => {
  const lens = { activeNodeId: 4 as number | null, selectedMeshId: 7 as number | null };

  it('closes when the focused node or the selected Mesh leaves the captured lens', () => {
    expect(diffLensDiverged(lens, { nodeId: 4, meshId: 7 })).toBe(false);
    expect(diffLensDiverged(lens, { nodeId: 5, meshId: 7 })).toBe(true);
    expect(diffLensDiverged(lens, { nodeId: 4, meshId: 8 })).toBe(true);
  });

  it('ignores a mesh-only diff and a cleared Mesh selection', () => {
    expect(diffLensDiverged(lens, { nodeId: null, meshId: 7 })).toBe(false);
    expect(diffLensDiverged(
      { activeNodeId: 9, selectedMeshId: null },
      { nodeId: null, meshId: 7 },
    )).toBe(false);
    expect(diffLensDiverged(
      { activeNodeId: 9, selectedMeshId: null },
      { nodeId: 4, meshId: 7 },
    )).toBe(true);
  });
});

describe('selectedActivityMember', () => {
  const members = [{ id: 10 }, { id: 11 }];

  it('prefers the active member, then the card selection, then the card root', () => {
    expect(selectedActivityMember(members, 11, 10, 10)).toBe(11);
    expect(selectedActivityMember(members, 99, 11, 10)).toBe(11);
    expect(selectedActivityMember(members, null, 11, 10)).toBe(11);
    expect(selectedActivityMember(members, 99, 99, 10)).toBe(10);
    expect(selectedActivityMember(members, null, undefined, 10)).toBe(10);
  });
});
