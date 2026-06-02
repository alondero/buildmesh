import { describe, it, expect, beforeEach, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import {
  setWorktreeCloseActionResolverForTests,
  useAgentNodeStore,
  type AgentNode,
} from '../../src/stores/agentNodeStore';
import type { WorktreeCloseSafety } from '../../src/lib/worktreeClose';

const mockInvoke = invoke as ReturnType<typeof vi.fn>;
const mockEmit = emit as ReturnType<typeof vi.fn>;

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 1, mesh_id: 1, name: 'bold-keen-brook', path: '/a', branch: 'main',
    env: 'windows', provider: 'anthropic', status: 'idle', created_at: '',
    use_worktree: true,
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
      checkpoints: [],
      loading: false,
      error: null,
    });
    vi.clearAllMocks();
    setWorktreeCloseActionResolverForTests();
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

      await mockEmit('session-renamed', { session_id: 10, name: 'fix-auth-flow' });
      expect(useAgentNodeStore.getState().agentNodes.find(n => n.id === 10)?.name).toBe('fix-auth-flow');

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

  describe('setActiveNode', () => {
    it('fetches checkpoints when setting active node', async () => {
      const checkpoints = [{ id: 1, node_id: 5, git_ref: 'abc', turn_index: 1, message: '', created_at: '' }];
      mockInvoke.mockResolvedValueOnce(checkpoints);

      await useAgentNodeStore.getState().setActiveNode(5);

      expect(useAgentNodeStore.getState().activeNodeId).toBe(5);
      expect(useAgentNodeStore.getState().checkpoints).toEqual(checkpoints);
    });

    it('clears checkpoints when setting null', async () => {
      useAgentNodeStore.setState({ checkpoints: [{ id: 1, node_id: 5, git_ref: '', turn_index: 0, message: '', created_at: '' }] });

      await useAgentNodeStore.getState().setActiveNode(null);

      expect(useAgentNodeStore.getState().activeNodeId).toBeNull();
      expect(useAgentNodeStore.getState().checkpoints).toEqual([]);
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
      expect(mockInvoke).toHaveBeenCalledWith('get_worktree_close_safety', { sessionId: 15 });
      expect(mockInvoke).toHaveBeenCalledWith('kill_agent', { sessionId: 15 });
      expect(mockInvoke).toHaveBeenCalledWith('delete_session', {
        sessionId: 15,
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
      expect(mockInvoke).toHaveBeenCalledWith('delete_session', {
        sessionId: 7,
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
      expect(mockInvoke).toHaveBeenCalledWith('delete_session', {
        sessionId: 8,
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
      expect(mockInvoke).toHaveBeenCalledWith('get_worktree_close_safety', { sessionId: 9 });
      expect(useAgentNodeStore.getState().agentNodes).toEqual([node]);
      expect(useAgentNodeStore.getState().activeNodeId).toBe(9);
    });

    it('removes the node from the UI before the worktree cleanup resolves', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 15 })], activeNodeId: 15 });
      let resolveDelete!: () => void;
      const deletePending = new Promise<void>((r) => { resolveDelete = r; });
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_worktree_close_safety') return Promise.resolve(makeSafety());
        if (cmd === 'delete_session') return deletePending;
        return Promise.resolve(undefined);
      });

      const closePromise = useAgentNodeStore.getState().deleteAgentNode(15);

      // The node must be gone optimistically while delete_session is still pending.
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
      expect(mockInvoke).toHaveBeenCalledWith('delete_session', {
        sessionId: 22,
        removeWorktree: true,
      });
      expect(useAgentNodeStore.getState().agentNodes).toHaveLength(0);
    });

    it('does not request worktree removal when the node has no worktree path', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 10, worktree_name: undefined })], activeNodeId: 10 });
      mockDeleteFlow(makeSafety({ worktree_path: null }));

      await useAgentNodeStore.getState().deleteAgentNode(10);

      expect(mockInvoke).toHaveBeenCalledWith('delete_session', {
        sessionId: 10,
        removeWorktree: false,
      });
    });
  });
});
