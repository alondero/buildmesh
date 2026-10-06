import { describe, it, expect, beforeEach } from 'vitest';
import { act, renderHook } from '@testing-library/react';
import {
  PROBE_TAB_DEFINITIONS,
  useProbeContext,
  type ProbeTab,
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
      hasRequiredContext: false,
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
      hasRequiredContext: true,
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
      hasRequiredContext: true,
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

  it('follows a changed Mesh selection', () => {
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
      hasRequiredContext: true,
      activeMeshId: null,
      activeNodeId: null,
      activePath: null,
      activeMeshPath: null,
      activeMeshName: null,
    });
  });

// Issue #2073 — the anti-drift contract for the removal of Probe Context
  // Pins: there is no capture, so a Mesh/Agent destination always reads the
  // live selection, a Host destination is fixed, and losing the subject
  // yields the explicit unavailable state instead of another subject.
  it('follows the focused Agent Node: Agent Changes re-targets on every focus change', () => {
    const mesh = makeMesh(1, '/a');
    useMeshStore.setState({
      meshes: [mesh],
      meshesById: new Map([[1, mesh]]),
      selectedMeshId: 1,
    });
    seedAgentNodes([
      makeNode({ id: 7, mesh_id: 1, name: 'agent-7', worktree_name: 'wt-7' }),
      makeNode({ id: 8, mesh_id: 1, name: 'agent-8', worktree_name: 'wt-8' }),
    ], 7);
    useUIStore.setState({ probeTab: 'review' });

    const { result } = renderHook(() => useProbeContext());
    expect(result.current).toMatchObject({
      lens: 'agent',
      subject: { lens: 'agent', id: 7, name: 'agent-7', available: true },
      subjectLabel: 'Agent: agent-7',
      detailLabel: 'Mesh: mesh-1',
      activeMeshId: 1,
      activeNodeId: 7,
      activePath: '/a/.claude/worktrees/wt-7',
      mode: 'following',
      followsSelection: true,
      hasRequiredContext: true,
    });

    act(() => {
      useAgentNodeStore.getState().setActiveNode(8);
    });

    expect(result.current).toMatchObject({
      lens: 'agent',
      subject: { lens: 'agent', id: 8, name: 'agent-8', available: true },
      subjectLabel: 'Agent: agent-8',
      activeNodeId: 8,
      activePath: '/a/.claude/worktrees/wt-8',
      mode: 'following',
      followsSelection: true,
      hasRequiredContext: true,
    });
  });

  it('follows the focused Agent Node: Project Files re-targets its working directory', () => {
    const mesh = makeMesh(1, '/a');
    useMeshStore.setState({
      meshes: [mesh],
      meshesById: new Map([[1, mesh]]),
      selectedMeshId: 1,
    });
    seedAgentNodes([
      makeNode({ id: 7, mesh_id: 1, name: 'agent-7', worktree_name: 'wt-7' }),
      makeNode({ id: 8, mesh_id: 1, name: 'agent-8', worktree_name: 'wt-8' }),
    ], 7);

    const { result } = renderHook(() => useProbeContext());
    expect(result.current).toMatchObject({
      lens: 'mesh',
      subjectLabel: 'Mesh: mesh-1',
      detailLabel: 'Working tree: agent-7',
      activeNodeId: 7,
      activePath: '/a/.claude/worktrees/wt-7',
      activeMeshPath: '/a',
      mode: 'following',
    });

    act(() => {
      useAgentNodeStore.getState().setActiveNode(8);
    });

    // The Mesh lens still owns the Mesh (its root stays available for repo
    // walking); only the mixed working-tree presentation re-targets.
    expect(result.current).toMatchObject({
      lens: 'mesh',
      subject: { lens: 'mesh', id: 1, name: 'mesh-1', available: true },
      detailLabel: 'Working tree: agent-8',
      activeMeshId: 1,
      activeNodeId: 8,
      activePath: '/a/.claude/worktrees/wt-8',
      activeMeshPath: '/a',
      mode: 'following',
    });
  });

  it('resolves Project Files mixed ownership from the selection alone', () => {
    const mesh = makeMesh(1, '/a');
    useMeshStore.setState({
      meshes: [mesh],
      meshesById: new Map([[1, mesh]]),
      selectedMeshId: 1,
    });
    seedAgentNodes([
      makeNode({ id: 7, mesh_id: 1, name: 'agent-7', worktree_name: 'wt-7' }),
    ], null);

    const { result } = renderHook(() => useProbeContext());

    // Mesh selected, no Agent Node focused → the Mesh-owned repository root.
    expect(result.current).toMatchObject({
      lens: 'mesh',
      subject: { lens: 'mesh', id: 1, name: 'mesh-1', available: true },
      activeNodeId: null,
      activeNodeName: null,
      activePath: '/a',
      activeMeshPath: '/a',
      detailLabel: 'Repository root',
      mode: 'following',
      hasRequiredContext: true,
    });

    act(() => {
      useAgentNodeStore.getState().setActiveNode(7);
    });

    expect(result.current).toMatchObject({
      activeNodeId: 7,
      activeNodeName: 'agent-7',
      activePath: '/a/.claude/worktrees/wt-7',
      activeMeshPath: '/a',
      detailLabel: 'Working tree: agent-7',
      mode: 'following',
      hasRequiredContext: true,
    });

    // Losing the focused Agent Node is not losing the destination's subject:
    // the Mesh lens keeps the repository root rather than going unavailable.
    act(() => {
      useAgentNodeStore.getState().setActiveNode(null);
    });

    expect(result.current).toMatchObject({
      activeNodeId: null,
      activePath: '/a',
      detailLabel: 'Repository root',
      hasRequiredContext: true,
    });
  });

  it('keeps every Host destination fixed and never resolves a Mesh', () => {
    const mesh = makeMesh(1, '/a');
    useMeshStore.setState({
      meshes: [mesh],
      meshesById: new Map([[1, mesh]]),
      selectedMeshId: 1,
    });
    seedAgentNodes([makeNode({ id: 7, mesh_id: 1 })], 7);

    const hostTabs = (Object.keys(PROBE_TAB_DEFINITIONS) as ProbeTab[])
      .filter((tab) => PROBE_TAB_DEFINITIONS[tab].lens === 'host');
    expect(hostTabs).toEqual(['usage', 'sessions']);

    for (const tab of hostTabs) {
      useUIStore.setState({ probeTab: tab });
      const { result, unmount } = renderHook(() => useProbeContext());
      expect(result.current).toMatchObject({
        lens: 'host',
        subject: { lens: 'host', id: null, name: null, available: true },
        subjectLabel: 'Host',
        mode: 'fixed',
        followsSelection: false,
        hasRequiredContext: true,
        activeMeshId: null,
        activeNodeId: null,
        activePath: null,
        activeMeshPath: null,
        activeMeshName: null,
        activeNodeName: null,
        detailLabel: null,
      });
      unmount();
    }
  });

  it('reports a lost Agent Node as unavailable instead of resolving another subject', () => {
    const mesh = makeMesh(1, '/a');
    useMeshStore.setState({
      meshes: [mesh],
      meshesById: new Map([[1, mesh]]),
      selectedMeshId: 1,
    });
    // Two nodes stay loaded throughout: a resolver that "helpedfully" fell
    // back would surface agent-8 here, and the assertion below would catch it.
    seedAgentNodes([
      makeNode({ id: 7, mesh_id: 1, name: 'agent-7', worktree_name: 'wt-7' }),
      makeNode({ id: 8, mesh_id: 1, name: 'agent-8', worktree_name: 'wt-8' }),
    ], 7);
    useUIStore.setState({ probeTab: 'review' });

    const { result } = renderHook(() => useProbeContext());
    expect(result.current.subjectLabel).toBe('Agent: agent-7');

    act(() => {
      useAgentNodeStore.getState().setActiveNode(null);
    });

    expect(result.current).toMatchObject({
      lens: 'agent',
      subject: { lens: 'agent', id: null, name: null, available: false },
      subjectLabel: 'Agent',
      activeNodeId: null,
      activeNodeName: null,
      activePath: null,
      mode: 'following',
      hasRequiredContext: false,
    });

    // The recovery action is selection: focusing a node makes it the subject.
    act(() => {
      useAgentNodeStore.getState().setActiveNode(8);
    });

    expect(result.current).toMatchObject({
      subjectLabel: 'Agent: agent-8',
      activeNodeId: 8,
      hasRequiredContext: true,
    });
  });

  it('reports a dangling focused Agent Node as unavailable rather than guessing', () => {
    // `activeNodeId` still points at a node that is gone (closed without the
    // store clearing the id). The Mesh lens degrades to the repository root;
    // the Agent lens has no such fallback and must report unavailable.
    const mesh = makeMesh(1, '/a');
    useMeshStore.setState({
      meshes: [mesh],
      meshesById: new Map([[1, mesh]]),
      selectedMeshId: 1,
    });
    seedAgentNodes([makeNode({ id: 7, mesh_id: 1, name: 'agent-7' })], 7);
    useUIStore.setState({ probeTab: 'review' });

    const { result, rerender } = renderHook(() => useProbeContext());
    act(() => {
      useAgentNodeStore.setState({ nodesById: {}, nodeIds: [], activeNodeId: 7 });
      rerender();
    });

    expect(result.current).toMatchObject({
      lens: 'agent',
      subject: { lens: 'agent', id: 7, name: null, available: false },
      subjectLabel: 'Agent: unavailable',
      activeNodeId: 7,
      activePath: null,
      hasRequiredContext: false,
    });
  });
});
