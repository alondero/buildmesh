import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useToastStore } from '../../src/stores/toastStore';
import { SCOPE_NOTICE_PROVIDER } from '../../src/lib/scopeNotices';
import type { AgentNode } from '../../src/types/generated/AgentNode';

const DELETED_MESH: Mesh = {
  id: 1, name: 'doomed', path: '/repo/1', branch: 'main', position: 0,
  color: null, layout: 'grid', use_worktree: false, created_at: '2026-01-01',
  github_owner: null, github_repo: null, github_last_synced: null, pre_spawn_pool_size: 0,
};
const OTHER_MESH: Mesh = { ...DELETED_MESH, id: 2, name: 'survivor', path: '/repo/2', position: 1 };

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

/** Issue #2326 — seed `meshes` and `meshesById` from the same rows. */
function seedMeshes(meshes: Mesh[]) {
  useMeshStore.setState({
    meshes,
    meshesById: new Map(meshes.map((m) => [m.id, m])),
  });
}

/**
 * A promise the test settles by hand, so read and mutation order is explicit.
 *
 * Every deferred is also registered so `afterEach` can settle whatever a failing
 * test left pending: `vi.clearAllMocks` does not clear the one-time queue, so an
 * unresolved promise left there would be handed to the NEXT test and hang it,
 * turning one real failure into a screen of timeouts.
 */
const pendingDeferreds: { settle: (value: unknown) => void }[] = [];

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  pendingDeferreds.push({ settle: (value) => resolve(value as T) });
  return { promise, resolve, reject };
}

function settlePendingReads() {
  while (pendingDeferreds.length > 0) pendingDeferreds.pop()!.settle([]);
}

/** Issue #2326 — `list_meshes` calls made so far, in order. */
function listMeshesCalls(): number {
  return mockInvoke.mock.calls.filter((call) => call[0] === 'list_meshes').length;
}

/**
 * Issue #2326 — wait until the store is genuinely quiet.
 *
 * "No read outstanding" is not the same as "`loading` is false right now": a
 * discarded response schedules its trailing refresh in the microtask that
 * follows, so a single check can observe the store in the gap. Requiring two
 * consecutive observations of the same read count, with no read outstanding,
 * waits out that gap instead of racing it.
 */
async function settleReads() {
  let previous = -1;
  await vi.waitFor(() => {
    const reads = listMeshesCalls();
    const stable = reads === previous;
    previous = reads;
    expect({ stable, loading: useMeshStore.getState().loading })
      .toEqual({ stable: true, loading: false });
  });
}

function mesh(overrides: Partial<Mesh> & { id: number }): Mesh {
  return {
    name: `mesh-${overrides.id}`, path: `/repo/${overrides.id}`, branch: 'main',
    position: overrides.id - 1, color: null, layout: 'grid', use_worktree: false,
    created_at: '2026-01-01', github_owner: null, github_repo: null,
    github_last_synced: null, pre_spawn_pool_size: 0,
    ...overrides,
  };
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
    useToastStore.setState({ toasts: [] });
    vi.clearAllMocks();
  });

  afterEach(async () => {
    settlePendingReads();
    // Let every read this test started reach a verdict before the next one
    // begins, so a trailing refresh cannot land in the next test's mock queue.
    await settleReads();
    // `vi.clearAllMocks` keeps unconsumed one-time values, and a leftover would
    // be handed to the NEXT test — one shifted mock and every later assertion
    // reports a failure it did not cause. Reset the queue, then restore the
    // permissive default the setup file installs for unmocked commands.
    mockInvoke.mockReset();
    mockInvoke.mockResolvedValue({});
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

    // #2076 — "scope changes announce themselves". Deleting the Mesh the
    // user is looking at clears `selectedMeshId`, which the mesh→mode
    // subscription turns into All Nodes: a scope change the user did not
    // ask for, made silently. This is the only remaining selection-clear
    // of that kind (#2072 removed the re-click-deselect gesture).
    it('announces the scope move when the deleted Mesh was the selected one', async () => {
      useMeshStore.setState({
        meshes: [DELETED_MESH],
        meshesById: new Map([[1, DELETED_MESH]]),
        selectedMeshId: 1,
      });
      useToastStore.setState({ toasts: [] });
      mockInvoke
        .mockResolvedValueOnce(undefined)
        .mockResolvedValueOnce([]);

      const ok = await useMeshStore.getState().deleteMesh(1);

      expect(ok).toBe(true);
      const toasts = useToastStore.getState().toasts;
      expect(toasts).toHaveLength(1);
      expect(toasts[0].provider).toBe(SCOPE_NOTICE_PROVIDER);
      expect(toasts[0].message).toBe('Mesh “doomed” was deleted — the canvas moved to All Nodes.');
      expect(toasts[0].severity).toBe('info');
      // The live path is untouched by the notice: the delete committed and
      // the selection cleared.
      expect(useMeshStore.getState().selectedMeshId).toBeNull();
    });

    it('announces nothing when a different Mesh was deleted', async () => {
      useMeshStore.setState({
        meshes: [DELETED_MESH, OTHER_MESH],
        meshesById: new Map([[1, DELETED_MESH], [2, OTHER_MESH]]),
        selectedMeshId: 2,
      });
      useToastStore.setState({ toasts: [] });
      mockInvoke
        .mockResolvedValueOnce(undefined)
        .mockResolvedValueOnce([]);

      await useMeshStore.getState().deleteMesh(1);

      expect(useToastStore.getState().toasts).toEqual([]);
      expect(useMeshStore.getState().selectedMeshId).toBe(2);
    });

    it('announces nothing when the backend delete fails — no scope changed', async () => {
      useMeshStore.setState({
        meshes: [DELETED_MESH],
        meshesById: new Map([[1, DELETED_MESH]]),
        selectedMeshId: 1,
      });
      useToastStore.setState({ toasts: [] });
      mockInvoke.mockRejectedValueOnce(new Error('Not found'));

      await useMeshStore.getState().deleteMesh(1);

      expect(useToastStore.getState().toasts).toEqual([]);
      expect(useMeshStore.getState().selectedMeshId).toBe(1);
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

  // ---- Issue #2326: Mesh snapshot request and mutation authority ----
  //
  // Every case drives the real store methods with hand-settled promises, so the
  // assertion is about the collection a caller would render, not about which
  // IPC calls were made. A `list_meshes` response is one of three things: the
  // newest thing the store knows (it applies), an obsolete thing (it is dropped
  // and a single trailing read converges), or an outstanding thing (it waits).
  describe('snapshot authority (#2326)', () => {
    it('does not let an older read replace a newer accepted one', async () => {
      const older = deferred<Mesh[]>();
      const newer = deferred<Mesh[]>();
      // No third value is queued: if the store opens a replacement read here it
      // would fall through to the mock default and corrupt this test.
      mockInvoke
        .mockReturnValueOnce(older.promise)  // read A, started first
        .mockReturnValueOnce(newer.promise); // read B, started second

      const readA = useMeshStore.getState().fetchMeshes();
      const readB = useMeshStore.getState().fetchMeshes();

      // Newer accepted first...
      newer.resolve([mesh({ id: 2, name: 'newer' })]);
      await readB;
      expect(useMeshStore.getState().meshesById.get(2)?.name).toBe('newer');

      // ...then the older response lands and must be dropped, with the accepted
      // snapshot left alone.
      older.resolve([mesh({ id: 1, name: 'older' })]);
      await readA;

      const state = useMeshStore.getState();
      expect(state.meshesById.has(1)).toBe(false);
      expect(state.meshesById.get(2)?.name).toBe('newer');
      expect(state.error).toBeNull();

      // Nothing mutated, so the accepted snapshot already reflects every
      // committed write: dropping A converges it without a third IPC read, and
      // without re-entering `loading`.
      await settleReads();
      expect(useMeshStore.getState().loading).toBe(false);
      expect(useMeshStore.getState().meshesById.get(2)?.name).toBe('newer');
      expect(listMeshesCalls()).toBe(2);
    });

    it('applies both reads in order when the older one answers first', async () => {
      const older = deferred<Mesh[]>();
      const newer = deferred<Mesh[]>();
      mockInvoke
        .mockReturnValueOnce(older.promise)
        .mockReturnValueOnce(newer.promise);

      const readA = useMeshStore.getState().fetchMeshes();
      const readB = useMeshStore.getState().fetchMeshes();

      older.resolve([mesh({ id: 1, name: 'older' })]);
      await readA;
      expect(useMeshStore.getState().meshesById.get(1)?.name).toBe('older');

      newer.resolve([mesh({ id: 1, name: 'newer' })]);
      await readB;

      await settleReads();
      expect(useMeshStore.getState().meshesById.get(1)?.name).toBe('newer');
      expect(useMeshStore.getState().meshes[0].name).toBe('newer');
    });

    it('keeps an acknowledged rename when the snapshot that predates it lands', async () => {
      const stale = deferred<Mesh[]>();
      const trailing = deferred<Mesh[]>();
      seedMeshes([mesh({ id: 1, name: 'old' })]);
      mockInvoke
        .mockReturnValueOnce(stale.promise)   // read started before the rename
        .mockResolvedValueOnce(undefined)     // update_mesh_name
        .mockReturnValueOnce(trailing.promise);

      const refresh = useMeshStore.getState().refreshMeshes();
      await useMeshStore.getState().updateMeshName(1, 'saved-new-name');
      expect(useMeshStore.getState().meshesById.get(1)?.name).toBe('saved-new-name');

      stale.resolve([mesh({ id: 1, name: 'old' })]);
      await vi.waitFor(() => expect(useMeshStore.getState().loading).toBe(true));

      const state = useMeshStore.getState();
      expect(state.meshesById.get(1)?.name).toBe('saved-new-name');
      expect(state.meshes[0].name).toBe('saved-new-name');

      // `refreshMeshes` waits for the trailing read it caused, so it resolves
      // on an applied snapshot rather than on the discarded one.
      trailing.resolve([mesh({ id: 1, name: 'saved-new-name' })]);
      await refresh;
      await settleReads();
      expect(useMeshStore.getState().meshesById.get(1)?.name).toBe('saved-new-name');
      expect(useMeshStore.getState().loading).toBe(false);
    });

    it('keeps acknowledged colour and layout writes against a stale snapshot', async () => {
      const stale = deferred<Mesh[]>();
      const trailing = deferred<Mesh[]>();
      seedMeshes([mesh({ id: 1, name: 'alpha' })]);
      mockInvoke
        .mockReturnValueOnce(stale.promise)
        .mockResolvedValueOnce(undefined) // update_mesh_color
        .mockResolvedValueOnce(undefined) // update_mesh_layout
        .mockReturnValueOnce(trailing.promise);

      const read = useMeshStore.getState().fetchMeshes();
      await useMeshStore.getState().updateMeshColor(1, '#a78bfa');
      await useMeshStore.getState().updateMeshLayout(1, 'single');

      stale.resolve([mesh({ id: 1, name: 'alpha' })]);
      await read;
      await vi.waitFor(() => expect(useMeshStore.getState().loading).toBe(true));

      const row = useMeshStore.getState().meshesById.get(1);
      expect(row?.color).toBe('#a78bfa');
      expect(row?.layout).toBe('single');

      trailing.resolve([mesh({ id: 1, name: 'alpha', color: '#a78bfa', layout: 'single' })]);
      await settleReads();
    });

    it('keeps a created Mesh against a snapshot that predates it', async () => {
      const stale = deferred<Mesh[]>();
      const trailing = deferred<Mesh[]>();
      const created = mesh({ id: 2, name: 'fresh' });
      seedMeshes([mesh({ id: 1, name: 'alpha' })]);
      mockInvoke
        .mockReturnValueOnce(stale.promise)
        .mockResolvedValueOnce(created)      // create_mesh
        .mockReturnValueOnce(trailing.promise);

      const read = useMeshStore.getState().fetchMeshes();
      const result = await useMeshStore.getState().createMesh('fresh', '/repo/2');
      expect(result?.id).toBe(2);

      stale.resolve([mesh({ id: 1, name: 'alpha' })]);
      await read;
      await vi.waitFor(() => expect(useMeshStore.getState().loading).toBe(true));
      expect(useMeshStore.getState().meshes.map((m) => m.id)).toEqual([1, 2]);
      expect(useMeshStore.getState().meshesById.has(2)).toBe(true);

      trailing.resolve([mesh({ id: 1, name: 'alpha' }), created]);
      await settleReads();
      expect(useMeshStore.getState().meshesById.has(2)).toBe(true);
    });

    it('does not let a late snapshot resurrect a deleted Mesh', async () => {
      const stale = deferred<Mesh[]>();
      seedMeshes([DELETED_MESH, OTHER_MESH]);
      // No trailing value is queued: the post-delete refetch already applied the
      // revision this drop would have converged, so a replacement read would be
      // spurious IPC (and a leak into the next test's queue).
      mockInvoke
        .mockReturnValueOnce(stale.promise)   // read started before the delete
        .mockResolvedValueOnce(undefined)     // delete_mesh
        .mockResolvedValueOnce([OTHER_MESH]); // post-delete refetch

      const read = useMeshStore.getState().fetchMeshes();
      await expect(useMeshStore.getState().deleteMesh(1)).resolves.toBe(true);

      stale.resolve([DELETED_MESH, OTHER_MESH]);
      await read;

      await settleReads();
      const state = useMeshStore.getState();
      expect(state.meshes.map((m) => m.id)).toEqual([2]);
      expect(state.meshesById.has(1)).toBe(false);
      expect(state.loading).toBe(false);
    });

    it('does not let an older rejection replace a newer success with an error', async () => {
      const older = deferred<Mesh[]>();
      const newer = deferred<Mesh[]>();
      mockInvoke
        .mockReturnValueOnce(older.promise)
        .mockReturnValueOnce(newer.promise);

      const readA = useMeshStore.getState().fetchMeshes();
      const readB = useMeshStore.getState().fetchMeshes();

      newer.resolve([mesh({ id: 2, name: 'newer' })]);
      await readB;
      older.reject(new Error('old read failed'));
      await readA;

      // The obsolete failure is dropped: it never reaches `error`, and the
      // accepted snapshot it was racing stays on screen. The accepted snapshot
      // already reflects every committed write, so the drop converges without a
      // replacement read.
      await settleReads();
      expect(useMeshStore.getState().error).toBeNull();
      expect(useMeshStore.getState().meshesById.get(2)?.name).toBe('newer');
      expect(useMeshStore.getState().loading).toBe(false);
      expect(listMeshesCalls()).toBe(2);
    });

    it('keeps loading true while a newer read is outstanding, then settles it', async () => {
      const older = deferred<Mesh[]>();
      const newer = deferred<Mesh[]>();
      mockInvoke
        .mockReturnValueOnce(older.promise)
        .mockReturnValueOnce(newer.promise);

      const readA = useMeshStore.getState().fetchMeshes();
      const readB = useMeshStore.getState().fetchMeshes();
      expect(useMeshStore.getState().loading).toBe(true);

      // The newer read answers while the older is still outstanding: its own
      // completion must not report loading finished for the one still running.
      newer.resolve([mesh({ id: 2, name: 'newer' })]);
      await readB;
      expect(useMeshStore.getState().loading).toBe(true);
      expect(useMeshStore.getState().meshesById.get(2)?.name).toBe('newer');

      // The older read is dropped. Nothing mutated, so the newer snapshot already
      // converged the collection: loading settles and the store stays quiet
      // rather than opening a replacement read.
      older.resolve([mesh({ id: 1, name: 'older' })]);
      await readA;

      await settleReads();
      expect(useMeshStore.getState().loading).toBe(false);
      expect(useMeshStore.getState().meshesById.has(1)).toBe(false);
      expect(useMeshStore.getState().meshesById.get(2)?.name).toBe('newer');
      expect(listMeshesCalls()).toBe(2);
    });

    it('clears an older failure when the newer snapshot applies after it', async () => {
      const older = deferred<Mesh[]>();
      const newer = deferred<Mesh[]>();
      mockInvoke
        .mockReturnValueOnce(older.promise)
        .mockReturnValueOnce(newer.promise);

      const readA = useMeshStore.getState().fetchMeshes();
      const readB = useMeshStore.getState().fetchMeshes();

      // The older read fails while the newer one is still outstanding, so at
      // that moment it is still authoritative and owns the error.
      older.reject(new Error('older read failed'));
      await readA;
      expect(useMeshStore.getState().error).toContain('older read failed');

      // The newer snapshot then applies: it must take the error with it, or the
      // store shows a failure banner over fresh data for the rest of the session.
      newer.resolve([mesh({ id: 2, name: 'newer' })]);
      await readB;

      await settleReads();
      expect(useMeshStore.getState().error).toBeNull();
      expect(useMeshStore.getState().meshesById.get(2)?.name).toBe('newer');
    });

    it('does not let a snapshot overwrite an optimistic reorder still pending', async () => {
      const stale = deferred<Mesh[]>();
      const positions = deferred<void>();
      const trailing = deferred<Mesh[]>();
      seedMeshes([mesh({ id: 1, name: 'a' }), mesh({ id: 2, name: 'b' }), mesh({ id: 3, name: 'c' })]);
      mockInvoke
        .mockReturnValueOnce(stale.promise)      // read started before the drag
        .mockReturnValueOnce(positions.promise)  // update_mesh_positions, held open
        .mockReturnValueOnce(trailing.promise);  // convergence read after the drag settles

      const read = useMeshStore.getState().fetchMeshes();
      const reorder = useMeshStore.getState().reorderMeshes(3, 0);
      expect(useMeshStore.getState().meshes.map((m) => m.id)).toEqual([3, 1, 2]);

      stale.resolve([mesh({ id: 1, name: 'a' }), mesh({ id: 2, name: 'b' }), mesh({ id: 3, name: 'c' })]);
      await read;
      await settleReads();
      expect(useMeshStore.getState().meshes.map((m) => m.id)).toEqual([3, 1, 2]);
      expect(useMeshStore.getState().meshes.map((m) => m.position)).toEqual([0, 1, 2]);
      // No trailing read during the reorder window — it would be dropped too,
      // so the drag's own acknowledgement is what converges the order.
      expect(useMeshStore.getState().loading).toBe(false);

      positions.resolve();
      await reorder;
      await vi.waitFor(() => expect(useMeshStore.getState().loading).toBe(true));

      trailing.resolve([
        mesh({ id: 3, name: 'c', position: 0 }),
        mesh({ id: 1, name: 'a', position: 1 }),
        mesh({ id: 2, name: 'b', position: 2 }),
      ]);
      await settleReads();
      expect(useMeshStore.getState().meshes.map((m) => m.id)).toEqual([3, 1, 2]);
    });

    it('converges a trailing refresh that a second write supersedes, then stops', async () => {
      const stale = deferred<Mesh[]>();
      const firstTrailing = deferred<Mesh[]>();
      const secondTrailing = deferred<Mesh[]>();
      seedMeshes([mesh({ id: 1, name: 'old' })]);
      mockInvoke
        .mockReturnValueOnce(stale.promise)            // read started before both writes
        .mockResolvedValueOnce(undefined)              // update_mesh_name
        .mockReturnValueOnce(firstTrailing.promise)    // the refresh the drop starts
        .mockResolvedValueOnce(undefined)              // update_mesh_color, while it runs
        .mockReturnValueOnce(secondTrailing.promise);  // its one replacement

      const read = useMeshStore.getState().fetchMeshes();
      await useMeshStore.getState().updateMeshName(1, 'first');

      stale.resolve([mesh({ id: 1, name: 'old' })]);
      await read;
      await vi.waitFor(() => expect(useMeshStore.getState().loading).toBe(true));
      expect(useMeshStore.getState().meshesById.get(1)?.name).toBe('first');

      // A second write lands while the trailing refresh is in flight, so that
      // refresh is dropped too — and the acknowledged colour must survive it.
      await useMeshStore.getState().updateMeshColor(1, '#a78bfa');
      firstTrailing.resolve([mesh({ id: 1, name: 'first' })]);
      await vi.waitFor(() => expect(listMeshesCalls()).toBe(3));
      expect(useMeshStore.getState().meshesById.get(1)?.color).toBe('#a78bfa');

      secondTrailing.resolve([mesh({ id: 1, name: 'first', color: '#a78bfa' })]);
      await settleReads();
      const row = useMeshStore.getState().meshesById.get(1);
      expect(row?.name).toBe('first');
      expect(row?.color).toBe('#a78bfa');

      // The chain is bounded: a settled chain starts no further refresh.
      const readsAfterConvergence = listMeshesCalls();
      await vi.waitFor(() => expect(useMeshStore.getState().loading).toBe(false));
      expect(listMeshesCalls()).toBe(readsAfterConvergence);
    });

    it('recovers the persisted order when a reorder fails', async () => {
      seedMeshes([mesh({ id: 1, name: 'a' }), mesh({ id: 2, name: 'b' })]);
      // Keyed by command rather than by call order: this case is about what the
      // recovery read is allowed to do, so it must not depend on how many reads
      // reached the IPC seam first.
      mockInvoke.mockImplementation(async (cmd: string) => {
        if (cmd === 'update_mesh_positions') throw new Error('Constraint violation');
        if (cmd === 'list_meshes') return [mesh({ id: 1, name: 'a' }), mesh({ id: 2, name: 'b' })];
        return undefined;
      });

      await useMeshStore.getState().reorderMeshes(2, 0);

      await settleReads();
      // The recovery read starts outside the reorder window, so it is
      // authoritative and applies: the failed drag does not survive.
      expect(useMeshStore.getState().meshes.map((m) => m.id)).toEqual([1, 2]);
      expect(useMeshStore.getState().meshesById.get(2)?.position).toBe(1);
      expect(useMeshStore.getState().loading).toBe(false);
    });

    it('ignores a reorder of a Mesh this store never loaded', async () => {
      seedMeshes([mesh({ id: 1, name: 'a' })]);

      await useMeshStore.getState().reorderMeshes(99, 0);

      // Nothing moved, so there is nothing to persist: the previous code read
      // `updatedMeshes.map` on `undefined` and reported the TypeError as a
      // failed drag. No invoke is queued, so a rejection here would leak into
      // the next test's one-time mock queue.
      expect(mockInvoke).not.toHaveBeenCalled();
      expect(useMeshStore.getState().error).toBeNull();
      expect(useMeshStore.getState().meshes.map((m) => m.id)).toEqual([1]);
    });

    it('rejects refreshMeshes on failure and keeps fetchMeshes non-throwing', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('DB unavailable'));

      await expect(useMeshStore.getState().refreshMeshes()).rejects.toThrow('DB unavailable');
      expect(useMeshStore.getState().error).toContain('DB unavailable');
      expect(useMeshStore.getState().loading).toBe(false);

      mockInvoke.mockRejectedValueOnce(new Error('still down'));

      await expect(useMeshStore.getState().fetchMeshes()).resolves.toBeUndefined();
      expect(useMeshStore.getState().error).toContain('still down');
      expect(useMeshStore.getState().loading).toBe(false);
    });

    it('keeps the previous row reference for a Mesh a refresh did not change', async () => {
      const alpha = mesh({ id: 1, name: 'alpha' });
      const beta = mesh({ id: 2, name: 'beta' });
      seedMeshes([alpha, beta]);
      mockInvoke.mockResolvedValueOnce([
        mesh({ id: 1, name: 'alpha' }),
        mesh({ id: 2, name: 'beta', color: '#fff' }),
      ]);

      await useMeshStore.getState().fetchMeshes();

      const state = useMeshStore.getState();
      // Both views hold the same reconciled objects, so a per-id selector skips
      // the render for a Mesh nothing changed on.
      expect(state.meshesById.get(1)).toBe(alpha);
      expect(state.meshes[0]).toBe(alpha);
      expect(state.meshesById.get(2)).not.toBe(beta);
      expect(state.meshesById.get(2)?.color).toBe('#fff');
    });
  });
});
