import { describe, it, expect, beforeEach, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useMeshStore } from '../../src/stores/meshStore';
import type { AgentNode } from '../../src/types/generated/AgentNode';

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

// Issue #1247 — `deleteMesh` reaches into `useAgentNodeStore` to capture
// doomed node ids, refetch after the IPC commits, and null `activeNodeId`
// when it pointed into the deleted mesh. Mock the store module with a
// hoisted shared-state object so each test can pre-load `nodesById` /
// `nodeIds` / `activeNodeId` and observe the post-call side effects
// (`fetchAgentNodes` invocation, `setActiveNode` invocation) without
// driving the real store's IPC plumbing.
//
// The methods live on `state` (matching the real Zustand `create()`
// shape — `useAgentNodeStore.getState()` returns the full state object
// INCLUDING actions). Putting them on the store object instead of on
// state produces `getState().fetchAgentNodes is not a function`.
const agentStoreMock = vi.hoisted(() => {
  const fetchAgentNodes = vi.fn().mockResolvedValue(undefined);
  const setActiveNode = vi.fn();
  const state: {
    // Issue #1384 — normalized state (`nodesById` + `nodeIds`) replaces
    // the previous flat `agentNodes` array. The mesh-store test only
    // cares about the doomed-id list, so we keep a derived array in
    // sync via a setter helper.
    nodesById: Record<number, AgentNode>;
    nodeIds: number[];
    activeNodeId: number | null;
    fetchAgentNodes: typeof fetchAgentNodes;
    setActiveNode: typeof setActiveNode;
  } = {
    nodesById: {},
    nodeIds: [],
    activeNodeId: null,
    fetchAgentNodes,
    setActiveNode,
  };
  // Mirror the production `setActiveNode`: just `set({ activeNodeId })`.
  // Defined here (not at construction) so `state` is in scope for the
  // closure.
  setActiveNode.mockImplementation((id: number | null) => {
    state.activeNodeId = id;
  });
  return {
    state,
    useAgentNodeStore: {
      getState: vi.fn(() => state),
    },
  };
});

vi.mock('../../src/stores/agentNodeStore', () => ({
  useAgentNodeStore: agentStoreMock.useAgentNodeStore,
}));

// Issue #1247 — `deleteMesh` calls `disposeTerminal` once per doomed
// node id after the backend delete commits. Mock the React-component
// surface that exports `disposeTerminal` so the test can observe the
// dispose fan-out without spinning up an xterm (Terminal.tsx loads
// @xterm/xterm + the WebGL addon pool, which would fail jsdom).
const disposeTerminalMock = vi.hoisted(() => vi.fn());
vi.mock('../../src/components/Terminal/Terminal', () => ({
  disposeTerminal: disposeTerminalMock,
  AgentTerminal: () => null,
}));

import { disposeTerminal } from '../../src/components/Terminal/Terminal';
const mockDisposeTerminal = disposeTerminal as ReturnType<typeof vi.fn>;

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 1,
    mesh_id: 1,
    name: 'node',
    path: '/p',
    branch: 'main',
    env: 'windows',
    provider: 'claude',
    status: 'idle',
    use_worktree: true,
    position: 0,
    created_at: '',
    ...overrides,
  };
}

// Issue #1384 — populate the mocked `nodesById` + `nodeIds` from an
// array, mirroring the normalized store shape. The mesh-store test only
// uses the doomed-id walk, so we don't care about canonical ordering.
function seedAgentNodes(nodes: AgentNode[]) {
  const nodesById: Record<number, AgentNode> = {};
  const nodeIds: number[] = [];
  for (const n of nodes) {
    nodesById[n.id] = n;
    nodeIds.push(n.id);
  }
  agentStoreMock.state.nodesById = nodesById;
  agentStoreMock.state.nodeIds = nodeIds;
}

describe('useMeshStore', () => {
  beforeEach(() => {
    useMeshStore.setState({
      meshes: [],
      meshesById: new Map(),
      selectedMeshId: null,
      loading: false,
      error: null,
    });
    // Reset the agent-store mock state + call history so each test starts
    // from a clean slate. `vi.clearAllMocks` only clears call history;
    // it does NOT reset the state object the hoisted closures capture.
    agentStoreMock.state.nodesById = {};
    agentStoreMock.state.nodeIds = [];
    agentStoreMock.state.activeNodeId = null;
    vi.clearAllMocks();
  });

  describe('fetchMeshes', () => {
    it('populates meshes and meshesById on success', async () => {
      const meshes = [
        { id: 1, name: 'project-a', path: '/a', layout: 'grid', position: 0, created_at: '2026-01-01' },
        { id: 2, name: 'project-b', path: '/b', layout: 'grid', position: 1, created_at: '2026-01-02' },
      ];
      mockInvoke.mockResolvedValueOnce(meshes);

      await useMeshStore.getState().fetchMeshes();

      const state = useMeshStore.getState();
      expect(state.meshes).toEqual(meshes);
      expect(state.meshesById.get(1)?.name).toBe('project-a');
      expect(state.meshesById.get(2)?.name).toBe('project-b');
      expect(state.loading).toBe(false);
      expect(state.error).toBeNull();
    });

    it('sets error on failure', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('DB connection failed'));

      await useMeshStore.getState().fetchMeshes();

      const state = useMeshStore.getState();
      expect(state.error).toContain('DB connection failed');
      expect(state.loading).toBe(false);
      expect(state.meshes).toEqual([]);
    });

    it('sets loading true during fetch', async () => {
      let resolvePromise: (value: unknown) => void;
      mockInvoke.mockReturnValueOnce(new Promise(r => { resolvePromise = r; }));

      const promise = useMeshStore.getState().fetchMeshes();
      expect(useMeshStore.getState().loading).toBe(true);

      resolvePromise!([]);
      await promise;
      expect(useMeshStore.getState().loading).toBe(false);
    });
  });

  describe('selectMesh', () => {
    it('sets selectedMeshId', () => {
      useMeshStore.getState().selectMesh(42);
      expect(useMeshStore.getState().selectedMeshId).toBe(42);
    });

    it('clears selection with null', () => {
      useMeshStore.getState().selectMesh(42);
      useMeshStore.getState().selectMesh(null);
      expect(useMeshStore.getState().selectedMeshId).toBeNull();
    });
  });

  describe('deleteMesh', () => {
    // Issue #1247 — `deleteMesh` calls `fetchMeshes` (which invokes
    // `list_meshes`) and then `useAgentNodeStore.getState().fetchAgentNodes`
    // (which is mocked at the module seam here, so it makes NO invokes).
    // On the happy path the production code makes exactly two invokes:
    // `delete_mesh` and `list_meshes`. Queueing extras would leave one-time
    // mocks in the queue that the NEXT test consumes — `vi.clearAllMocks`
    // clears call history but does NOT clear one-time mock queues, so the
    // wrong return value lands on the next test's first invoke. Pin the
    // queue size to the production invoke count.

    it('calls delete_mesh and refetches, returns true on success', async () => {
      mockInvoke
        .mockResolvedValueOnce(undefined) // delete_mesh
        .mockResolvedValueOnce([]);       // list_meshes (refetch)

      const ok = await useMeshStore.getState().deleteMesh(5);

      expect(ok).toBe(true);
      expect(mockInvoke).toHaveBeenCalledWith('delete_mesh', { meshId: 5 });
    });

    it('sets error and returns false if deletion fails', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('Not found'));

      const ok = await useMeshStore.getState().deleteMesh(99);

      expect(ok).toBe(false);
      expect(useMeshStore.getState().error).toContain('Not found');
    });

    // ---- Issue #1247 ghost-cleanup invariants ----
    // Pre-#1247 `deleteMesh` only refetched meshes, which left:
    //   * ghost `agentNodes` rows still clickable in the grid + sidebar,
    //   * xterm instances + 'agent-output' listeners alive in the
    //     TerminalRegistry,
    //   * `selectedMeshId` / `activeNodeId` dangling into the deleted mesh.
    // These tests pin the three cleanup steps the fix introduces.

    it('refetches agent nodes after successful backend delete', async () => {
      mockInvoke
        .mockResolvedValueOnce(undefined)
        .mockResolvedValueOnce([]);

      await useMeshStore.getState().deleteMesh(1);

      expect(agentStoreMock.state.fetchAgentNodes).toHaveBeenCalledTimes(1);
    });

    it('does NOT refetch agent nodes when backend delete fails', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('boom'));

      await useMeshStore.getState().deleteMesh(1);

      expect(agentStoreMock.state.fetchAgentNodes).not.toHaveBeenCalled();
      expect(mockDisposeTerminal).not.toHaveBeenCalled();
    });

    it('disposes each doomed node terminal after successful delete', async () => {
      // Two doomed nodes (mesh 1) and one survivor (mesh 2). The fix
      // must only dispose the doomed ids — the survivor's terminal
      // stays alive because the node IS still alive.
      seedAgentNodes([
        makeNode({ id: 10, mesh_id: 1 }),
        makeNode({ id: 11, mesh_id: 1 }),
        makeNode({ id: 12, mesh_id: 2 }),
      ]);
      mockInvoke
        .mockResolvedValueOnce(undefined)
        .mockResolvedValueOnce([]);

      await useMeshStore.getState().deleteMesh(1);

      expect(mockDisposeTerminal).toHaveBeenCalledTimes(2);
      expect(mockDisposeTerminal).toHaveBeenCalledWith(10);
      expect(mockDisposeTerminal).toHaveBeenCalledWith(11);
      expect(mockDisposeTerminal).not.toHaveBeenCalledWith(12);
    });

    it('does not dispose any terminals when the doomed mesh had no nodes', async () => {
      // No nodes belong to mesh 7 — the doomed-id list is empty, so
      // the dispose loop is a no-op (matches the registry's "missing
      // id → silent no-op" discipline, TerminalRegistry.dispose).
      seedAgentNodes([makeNode({ id: 99, mesh_id: 2 })]);
      mockInvoke
        .mockResolvedValueOnce(undefined)
        .mockResolvedValueOnce([]);

      await useMeshStore.getState().deleteMesh(7);

      expect(mockDisposeTerminal).not.toHaveBeenCalled();
    });

    it('nulls selectedMeshId when it pointed at the deleted mesh', async () => {
      useMeshStore.setState({ selectedMeshId: 1 });
      mockInvoke
        .mockResolvedValueOnce(undefined)
        .mockResolvedValueOnce([]);

      await useMeshStore.getState().deleteMesh(1);

      expect(useMeshStore.getState().selectedMeshId).toBeNull();
    });

    it('leaves selectedMeshId alone when it pointed at a different mesh', async () => {
      useMeshStore.setState({ selectedMeshId: 2 });
      mockInvoke
        .mockResolvedValueOnce(undefined)
        .mockResolvedValueOnce([]);

      await useMeshStore.getState().deleteMesh(1);

      expect(useMeshStore.getState().selectedMeshId).toBe(2);
    });

    it('nulls activeNodeId when it pointed at a doomed node', async () => {
      agentStoreMock.state.activeNodeId = 10;
      seedAgentNodes([makeNode({ id: 10, mesh_id: 1 })]);
      mockInvoke
        .mockResolvedValueOnce(undefined)
        .mockResolvedValueOnce([]);

      await useMeshStore.getState().deleteMesh(1);

      expect(agentStoreMock.state.activeNodeId).toBeNull();
      expect(agentStoreMock.state.setActiveNode).toHaveBeenCalledWith(null);
    });

    it('leaves activeNodeId alone when it pointed at a node in another mesh', async () => {
      // activeNodeId points at mesh 2's node 99. Mesh 1 is being deleted
      // (which would take nodes 10 + 11). Node 99 survives — the fix must
      // NOT clobber a still-active node id.
      agentStoreMock.state.activeNodeId = 99;
      seedAgentNodes([
        makeNode({ id: 10, mesh_id: 1 }),
        makeNode({ id: 11, mesh_id: 1 }),
        makeNode({ id: 99, mesh_id: 2 }),
      ]);
      mockInvoke
        .mockResolvedValueOnce(undefined)
        .mockResolvedValueOnce([]);

      await useMeshStore.getState().deleteMesh(1);

      expect(agentStoreMock.state.activeNodeId).toBe(99);
      expect(agentStoreMock.state.setActiveNode).not.toHaveBeenCalled();
    });

    it('does not dispose terminals or null selection when backend delete fails', async () => {
      // End-to-end "everything stays put on failure" — guards against a
      // regression where the post-IPC cleanup leaks past the try/catch
      // and quietly disposes a mesh that the backend refused to delete.
      agentStoreMock.state.activeNodeId = 10;
      seedAgentNodes([makeNode({ id: 10, mesh_id: 1 })]);
      useMeshStore.setState({ selectedMeshId: 1 });
      mockInvoke.mockRejectedValueOnce(new Error('backend says no'));

      const ok = await useMeshStore.getState().deleteMesh(1);

      expect(ok).toBe(false);
      expect(mockDisposeTerminal).not.toHaveBeenCalled();
      expect(agentStoreMock.state.activeNodeId).toBe(10);
      expect(useMeshStore.getState().selectedMeshId).toBe(1);
    });
  });

  describe('reorderMeshes', () => {
    it('reorders optimistically then persists', async () => {
      const meshes = [
        { id: 1, name: 'a', path: '/a', layout: 'grid' as const, position: 0, created_at: '' },
        { id: 2, name: 'b', path: '/b', layout: 'grid' as const, position: 1, created_at: '' },
        { id: 3, name: 'c', path: '/c', layout: 'grid' as const, position: 2, created_at: '' },
      ];
      useMeshStore.setState({
        meshes,
        meshesById: new Map(meshes.map(p => [p.id, p])),
      });
      mockInvoke.mockResolvedValueOnce(undefined); // update_mesh_positions

      await useMeshStore.getState().reorderMeshes(3, 0);

      const state = useMeshStore.getState();
      expect(state.meshes[0].id).toBe(3);
      expect(state.meshes[1].id).toBe(1);
      expect(state.meshes[2].id).toBe(2);
      expect(state.meshes[0].position).toBe(0);
      expect(state.meshes[1].position).toBe(1);
      expect(state.meshes[2].position).toBe(2);
    });

    it('refetches from backend on reorder failure to restore state', async () => {
      const meshes = [
        { id: 1, name: 'a', path: '/a', layout: 'grid' as const, position: 0, created_at: '' },
        { id: 2, name: 'b', path: '/b', layout: 'grid' as const, position: 1, created_at: '' },
      ];
      useMeshStore.setState({
        meshes,
        meshesById: new Map(meshes.map(p => [p.id, p])),
      });
      mockInvoke
        .mockRejectedValueOnce(new Error('Constraint violation')) // update_mesh_positions
        .mockResolvedValueOnce(meshes); // list_meshes refetch restores original order

      await useMeshStore.getState().reorderMeshes(2, 0);

      // fetchMeshes is called as recovery, which calls list_meshes
      expect(mockInvoke).toHaveBeenCalledWith('list_meshes');
    });
  });

  describe('createMesh', () => {
    it('passes name/path/color to create_mesh and appends the returned mesh', async () => {
      const created = {
        id: 7, name: 'newmesh', path: '/new', layout: 'grid', position: 0,
        created_at: '', color: '#38bdf8',
      };
      mockInvoke.mockResolvedValueOnce(created);

      const result = await useMeshStore.getState().createMesh('newmesh', '/new', '#38bdf8');

      expect(mockInvoke).toHaveBeenCalledWith('create_mesh', {
        name: 'newmesh', path: '/new', color: '#38bdf8',
      });
      expect(result).toEqual(created);
      const state = useMeshStore.getState();
      expect(state.meshes).toHaveLength(1);
      expect(state.meshesById.get(7)?.color).toBe('#38bdf8');
    });

    it('sends null color when none is provided', async () => {
      mockInvoke.mockResolvedValueOnce({ id: 8, name: 'm', path: '/m', layout: 'grid', position: 0, created_at: '' });
      await useMeshStore.getState().createMesh('m', '/m');
      expect(mockInvoke).toHaveBeenCalledWith('create_mesh', { name: 'm', path: '/m', color: null });
    });

    it('returns null and sets error on failure', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('boom'));
      const result = await useMeshStore.getState().createMesh('m', '/m', '#fff');
      expect(result).toBeNull();
      expect(useMeshStore.getState().error).toContain('boom');
    });
  });

  describe('updateMeshColor', () => {
    it('optimistically updates the color in meshes and meshesById', async () => {
      const meshes = [
        { id: 1, name: 'a', path: '/a', layout: 'grid' as const, position: 0, created_at: '' },
      ];
      useMeshStore.setState({ meshes, meshesById: new Map(meshes.map(p => [p.id, p])) });
      mockInvoke.mockResolvedValueOnce(undefined);

      await useMeshStore.getState().updateMeshColor(1, '#a78bfa');

      expect(mockInvoke).toHaveBeenCalledWith('update_mesh_color', { meshId: 1, color: '#a78bfa' });
      const state = useMeshStore.getState();
      expect(state.meshes[0].color).toBe('#a78bfa');
      expect(state.meshesById.get(1)?.color).toBe('#a78bfa');
    });

    it('ignores update for unknown mesh id', async () => {
      useMeshStore.setState({ meshes: [], meshesById: new Map() });
      mockInvoke.mockResolvedValueOnce(undefined);
      await useMeshStore.getState().updateMeshColor(999, '#000');
      expect(useMeshStore.getState().meshes).toEqual([]);
    });
  });

  describe('updateMeshLayout', () => {
    it('updates layout optimistically in meshes and meshesById', async () => {
      const meshes = [
        { id: 1, name: 'a', path: '/a', layout: 'grid' as const, position: 0, created_at: '' },
      ];
      useMeshStore.setState({
        meshes,
        meshesById: new Map(meshes.map(p => [p.id, p])),
      });
      mockInvoke.mockResolvedValueOnce(undefined);

      await useMeshStore.getState().updateMeshLayout(1, 'grid');

      const state = useMeshStore.getState();
      expect(state.meshes[0].layout).toBe('grid');
      expect(state.meshesById.get(1)?.layout).toBe('grid');
      expect(mockInvoke).toHaveBeenCalledWith('update_mesh_layout', { meshId: 1, layout: 'grid' });
    });

    it('ignores update for unknown mesh id', async () => {
      useMeshStore.setState({ meshes: [], meshesById: new Map() });
      mockInvoke.mockResolvedValueOnce(undefined);

      await useMeshStore.getState().updateMeshLayout(999, 'grid');

      expect(useMeshStore.getState().meshes).toEqual([]);
    });
  });
});
