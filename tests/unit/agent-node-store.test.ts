import { describe, it, expect, beforeEach, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';

const mockInvoke = invoke as ReturnType<typeof vi.fn>;
const mockEmit = emit as ReturnType<typeof vi.fn>;

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 1, mesh_id: 1, name: 'bold-keen-brook', path: '/a', branch: 'main',
    env: 'windows', provider: 'anthropic', status: 'idle', created_at: '',
    ...overrides,
  };
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
    it('removes node from local state', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 15 })], activeNodeId: 15 });
      mockInvoke.mockResolvedValue(undefined);

      await useAgentNodeStore.getState().deleteAgentNode(15);

      expect(useAgentNodeStore.getState().agentNodes).toHaveLength(0);
      expect(useAgentNodeStore.getState().activeNodeId).toBeNull();
    });

    it('clears activeNodeId only if deleted node was active', async () => {
      useAgentNodeStore.setState({
        agentNodes: [makeNode({ id: 1 }), makeNode({ id: 2, path: '/b' })],
        activeNodeId: 2,
      });
      mockInvoke.mockResolvedValue(undefined);

      await useAgentNodeStore.getState().deleteAgentNode(1);

      expect(useAgentNodeStore.getState().activeNodeId).toBe(2);
      expect(useAgentNodeStore.getState().agentNodes).toHaveLength(1);
    });
  });
});
