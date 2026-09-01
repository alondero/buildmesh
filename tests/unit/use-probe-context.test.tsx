import { describe, it, expect, beforeEach } from 'vitest';
import { act, renderHook } from '@testing-library/react';
import {
  PROBE_TAB_DEFINITIONS,
  useProbeContext,
} from '../../src/hooks/useProbeContext';
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

describe('useProbeContext (issue #1456)', () => {
  beforeEach(() => {
    useMeshStore.setState({
      meshes: [],
      meshesById: new Map(),
      selectedMeshId: null,
      loading: false,
      error: null,
    });
    useAgentNodeStore.setState({ nodesById: {}, nodeIds: [], activeNodeId: null,
      loading: false,
      error: null,
      closingNodeIds: new Set(),
    });
    useUIStore.setState({
      viewMode: 'mesh',
      lastNonSingleMode: 'mesh',
      probeTab: 'files',
      probeContextPins: {},
    });
  });

  it('returns an empty context when nothing is selected or focused', () => {
    const { result } = renderHook(() => useProbeContext());
    expect(result.current).toMatchObject({
      lens: 'mesh',
      subject: { lens: 'mesh', id: null, name: null, available: false },
      subjectLabel: 'Mesh',
      mode: 'following',
      followsSelection: true,
      pinnable: true,
      hasRequiredContext: false,
      canPin: false,
      pinCandidate: null,
      activeMeshId: null,
      activeNodeId: null,
      activePath: null,
      activeMeshPath: null,
      activeMeshName: null,
      activeNodeName: null,
      detailLabel: null,
    });
  });

  it('derives from selectedMeshId when a mesh is selected and no node is focused', () => {
    useMeshStore.setState({
      meshes: [makeMesh(1, '/a')],
      meshesById: new Map([[1, makeMesh(1, '/a')]]),
      selectedMeshId: 1,
    });
    const { result } = renderHook(() => useProbeContext());
    expect(result.current).toMatchObject({
      lens: 'mesh',
      subject: { lens: 'mesh', id: 1, name: 'mesh-1', available: true },
      subjectLabel: 'Mesh: mesh-1',
      mode: 'following',
      followsSelection: true,
      pinnable: true,
      hasRequiredContext: true,
      canPin: true,
      pinCandidate: { tab: 'files', lens: 'mesh', meshId: 1, nodeId: null },
      activeMeshId: 1,
      activeNodeId: null,
      activePath: '/a',
      activeMeshPath: '/a',
      activeMeshName: 'mesh-1',
      activeNodeName: null,
      detailLabel: 'Repository root',
    });
  });

  it('uses the focused node\'s path (worktree dir) when a node is active', () => {
    useMeshStore.setState({
      meshes: [makeMesh(1, '/a')],
      meshesById: new Map([[1, makeMesh(1, '/a')]]),
      selectedMeshId: 1,
    });
    seedAgentNodes([makeNode({ id: 7, mesh_id: 1 })], 7);
    const { result } = renderHook(() => useProbeContext());
    expect(result.current).toMatchObject({
      lens: 'mesh',
      subject: { lens: 'mesh', id: 1, name: 'mesh-1', available: true },
      subjectLabel: 'Mesh: mesh-1',
      mode: 'following',
      followsSelection: true,
      pinnable: true,
      hasRequiredContext: true,
      canPin: true,
      pinCandidate: { tab: 'files', lens: 'mesh', meshId: 1, nodeId: 7 },
      activeMeshId: 1,
      activeNodeId: 7,
      // activePath follows the focused node (worktree subdir), but
      // activeMeshPath stays anchored on the mesh root so the new
      // Mesh-lens tabs (issues, sessions) can walk the repo.
      // activeMeshName follows the selected mesh, independent of the
      // focused worktree (the dock header uses it to label the active
      // context).
      activePath: '/a/.claude/worktrees/bold-keen-brook',
      activeMeshPath: '/a',
      activeMeshName: 'mesh-1',
      activeNodeName: 'bold-keen-brook',
      detailLabel: 'Working tree: bold-keen-brook',
    });
  });

  it('keeps non-file Mesh destinations independent of focused Agent Nodes', () => {
    const mesh = makeMesh(1, '/a');
    useMeshStore.setState({
      meshes: [mesh],
      meshesById: new Map([[1, mesh]]),
      selectedMeshId: 1,
    });
    seedAgentNodes([makeNode({ id: 7, mesh_id: 1 })], 7);
    useUIStore.setState({ probeTab: 'properties' });

    const { result } = renderHook(() => useProbeContext());

    expect(result.current).toMatchObject({
      lens: 'mesh',
      subjectLabel: 'Mesh: mesh-1',
      activeMeshId: 1,
      activeNodeId: null,
      activePath: '/a',
      activeMeshPath: '/a',
      activeNodeName: null,
      detailLabel: null,
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
    seedAgentNodes([
        makeNode({ id: 11, mesh_id: 1, path: '/a', worktree_name: 'x' }),
        makeNode({ id: 22, mesh_id: 2, path: '/b', worktree_name: 'y' }),
      ], 22);
    const { result } = renderHook(() => useProbeContext());
    expect(result.current).toMatchObject({
      lens: 'mesh',
      subject: { lens: 'mesh', id: 2, name: 'mesh-2', available: true },
      subjectLabel: 'Mesh: mesh-2',
      mode: 'following',
      followsSelection: true,
      hasRequiredContext: true,
      activeMeshId: 2,
      activeNodeId: 22,
      activePath: '/b/.claude/worktrees/y',
      activeMeshPath: '/b',
      activeMeshName: 'mesh-2',
      activeNodeName: 'bold-keen-brook',
      detailLabel: 'Working tree: bold-keen-brook',
    });
  });

  it('uses the focused node mesh in Single mode when it differs from the sidebar mesh', () => {
    useMeshStore.setState({
      meshes: [makeMesh(1, '/a'), makeMesh(2, '/b')],
      meshesById: new Map([
        [1, makeMesh(1, '/a')],
        [2, makeMesh(2, '/b')],
      ]),
      selectedMeshId: 1,
    });
    seedAgentNodes([
        makeNode({ id: 11, mesh_id: 1, path: '/a', worktree_name: 'x' }),
        makeNode({ id: 22, mesh_id: 2, path: '/b', worktree_name: 'y' }),
      ], 11);
    useUIStore.setState({ viewMode: 'single' });

    const { result } = renderHook(() => useProbeContext());
    act(() => {
      useAgentNodeStore.getState().setActiveNode(22);
    });

    expect(result.current).toMatchObject({
      lens: 'mesh',
      subject: { lens: 'mesh', id: 2, name: 'mesh-2', available: true },
      subjectLabel: 'Mesh: mesh-2',
      mode: 'following',
      followsSelection: true,
      activeMeshId: 2,
      activeNodeId: 22,
      activePath: '/b/.claude/worktrees/y',
      activeMeshPath: '/b',
      activeMeshName: 'mesh-2',
      activeNodeName: 'bold-keen-brook',
      detailLabel: 'Working tree: bold-keen-brook',
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
    seedAgentNodes([
        makeNode({ id: 11, mesh_id: 1, path: '/a', worktree_name: 'x' }),
        makeNode({ id: 22, mesh_id: 2, path: '/b', worktree_name: 'y' }),
      ], 22);
    const { result } = renderHook(() => useProbeContext());
    // The selected mesh (1) wins, but the focused node (22) is still
    // The mesh lens owns the selected Mesh. A focused node in another Mesh
    // must not leak its path into a stateful mesh destination.
    expect(result.current).toMatchObject({
      lens: 'mesh',
      subject: { lens: 'mesh', id: 1, name: 'mesh-1', available: true },
      subjectLabel: 'Mesh: mesh-1',
      mode: 'following',
      followsSelection: true,
      activeMeshId: 1,
      activeNodeId: null,
      activePath: '/a',
      activeMeshPath: '/a',
      activeMeshName: 'mesh-1',
      activeNodeName: null,
      detailLabel: 'Repository root',
    });
  });

  it('returns null activePath when the selected mesh is missing from the map', () => {
    // Defensive: selectedMeshId set but meshes not yet fetched (or
    // stale). The hook should not throw; activePath degrades to null.
    useMeshStore.setState({ selectedMeshId: 99 });
    const { result } = renderHook(() => useProbeContext());
    expect(result.current.lens).toBe('mesh');
    expect(result.current.subjectLabel).toBe('Mesh: unavailable');
    expect(result.current.hasRequiredContext).toBe(false);
    expect(result.current.canPin).toBe(false);
    expect(result.current.activeMeshId).toBe(99);
    expect(result.current.activePath).toBeNull();
    expect(result.current.activeMeshPath).toBeNull();
    // activeMeshName also degrades to null — without a mesh row there's
    // no name to surface in the dock header.
    expect(result.current.activeMeshName).toBeNull();
  });

  it('falls back to mesh root for activePath when the focused node is missing from the list', () => {
    // activeNodeId points at a node that doesn't exist (e.g. the node
    // was just closed but activeNodeId hasn\'t been cleared yet). The
    // hook degrades to "no active node" and keeps the Mesh lens anchored to
    // the mesh root. A stale node id must not escape through the context API.
    useMeshStore.setState({
      meshes: [makeMesh(1, '/a')],
      meshesById: new Map([[1, makeMesh(1, '/a')]]),
      selectedMeshId: 1,
    });
    useAgentNodeStore.setState({ activeNodeId: 999 });
    const { result } = renderHook(() => useProbeContext());
    expect(result.current.activeMeshId).toBe(1);
    expect(result.current.activeNodeId).toBeNull();
    expect(result.current.activePath).toBe('/a');
    // The mesh is known, so the name is too — the dock header still has
    // a subheading to render even with a dangling activeNodeId.
    expect(result.current.activeMeshName).toBe('mesh-1');
    expect(result.current.subjectLabel).toBe('Mesh: mesh-1');
    expect(result.current.hasRequiredContext).toBe(true);
  });

  it('declares an ownership lens and baseline for every Probe destination', () => {
    expect(Object.keys(PROBE_TAB_DEFINITIONS).sort()).toEqual([
      'autopilot',
      'circuits',
      'files',
      'issues',
      'properties',
      'pulls',
      'review',
      'scratchpad',
      'sessions',
      'usage',
      'worktrees',
    ]);
    expect(PROBE_TAB_DEFINITIONS.usage).toMatchObject({
      lens: 'host',
      baseline: 'host',
      followsSelection: false,
      pinnable: false,
    });
    expect(PROBE_TAB_DEFINITIONS.review).toMatchObject({
      lens: 'agent',
      baseline: 'node-base',
    });
    expect(PROBE_TAB_DEFINITIONS.properties).toMatchObject({
      lens: 'mesh',
      stateful: true,
    });
  });

  it('follows a changed Mesh selection until the destination is pinned', () => {
    const mesh1 = makeMesh(1, '/a');
    const mesh2 = makeMesh(2, '/b');
    useMeshStore.setState({
      meshes: [mesh1, mesh2],
      meshesById: new Map([[1, mesh1], [2, mesh2]]),
      selectedMeshId: 1,
    });
    const { result } = renderHook(() => useProbeContext());

    expect(result.current.subjectLabel).toBe('Mesh: mesh-1');
    expect(result.current.mode).toBe('following');
    act(() => {
      useMeshStore.getState().selectMesh(2);
    });

    expect(result.current.subjectLabel).toBe('Mesh: mesh-2');
    expect(result.current.activeMeshId).toBe(2);
    expect(result.current.followsSelection).toBe(true);
  });

  it('keeps Usage Host-lens when a Mesh and Agent Node are selected', () => {
    const mesh = makeMesh(1, '/a');
    useMeshStore.setState({
      meshes: [mesh],
      meshesById: new Map([[1, mesh]]),
      selectedMeshId: 1,
    });
    seedAgentNodes([makeNode({ id: 7, mesh_id: 1 })], 7);
    useUIStore.setState({ probeTab: 'usage' });

    const { result } = renderHook(() => useProbeContext());

    expect(result.current).toMatchObject({
      lens: 'host',
      subject: { lens: 'host', id: null, name: null, available: true },
      subjectLabel: 'Host',
      mode: 'fixed',
      followsSelection: false,
      pinnable: false,
      hasRequiredContext: true,
      canPin: false,
      pinCandidate: null,
      activeMeshId: null,
      activeNodeId: null,
      activePath: null,
      activeMeshPath: null,
      activeMeshName: null,
    });
  });

  it('pins an Agent lens to its node and does not follow a later focus change', () => {
    const mesh = makeMesh(1, '/a');
    useMeshStore.setState({
      meshes: [mesh],
      meshesById: new Map([[1, mesh]]),
      selectedMeshId: 1,
    });
    seedAgentNodes([
      makeNode({ id: 7, mesh_id: 1, name: 'agent-7' }),
      makeNode({ id: 8, mesh_id: 1, name: 'agent-8', worktree_name: 'agent-8' }),
    ], 7);
    useUIStore.setState({ probeTab: 'review' });

    const { result } = renderHook(() => useProbeContext());
    expect(result.current).toMatchObject({
      lens: 'agent',
      subjectLabel: 'Agent: agent-7',
      detailLabel: 'Mesh: mesh-1',
      activeNodeId: 7,
      mode: 'following',
    });

    act(() => {
      useUIStore.getState().pinProbeContext(result.current.pinCandidate!);
      useAgentNodeStore.getState().setActiveNode(8);
    });

    expect(result.current).toMatchObject({
      lens: 'agent',
      subjectLabel: 'Agent: agent-7',
      activeNodeId: 7,
      activePath: '/a/.claude/worktrees/bold-keen-brook',
      mode: 'pinned',
      followsSelection: false,
    });
  });

  it('keeps a pinned Mesh destination on its subject after selection changes', () => {
    const mesh1 = makeMesh(1, '/a');
    const mesh2 = makeMesh(2, '/b');
    useMeshStore.setState({
      meshes: [mesh1, mesh2],
      meshesById: new Map([[1, mesh1], [2, mesh2]]),
      selectedMeshId: 1,
    });
    useUIStore.setState({ probeTab: 'properties' });

    const { result } = renderHook(() => useProbeContext());
    act(() => {
      useUIStore.getState().pinProbeContext(result.current.pinCandidate!);
      useMeshStore.getState().selectMesh(2);
    });

    expect(result.current).toMatchObject({
      subjectLabel: 'Mesh: mesh-1',
      activeMeshId: 1,
      activeMeshPath: '/a',
      mode: 'pinned',
      followsSelection: false,
    });
  });

  it('keeps pins isolated when multiple Probe destinations are captured', () => {
    const mesh1 = makeMesh(1, '/a');
    const mesh2 = makeMesh(2, '/b');
    useMeshStore.setState({
      meshes: [mesh1, mesh2],
      meshesById: new Map([[1, mesh1], [2, mesh2]]),
      selectedMeshId: 1,
    });
    useUIStore.setState({ probeTab: 'properties' });

    const { result } = renderHook(() => useProbeContext());
    act(() => {
      useUIStore.getState().pinProbeContext(result.current.pinCandidate!);
      useUIStore.getState().setProbeTab('issues');
    });
    act(() => {
      useUIStore.getState().pinProbeContext(result.current.pinCandidate!);
      useMeshStore.getState().selectMesh(2);
    });

    expect(result.current).toMatchObject({
      subjectLabel: 'Mesh: mesh-1',
      mode: 'pinned',
    });

    act(() => {
      useUIStore.getState().setProbeTab('properties');
    });
    expect(result.current).toMatchObject({
      subjectLabel: 'Mesh: mesh-1',
      mode: 'pinned',
    });
    expect(useUIStore.getState().probeContextPins).toEqual({
      properties: { tab: 'properties', lens: 'mesh', meshId: 1, nodeId: null },
      issues: { tab: 'issues', lens: 'mesh', meshId: 1, nodeId: null },
    });
  });

  it('does not fall back to the Mesh root when a pinned working tree disappears', () => {
    const mesh = makeMesh(1, '/a');
    useMeshStore.setState({
      meshes: [mesh],
      meshesById: new Map([[1, mesh]]),
      selectedMeshId: 1,
    });
    seedAgentNodes([makeNode({ id: 7, mesh_id: 1 })], 7);

    const { result } = renderHook(() => useProbeContext());
    act(() => {
      useUIStore.getState().pinProbeContext(result.current.pinCandidate!);
      seedAgentNodes([]);
    });

    expect(result.current).toMatchObject({
      lens: 'mesh',
      subjectLabel: 'Mesh: mesh-1',
      mode: 'pinned',
      hasRequiredContext: false,
      activeNodeId: null,
      activePath: null,
      detailLabel: 'Pinned working tree unavailable',
    });
  });

  it('shows a pinned context as unavailable instead of falling back', () => {
    const mesh1 = makeMesh(1, '/a');
    const mesh2 = makeMesh(2, '/b');
    useMeshStore.setState({
      meshes: [mesh1, mesh2],
      meshesById: new Map([[1, mesh1], [2, mesh2]]),
      selectedMeshId: 1,
    });

    const { result } = renderHook(() => useProbeContext());
    act(() => {
      useUIStore.getState().pinProbeContext(result.current.pinCandidate!);
      useMeshStore.setState({
        meshes: [mesh2],
        meshesById: new Map([[2, mesh2]]),
        selectedMeshId: 2,
      });
    });

    expect(result.current).toMatchObject({
      subject: { lens: 'mesh', id: 1, name: null, available: false },
      subjectLabel: 'Mesh: unavailable',
      mode: 'pinned',
      hasRequiredContext: false,
      activeMeshId: 1,
      activePath: null,
      activeMeshPath: null,
    });
  });
});
