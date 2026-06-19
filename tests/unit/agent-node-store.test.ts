import { describe, it, expect, beforeEach, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import {
  setWorktreeCloseActionResolverForTests,
  useAgentNodeStore,
  type AgentNode,
} from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import type { WorktreeCloseSafety } from '../../src/lib/worktreeClose';

const mockInvoke = invoke as ReturnType<typeof vi.fn>;
const mockEmit = emit as ReturnType<typeof vi.fn>;

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 1, mesh_id: 1, name: 'bold-keen-brook', path: '/a', branch: 'main',
    env: 'windows', provider: 'anthropic', status: 'idle', created_at: '',
    use_worktree: true, position: 0,
    ...overrides,
  };
}

function makeSafety(overrides: Partial<WorktreeCloseSafety> = {}): WorktreeCloseSafety {
  return {
    worktree_path: '/a/.claude/worktrees/bold-keen-brook',
    has_uncommitted: false,
    has_unpushed: false,
    is_detached: false,
    ...overrides,
  };
}

function mockDeleteFlow(safety: WorktreeCloseSafety) {
  mockInvoke.mockImplementation((cmd: string) => {
    if (cmd === 'get_worktree_close_safety') return Promise.resolve(safety);
    return Promise.resolve(undefined);
  });
}

describe('useAgentNodeStore', () => {
  beforeEach(() => {
    useAgentNodeStore.setState({
      agentNodes: [],
      activeNodeId: null,
      loading: false,
      error: null,
      closingNodeIds: new Set(),
    });
    vi.clearAllMocks();
    setWorktreeCloseActionResolverForTests();
    // selectProviderForMesh reaches into useMeshStore — keep it clean too so
    // a leftover selectedMeshId from a sibling test can't mask a rollback bug.
    useMeshStore.setState({
      meshes: [],
      meshesById: new Map(),
      selectedMeshId: null,
      loading: false,
      error: null,
    });
  });

  describe('fetchAgentNodes', () => {
    it('populates agent nodes on success', async () => {
      const nodes = [makeNode()];
      mockInvoke.mockResolvedValueOnce(nodes);

      await useAgentNodeStore.getState().fetchAgentNodes();

      expect(useAgentNodeStore.getState().agentNodes).toEqual(nodes);
      expect(useAgentNodeStore.getState().loading).toBe(false);
    });

    it('sets error on fetch failure', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('timeout'));

      await useAgentNodeStore.getState().fetchAgentNodes();

      expect(useAgentNodeStore.getState().error).toContain('timeout');
    });
  });

  describe('getActiveNode', () => {
    it('returns null when no active node', () => {
      expect(useAgentNodeStore.getState().getActiveNode()).toBeNull();
    });

    it('returns the active node when set', () => {
      const node = makeNode({ id: 5 });
      useAgentNodeStore.setState({ agentNodes: [node], activeNodeId: 5 });

      expect(useAgentNodeStore.getState().getActiveNode()).toEqual(node);
    });

    it('returns null if activeNodeId does not match any node', () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 5 })], activeNodeId: 99 });

      expect(useAgentNodeStore.getState().getActiveNode()).toBeNull();
    });
  });

  describe('getActiveMeshId', () => {
    it('returns mesh_id of active node', () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 3, mesh_id: 7 })], activeNodeId: 3 });

      expect(useAgentNodeStore.getState().getActiveMeshId()).toBe(7);
    });

    it('returns null when no active node', () => {
      expect(useAgentNodeStore.getState().getActiveMeshId()).toBeNull();
    });
  });

  describe('attention event handling via initAttentionListeners', () => {
    // initAttentionListeners uses a closure guard (listenersAttached) that only
    // registers once per process. We test all scenarios in one test to keep
    // listeners alive across the mockListeners.clear() boundary.
    it('handles attention-needed, attention-cleared, and session-renamed events', async () => {
      const node = makeNode({ id: 10, name: 'test-node', status: 'running' });
      useAgentNodeStore.setState({ agentNodes: [node] });
      await useAgentNodeStore.getState().initAttentionListeners();

      await mockEmit('attention-needed', { session_id: 10 });
      expect(useAgentNodeStore.getState().agentNodes.find(n => n.id === 10)?.status).toBe('awaiting_input');

      await mockEmit('attention-cleared', { session_id: 10 });
      expect(useAgentNodeStore.getState().agentNodes.find(n => n.id === 10)?.status).toBe('running');

      await mockEmit('node-renamed', { node_id: 10, name: 'fix-auth-flow' });
      expect(useAgentNodeStore.getState().agentNodes.find(n => n.id === 10)?.name).toBe('fix-auth-flow');

      // `node-created` is the internal event the Rust spawn paths
      // (`create_issue_node`, `create_pr_node`, mobile HTTP `/nodes`) emit
      // after committing a new node row (issue #490). The store refetches
      // the full list rather than appending — the row is already committed
      // by the time the event fires, so a refetch is race-free.
      const newRow = makeNode({ id: 99, name: 'spawned-node' });
      mockInvoke.mockResolvedValueOnce([newRow]);
      await mockEmit('node-created', { id: 99 });
      // fetchAgentNodes is async; await the microtask + the awaited promise.
      await new Promise((r) => setTimeout(r, 0));
      await new Promise((r) => setTimeout(r, 0));
      expect(useAgentNodeStore.getState().agentNodes.find(n => n.id === 99)).toBeDefined();

      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 10, status: 'running' })] });
      await mockEmit('attention-needed', { session_id: 999 });
      expect(useAgentNodeStore.getState().agentNodes.find(n => n.id === 10)?.status).toBe('running');
    });
  });

  describe('createAgentNode', () => {
    it('appends new node to state', async () => {
      const newNode = { id: 20, mesh_id: 1, name: 'new-node', path: '/n', branch: 'feat', env: 'windows', provider: 'anthropic', status: 'idle', created_at: '' };
      mockInvoke.mockResolvedValueOnce(newNode);

      const result = await useAgentNodeStore.getState().createAgentNode(1, 'new-node', '/n', 'feat');

      expect(result.id).toBe(20);
      expect(useAgentNodeStore.getState().agentNodes).toHaveLength(1);
      expect(useAgentNodeStore.getState().agentNodes[0].name).toBe('new-node');
    });

    it('sets error and throws on failure', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('Duplicate path'));

      await expect(
        useAgentNodeStore.getState().createAgentNode(1, 'x', '/dup', 'main')
      ).rejects.toThrow('Duplicate path');

      expect(useAgentNodeStore.getState().error).toContain('Duplicate path');
    });
  });

  describe('selectProviderForMesh', () => {
    // The whole point of pulling this orchestration out of Sidebar.handleSelectProvider
    // (issue #283): the create-then-activate-then-select-mesh sequence enforces ONE
    // invariant — "only switch active mesh/node if creation succeeded" — and the
    // invariant lives next to the orchestration, not in each click handler.

    it('creates the node, then sets it active, then selects the mesh', async () => {
      const newNode = {
        id: 42, mesh_id: 7, name: 'mesh-7', path: '/p', branch: 'main',
        env: 'windows', provider: 'anthropic', status: 'idle', created_at: '',
        use_worktree: true, position: 0,
      };
      mockInvoke.mockResolvedValueOnce(newNode); // create_agent_node

      const result = await useAgentNodeStore.getState().selectProviderForMesh(
        7, 'mesh-7', '/p', 'anthropic', undefined,
      );

      expect(result.id).toBe(42);
      expect(mockInvoke).toHaveBeenCalledWith('create_agent_node', {
        meshId: 7, name: 'mesh-7', path: '/p', branch: 'main',
        provider: 'anthropic', useWorktree: undefined,
      });
      expect(useAgentNodeStore.getState().agentNodes).toHaveLength(1);
      expect(useAgentNodeStore.getState().activeNodeId).toBe(42);
      expect(useMeshStore.getState().selectedMeshId).toBe(7);
    });

    it('passes useWorktree=false through to create_agent_node (mesh-root spawn)', async () => {
      // Alt-click on + spawns in the mesh root: that signal must reach the
      // backend, otherwise the new node gets a worktree it shouldn't have.
      const newNode = {
        id: 43, mesh_id: 7, name: 'm', path: '/p', branch: 'main',
        env: 'windows', provider: 'anthropic', status: 'idle', created_at: '',
        use_worktree: false, position: 0,
      };
      mockInvoke.mockResolvedValueOnce(newNode);

      await useAgentNodeStore.getState().selectProviderForMesh(7, 'm', '/p', 'anthropic', false);

      expect(mockInvoke).toHaveBeenCalledWith('create_agent_node', expect.objectContaining({
        useWorktree: false,
      }));
    });

    it('does NOT switch the active mesh when node creation fails (the bug fix)', async () => {
      // Pre-seed: a different mesh is selected. If creation fails the
      // active-mesh selection must NOT move to the new mesh — that was the
      // "mesh selected but no node exists" half-applied state called out by
      // the issue.
      useMeshStore.setState({ selectedMeshId: 99 });
      mockInvoke.mockRejectedValueOnce(new Error('create_agent_node failed'));

      await expect(
        useAgentNodeStore.getState().selectProviderForMesh(7, 'm', '/p', 'anthropic', undefined)
      ).rejects.toThrow('create_agent_node failed');

      expect(useAgentNodeStore.getState().activeNodeId).toBeNull();
      expect(useMeshStore.getState().selectedMeshId).toBe(99); // unchanged
      expect(useAgentNodeStore.getState().agentNodes).toHaveLength(0);
    });
  });

  describe('setActiveNode', () => {
    it('switches activeNodeId synchronously, with no backend round-trip', () => {
      // The click must feel instant: the active node flips immediately and the
      // UI (highlight, terminal focus, file-watch) reacts without any IPC.
      useAgentNodeStore.getState().setActiveNode(5);

      expect(useAgentNodeStore.getState().activeNodeId).toBe(5);
      expect(mockInvoke).not.toHaveBeenCalled();
    });

    it('clears the active node when setting null', () => {
      useAgentNodeStore.setState({ activeNodeId: 5 });

      useAgentNodeStore.getState().setActiveNode(null);

      expect(useAgentNodeStore.getState().activeNodeId).toBeNull();
    });
  });

  describe('deleteAgentNode', () => {
    it('silently removes a clean worktree when closing the node', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 15 })], activeNodeId: 15 });
      mockDeleteFlow(makeSafety());
      const prompt = vi.fn();
      setWorktreeCloseActionResolverForTests(prompt);

      await useAgentNodeStore.getState().deleteAgentNode(15);

      expect(prompt).not.toHaveBeenCalled();
      expect(mockInvoke).toHaveBeenCalledWith('get_worktree_close_safety', { nodeId: 15 });
      expect(mockInvoke).toHaveBeenCalledWith('kill_agent', { sessionId: 15 });
      expect(mockInvoke).toHaveBeenCalledWith('delete_agent_node', {
        nodeId: 15,
        removeWorktree: true,
      });
      expect(useAgentNodeStore.getState().agentNodes).toHaveLength(0);
      expect(useAgentNodeStore.getState().activeNodeId).toBeNull();
    });

    it('clears activeNodeId only if deleted node was active', async () => {
      useAgentNodeStore.setState({
        agentNodes: [makeNode({ id: 1 }), makeNode({ id: 2, path: '/b' })],
        activeNodeId: 2,
      });
      mockDeleteFlow(makeSafety());

      await useAgentNodeStore.getState().deleteAgentNode(1);

      expect(useAgentNodeStore.getState().activeNodeId).toBe(2);
      expect(useAgentNodeStore.getState().agentNodes).toHaveLength(1);
    });

    it('keeps a risky worktree on disk when the prompt chooses keep', async () => {
      const node = makeNode({ id: 7 });
      useAgentNodeStore.setState({ agentNodes: [node], activeNodeId: 7 });
      mockDeleteFlow(makeSafety({ has_uncommitted: true }));
      const prompt = vi.fn().mockResolvedValue('keep');
      setWorktreeCloseActionResolverForTests(prompt);

      await useAgentNodeStore.getState().deleteAgentNode(7);

      expect(prompt).toHaveBeenCalledWith(node, makeSafety({ has_uncommitted: true }));
      expect(mockInvoke).toHaveBeenCalledWith('delete_agent_node', {
        nodeId: 7,
        removeWorktree: false,
      });
      expect(useAgentNodeStore.getState().agentNodes).toEqual([]);
    });

    it('removes a risky worktree when the prompt chooses remove', async () => {
      const node = makeNode({ id: 8 });
      useAgentNodeStore.setState({ agentNodes: [node], activeNodeId: 8 });
      mockDeleteFlow(makeSafety({ has_unpushed: true }));
      const prompt = vi.fn().mockResolvedValue('remove');
      setWorktreeCloseActionResolverForTests(prompt);

      await useAgentNodeStore.getState().deleteAgentNode(8);

      expect(prompt).toHaveBeenCalledWith(node, makeSafety({ has_unpushed: true }));
      expect(mockInvoke).toHaveBeenCalledWith('delete_agent_node', {
        nodeId: 8,
        removeWorktree: true,
      });
      expect(useAgentNodeStore.getState().agentNodes).toEqual([]);
    });

    it('cancels close before killing or deleting when the prompt chooses cancel', async () => {
      const node = makeNode({ id: 9 });
      useAgentNodeStore.setState({ agentNodes: [node], activeNodeId: 9 });
      mockDeleteFlow(makeSafety({ has_uncommitted: true }));
      setWorktreeCloseActionResolverForTests(vi.fn().mockResolvedValue('cancel'));

      await useAgentNodeStore.getState().deleteAgentNode(9);

      expect(mockInvoke).toHaveBeenCalledTimes(1);
      expect(mockInvoke).toHaveBeenCalledWith('get_worktree_close_safety', { nodeId: 9 });
      expect(useAgentNodeStore.getState().agentNodes).toEqual([node]);
      expect(useAgentNodeStore.getState().activeNodeId).toBe(9);
    });

    it('removes the node from the UI before the worktree cleanup resolves', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 15 })], activeNodeId: 15 });
      let resolveDelete!: () => void;
      const deletePending = new Promise<void>((r) => { resolveDelete = r; });
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_worktree_close_safety') return Promise.resolve(makeSafety());
        if (cmd === 'delete_agent_node') return deletePending;
        return Promise.resolve(undefined);
      });

      const closePromise = useAgentNodeStore.getState().deleteAgentNode(15);

      // The node must be gone optimistically while delete_agent_node is still pending.
      await vi.waitFor(() => {
        expect(useAgentNodeStore.getState().agentNodes).toHaveLength(0);
        expect(useAgentNodeStore.getState().activeNodeId).toBeNull();
      });

      resolveDelete();
      await closePromise;
    });

    it('still deletes the session when kill_agent rejects', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 22 })], activeNodeId: 22 });
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_worktree_close_safety') return Promise.resolve(makeSafety());
        if (cmd === 'kill_agent') return Promise.reject(new Error('status update failed'));
        return Promise.resolve(undefined);
      });

      await useAgentNodeStore.getState().deleteAgentNode(22);

      // A kill_agent failure must not abandon the DB-side delete, or the node
      // would resurrect on the next fetch.
      expect(mockInvoke).toHaveBeenCalledWith('delete_agent_node', {
        nodeId: 22,
        removeWorktree: true,
      });
      expect(useAgentNodeStore.getState().agentNodes).toHaveLength(0);
    });

    it('marks the node closing synchronously, before the slow safety check resolves', async () => {
      // The whole point of the close UX fix: the safety check is a git status
      // + ref walk that can take seconds on a large repo, and it has to run
      // BEFORE we can drop the row (its result decides whether to prompt about
      // uncommitted work). Without an immediate "closing" flag the click looks
      // ignored for those seconds. This pins that the flag flips the instant
      // the user clicks — before any IPC resolves.
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 30 })], activeNodeId: 30 });
      let resolveSafety!: (s: WorktreeCloseSafety) => void;
      const safetyPending = new Promise<WorktreeCloseSafety>((r) => { resolveSafety = r; });
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_worktree_close_safety') return safetyPending;
        return Promise.resolve(undefined);
      });

      const closePromise = useAgentNodeStore.getState().deleteAgentNode(30);

      // Synchronously after the call: node is still visible (we can't know yet
      // whether to prompt) but it is flagged closing so the UI can show a
      // spinner instead of looking frozen.
      expect(useAgentNodeStore.getState().closingNodeIds.has(30)).toBe(true);
      expect(useAgentNodeStore.getState().agentNodes).toHaveLength(1);

      resolveSafety(makeSafety());
      await closePromise;

      // Once the (clean) check resolves the node is gone and the flag is cleared.
      expect(useAgentNodeStore.getState().agentNodes).toHaveLength(0);
      expect(useAgentNodeStore.getState().closingNodeIds.has(30)).toBe(false);
    });

    it('clears the closing flag when the prompt cancels the close', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 31 })], activeNodeId: 31 });
      mockDeleteFlow(makeSafety({ has_uncommitted: true }));
      setWorktreeCloseActionResolverForTests(vi.fn().mockResolvedValue('cancel'));

      await useAgentNodeStore.getState().deleteAgentNode(31);

      // Cancel keeps the node, and must not leave it stuck in the dimmed
      // "closing" state forever.
      expect(useAgentNodeStore.getState().agentNodes).toHaveLength(1);
      expect(useAgentNodeStore.getState().closingNodeIds.has(31)).toBe(false);
    });

    it('ignores a repeat close while one is already in flight', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 32 })], activeNodeId: 32 });
      let resolveSafety!: (s: WorktreeCloseSafety) => void;
      const safetyPending = new Promise<WorktreeCloseSafety>((r) => { resolveSafety = r; });
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_worktree_close_safety') return safetyPending;
        return Promise.resolve(undefined);
      });

      const first = useAgentNodeStore.getState().deleteAgentNode(32);
      // A double-click while the safety check is still pending must not fire a
      // second safety check / kill / delete round-trip.
      const second = useAgentNodeStore.getState().deleteAgentNode(32);

      resolveSafety(makeSafety());
      await Promise.all([first, second]);

      const safetyCalls = mockInvoke.mock.calls.filter(([cmd]) => cmd === 'get_worktree_close_safety');
      expect(safetyCalls).toHaveLength(1);
    });

    it('does not request worktree removal when the node has no worktree path', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 10, worktree_name: undefined })], activeNodeId: 10 });
      mockDeleteFlow(makeSafety({ worktree_path: null }));

      await useAgentNodeStore.getState().deleteAgentNode(10);

      expect(mockInvoke).toHaveBeenCalledWith('delete_agent_node', {
        nodeId: 10,
        removeWorktree: false,
      });
    });
  });

  describe('renameAgentNode', () => {
    it('optimistically updates the name and calls rename_agent_node', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 11, name: 'bold-keen-brook' })] });
      mockInvoke.mockResolvedValueOnce(undefined);

      await useAgentNodeStore.getState().renameAgentNode(11, 'Refactor OAuth callback');

      // The store reflects the new name.
      expect(useAgentNodeStore.getState().agentNodes.find(n => n.id === 11)?.name)
        .toBe('Refactor OAuth callback');
      // And the backend was told the same name, with the camelCase nodeId
      // convention used by every other agent-node invoke call.
      expect(mockInvoke).toHaveBeenCalledWith('rename_agent_node', {
        nodeId: 11,
        name: 'Refactor OAuth callback',
      });
    });

    it('rolls back the optimistic update and re-throws on invoke failure', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 12, name: 'old-name' })] });
      mockInvoke.mockRejectedValueOnce(new Error('name too long (max 80 chars)'));

      await expect(
        useAgentNodeStore.getState().renameAgentNode(12, 'x'.repeat(81))
      ).rejects.toThrow('name too long');

      // Rollback restored the prior name, and the error is surfaced on the store.
      expect(useAgentNodeStore.getState().agentNodes.find(n => n.id === 12)?.name)
        .toBe('old-name');
      expect(useAgentNodeStore.getState().error).toContain('name too long');
    });

    it('is a no-op when the node is not in the store', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 13 })] });

      await useAgentNodeStore.getState().renameAgentNode(999, 'whatever');

      // No invoke call was made and the store is untouched.
      expect(mockInvoke).not.toHaveBeenCalled();
      expect(useAgentNodeStore.getState().agentNodes).toHaveLength(1);
    });
  });

  describe('reorderAgentNode', () => {
    // Three nodes in one mesh, positions 0,1,2.
    const seed = () => useAgentNodeStore.setState({
      agentNodes: [
        makeNode({ id: 1, position: 0 }),
        makeNode({ id: 2, position: 1 }),
        makeNode({ id: 3, position: 2 }),
      ],
    });
    const order = () => useAgentNodeStore.getState().agentNodes.map(n => n.id);

    it('moves a node forward and renumbers positions contiguously', async () => {
      seed();
      mockInvoke.mockResolvedValue(undefined);

      // Move node 1 to the end (insert at flat index 3).
      await useAgentNodeStore.getState().reorderAgentNode(1, 3);

      expect(order()).toEqual([2, 3, 1]);
      const positions = useAgentNodeStore.getState().agentNodes.map(n => n.position);
      expect(positions).toEqual([0, 1, 2]);
      // Persists the full new ordering for the mesh.
      expect(mockInvoke).toHaveBeenCalledWith('update_agent_node_positions', {
        updates: [[2, 0], [3, 1], [1, 2]],
      });
    });

    it('moves a node backward (insert before an earlier node)', async () => {
      seed();
      mockInvoke.mockResolvedValue(undefined);

      // Move node 3 to the front (insert at flat index 0).
      await useAgentNodeStore.getState().reorderAgentNode(3, 0);

      expect(order()).toEqual([3, 1, 2]);
    });

    it('is a no-op when the drop lands on the node’s own slot', async () => {
      seed();
      mockInvoke.mockResolvedValue(undefined);

      await useAgentNodeStore.getState().reorderAgentNode(2, 1); // already at index 1

      expect(order()).toEqual([1, 2, 3]);
      expect(mockInvoke).not.toHaveBeenCalled();
    });

    it('rolls back via a refetch when persistence fails', async () => {
      seed();
      const serverTruth = [makeNode({ id: 1, position: 0 }), makeNode({ id: 2, position: 1 }), makeNode({ id: 3, position: 2 })];
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'update_agent_node_positions') return Promise.reject(new Error('db locked'));
        if (cmd === 'list_agent_nodes') return Promise.resolve(serverTruth);
        return Promise.resolve(undefined);
      });

      await useAgentNodeStore.getState().reorderAgentNode(1, 3);

      // The optimistic move is discarded: we resync from the backend's truth
      // (the refetch clears the transient error as it reloads).
      expect(order()).toEqual([1, 2, 3]);
      expect(mockInvoke).toHaveBeenCalledWith('list_agent_nodes');
    });
  });

  describe('swapAgentNodes', () => {
    it('exchanges two nodes’ positions and persists just those two', async () => {
      useAgentNodeStore.setState({
        agentNodes: [
          makeNode({ id: 1, position: 0 }),
          makeNode({ id: 2, position: 1 }),
          makeNode({ id: 3, position: 2 }),
        ],
      });
      mockInvoke.mockResolvedValue(undefined);

      await useAgentNodeStore.getState().swapAgentNodes(1, 3);

      expect(useAgentNodeStore.getState().agentNodes.map(n => n.id)).toEqual([3, 2, 1]);
      expect(mockInvoke).toHaveBeenCalledWith('update_agent_node_positions', {
        updates: expect.arrayContaining([[1, 2], [3, 0]]),
      });
    });

    it('refuses to swap nodes that live in different meshes', async () => {
      useAgentNodeStore.setState({
        agentNodes: [
          makeNode({ id: 1, mesh_id: 1, position: 0 }),
          makeNode({ id: 2, mesh_id: 2, position: 0 }),
        ],
      });

      await useAgentNodeStore.getState().swapAgentNodes(1, 2);

      expect(mockInvoke).not.toHaveBeenCalled();
    });
  });
});
