import { describe, it, expect, beforeEach } from 'vitest';
import { renderHook } from '@testing-library/react';
import { useProbeContext } from '../../src/hooks/useProbeContext';
import { useMeshStore } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 1,
    mesh_id: 1,
    name: 'bold-keen-brook',
    path: '/a/.claude/worktrees/bold-keen-brook',
    branch: 'main',
    env: 'windows',
    provider: 'anthropic',
    status: 'idle',
    created_at: '',
    use_worktree: true,
    position: 0,
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
  };
}

describe('useProbeContext (issue #373)', () => {
  beforeEach(() => {
    useMeshStore.setState({
      meshes: [],
      meshesById: new Map(),
      selectedMeshId: null,
      loading: false,
      error: null,
    });
    useAgentNodeStore.setState({
      agentNodes: [],
      activeNodeId: null,
      loading: false,
      error: null,
      closingNodeIds: new Set(),
    });
  });

  it('returns an empty context when nothing is selected or focused', () => {
    const { result } = renderHook(() => useProbeContext());
    expect(result.current).toEqual({
      activeMeshId: null,
      activeNodeId: null,
      activePath: null,
    });
  });

  it('derives from selectedMeshId when a mesh is selected and no node is focused', () => {
    useMeshStore.setState({
      meshes: [makeMesh(1, '/a')],
      meshesById: new Map([[1, makeMesh(1, '/a')]]),
      selectedMeshId: 1,
    });
    const { result } = renderHook(() => useProbeContext());
    expect(result.current).toEqual({
      activeMeshId: 1,
      activeNodeId: null,
      activePath: '/a',
    });
  });

  it('uses the focused node\'s path (worktree dir) when a node is active', () => {
    useMeshStore.setState({
      meshes: [makeMesh(1, '/a')],
      meshesById: new Map([[1, makeMesh(1, '/a')]]),
      selectedMeshId: 1,
    });
    useAgentNodeStore.setState({
      agentNodes: [makeNode({ id: 7, mesh_id: 1, path: '/a/.claude/worktrees/bold-keen-brook' })],
      activeNodeId: 7,
    });
    const { result } = renderHook(() => useProbeContext());
    expect(result.current).toEqual({
      activeMeshId: 1,
      activeNodeId: 7,
      activePath: '/a/.claude/worktrees/bold-keen-brook',
    });
  });

  it('auto-derives mesh from focused node in the global view (selectedMeshId === null)', () => {
    // Sidebar shows all meshes, no specific mesh is "selected", but the user
    // just clicked an agent card. The probe must still resolve to that
    // card's mesh + worktree path.
    useMeshStore.setState({
      meshes: [makeMesh(1, '/a'), makeMesh(2, '/b')],
      meshesById: new Map([
        [1, makeMesh(1, '/a')],
        [2, makeMesh(2, '/b')],
      ]),
      selectedMeshId: null,
    });
    useAgentNodeStore.setState({
      agentNodes: [
        makeNode({ id: 11, mesh_id: 1, path: '/a/.claude/worktrees/x' }),
        makeNode({ id: 22, mesh_id: 2, path: '/b/.claude/worktrees/y' }),
      ],
      activeNodeId: 22,
    });
    const { result } = renderHook(() => useProbeContext());
    expect(result.current).toEqual({
      activeMeshId: 2,
      activeNodeId: 22,
      activePath: '/b/.claude/worktrees/y',
    });
  });

  it('prefers an explicit selectedMeshId over the focused node\'s mesh', () => {
    // If the user has a mesh selected and ALSO has a node focused in a
    // different mesh (edge case: stale focus after a sidebar switch), the
    // sidebar selection wins — that\'s the source of truth for "where am I".
    useMeshStore.setState({
      meshes: [makeMesh(1, '/a'), makeMesh(2, '/b')],
      meshesById: new Map([
        [1, makeMesh(1, '/a')],
        [2, makeMesh(2, '/b')],
      ]),
      selectedMeshId: 1,
    });
    useAgentNodeStore.setState({
      agentNodes: [
        makeNode({ id: 11, mesh_id: 1, path: '/a/.claude/worktrees/x' }),
        makeNode({ id: 22, mesh_id: 2, path: '/b/.claude/worktrees/y' }),
      ],
      activeNodeId: 22,
    });
    const { result } = renderHook(() => useProbeContext());
    // The selected mesh (1) wins, but the focused node (22) is still
    // surfaced as activeNodeId; activePath follows the focused node
    // (the user is looking at that card, not at the mesh).
    expect(result.current).toEqual({
      activeMeshId: 1,
      activeNodeId: 22,
      activePath: '/b/.claude/worktrees/y',
    });
  });

  it('returns null activePath when the selected mesh is missing from the map', () => {
    // Defensive: selectedMeshId set but meshes not yet fetched (or
    // stale). The hook should not throw; activePath degrades to null.
    useMeshStore.setState({ selectedMeshId: 99 });
    const { result } = renderHook(() => useProbeContext());
    expect(result.current.activeMeshId).toBe(99);
    expect(result.current.activePath).toBeNull();
  });

  it('falls back to mesh root for activePath when the focused node is missing from the list', () => {
    // activeNodeId points at a node that doesn't exist (e.g. the node
    // was just closed but activeNodeId hasn\'t been cleared yet). The
    // hook degrades to "no active node" and falls back to the mesh
    // root path if a mesh is known — NOT to null, because the mesh
    // IS known and the probe should still have a directory to anchor.
    useMeshStore.setState({
      meshes: [makeMesh(1, '/a')],
      meshesById: new Map([[1, makeMesh(1, '/a')]]),
      selectedMeshId: 1,
    });
    useAgentNodeStore.setState({ activeNodeId: 999 });
    const { result } = renderHook(() => useProbeContext());
    expect(result.current.activeMeshId).toBe(1);
    expect(result.current.activeNodeId).toBe(999);
    expect(result.current.activePath).toBe('/a');
  });
});
