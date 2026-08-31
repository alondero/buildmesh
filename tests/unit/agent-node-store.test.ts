import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import {
  setWorktreeCloseActionResolverForTests,
  useAgentNodeStore,
  type AgentNode,
} from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useWorktreeClosePromptStore } from '../../src/stores/worktreeClosePromptStore';
import type { WorktreeCloseSafety } from '../../src/lib/worktreeClose';

// Issue #647: `agentNodeStore.deleteAgentNode` disposes the xterm terminal
// BEFORE the `delete_agent_node` IPC commits. On failure the restored row
// re-mounts an xterm bound to a dead PTY (the agent was already killed by
// the warn-only `kill_agent` path), so the user sees a blank terminal +
// a dead agent and has to click × again. Mock the dispose seam here so the
// test can pin both:
//   - "dispose runs AFTER delete_agent_node resolves successfully"
//   - "dispose does NOT run if delete_agent_node rejects" (terminal stays
//      live so the restored row can re-attach with its scrollback).
// `Terminal.tsx` is a React component, so we mock it as a module — only
// `disposeTerminal` is exercised here.
vi.mock('../../src/components/Terminal/Terminal', () => ({
  disposeTerminal: vi.fn(),
  AgentTerminal: () => null,
}));

// Issue #1001 — `deleteAgentNode` Phase 2 (delete_commit) now surfaces
// its failure via the shared `addToast` wrapper from `stores/toastStore`
// instead of writing to `state.error` (which would produce a duplicate
// 'System' toast alongside the explicit 'Node' toast). Mock the wrapper
// the same way `Terminal.tsx` is mocked above — production code imports
// the named export, tests capture the spy via `vi.mocked(addToast)`.
const { addToastMock } = vi.hoisted(() => ({
  addToastMock: vi.fn(),
}));
vi.mock('../../src/stores/toastStore', () => ({
  addToast: addToastMock,
  // `dismissToast` is exported by the module but isn't exercised by this
  // test file; pass through to keep the module shape intact.
  dismissToast: vi.fn(),
}));

import { disposeTerminal } from '../../src/components/Terminal/Terminal';
import { addToast } from '../../src/stores/toastStore';

const mockDisposeTerminal = disposeTerminal as ReturnType<typeof vi.fn>;
const mockAddToast = vi.mocked(addToast);

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
      schedules: {},
      pendingPrefills: {},
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
    // The #644 regression test drives the real prompt-store resolver; a
    // leftover pending from a sibling test would silently settle an
    // unrelated promise as 'cancel' on the first request() call.
    useWorktreeClosePromptStore.setState({ pending: null });
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

    it('stages a non-empty initial prompt for the subsequent auto-spawn (issue #1413)', async () => {
      const newNode = {
        id: 42, mesh_id: 7, name: 'm', path: '/p', branch: 'main',
        env: 'windows', provider: 'anthropic', status: 'idle', created_at: '',
        use_worktree: true, position: 0,
      };
      mockInvoke.mockResolvedValueOnce(newNode);

      await useAgentNodeStore.getState().selectProviderForMesh(
        7, 'm', '/p', 'anthropic', undefined, '  fix the flaky test  ',
      );

      expect(useAgentNodeStore.getState().pendingPrefills[42]).toBe('fix the flaky test');
    });

    it('does not stage a whitespace-only initial prompt', async () => {
      const newNode = {
        id: 44, mesh_id: 7, name: 'm', path: '/p', branch: 'main',
        env: 'windows', provider: 'anthropic', status: 'idle', created_at: '',
        use_worktree: true, position: 0,
      };
      mockInvoke.mockResolvedValueOnce(newNode);

      await useAgentNodeStore.getState().selectProviderForMesh(
        7, 'm', '/p', 'anthropic', undefined, '   ',
      );

      expect(useAgentNodeStore.getState().pendingPrefills[44]).toBeUndefined();
    });
  });

  describe('spawnAgent pending prefill (issue #1413)', () => {
    it('forwards a staged prompt to spawn_agent and consumes it', async () => {
      const existing = makeNode({ id: 42, provider: 'anthropic', status: 'idle' });
      useAgentNodeStore.setState({
        agentNodes: [existing],
        pendingPrefills: { 42: 'fix the flaky test' },
      });
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'spawn_agent') return Promise.resolve(undefined);
        if (cmd === 'list_agent_nodes') return Promise.resolve([existing]);
        if (cmd === 'list_autopilot_runs') return Promise.resolve([]);
        return Promise.resolve(undefined);
      });

      await useAgentNodeStore.getState().spawnAgent(42, 'anthropic', 24, 80);

      expect(mockInvoke).toHaveBeenCalledWith('spawn_agent', expect.objectContaining({
        sessionId: 42,
        provider: 'anthropic',
        prefill: 'fix the flaky test',
      }));
      expect(useAgentNodeStore.getState().pendingPrefills[42]).toBeUndefined();
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

// Issue #645: when `delete_agent_node` rejects, the optimistic removal must
    // not be allowed to silently hide the failure. Three invariants:
    //   1. The row is restored to `agentNodes` so UI/DB stay in sync — otherwise
    //      the next fetchAgentNodes (any unrelated trigger — node-created,
    //      mesh switch, …) re-fetches from the DB and the row "zombie"
    //      reappears, looking like the close silently failed. Pin the row
    //      back BEFORE the next refetch.
    //   2. The error reaches the shared toast pipeline (issue #1001).
    //      `addToast('Node', \`Failed to close node: ${formatError(e)}\`, 'error')`
    //      is the user-visible feedback path. Phase 2 no longer writes
    //      `state.error` because that would produce a duplicate 'System'
    //      toast alongside the explicit 'Node' toast (different providers,
    //      dedup wouldn't collapse them, two of three slots burned on one
    //      failure). Phase 1 (worktree safety check) still writes
    //      `state.error` — App.tsx's System toast pipeline handles it.
    //   3. The promise rejects, so any caller wanting to react (catch block,
    //      await rejection) can. Matches `createAgentNode` / `renameAgentNode`
    //      which already re-throw.
    it('restores the node on delete_agent_node failure (issue #645 zombie-row fix)', async () => {
      const node = makeNode({ id: 41 });
      useAgentNodeStore.setState({ agentNodes: [node], activeNodeId: 41 });
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_worktree_close_safety') return Promise.resolve(makeSafety());
        if (cmd === 'delete_agent_node') return Promise.reject(new Error('db locked'));
        return Promise.resolve(undefined);
      });

      await expect(
        useAgentNodeStore.getState().deleteAgentNode(41)
      ).rejects.toThrow('db locked');

      // The row is back — UI matches the DB (which still holds the row).
      const restored = useAgentNodeStore.getState().agentNodes;
      expect(restored).toHaveLength(1);
      expect(restored[0]).toEqual(node);
    });

    it('surfaces the delete_agent_node rejection through the shared toast pipeline (issue #1001)', async () => {
      const node = makeNode({ id: 42 });
      useAgentNodeStore.setState({ agentNodes: [node], activeNodeId: 42 });
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_worktree_close_safety') return Promise.resolve(makeSafety());
        if (cmd === 'delete_agent_node') return Promise.reject(new Error('foreign key violation'));
        return Promise.resolve(undefined);
      });

      await expect(
        useAgentNodeStore.getState().deleteAgentNode(42)
      ).rejects.toThrow('foreign key');

      // Issue #1001 — the failure reaches the shared toast pipeline with
      // the explicit 'Node' provider, the formatted 'Failed to close node:
      // ...' message, and the 'error' severity. The toast itself is
      // rendered by App.tsx; this test pins the wrapper was called with
      // the right args. `state.error` is NOT written here — keeping it
      // would produce a duplicate 'System' toast alongside the 'Node'
      // one (different providers → not deduped).
      expect(mockAddToast).toHaveBeenCalledWith(
        'Node',
        'Failed to close node: foreign key violation',
        'error',
      );
    });

    it('rejects the outer promise so callers can react to a delete_agent_node failure', async () => {
      const node = makeNode({ id: 43 });
      useAgentNodeStore.setState({ agentNodes: [node], activeNodeId: 43 });
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_worktree_close_safety') return Promise.resolve(makeSafety());
        if (cmd === 'delete_agent_node') return Promise.reject(new Error('ipc disconnected'));
        return Promise.resolve(undefined);
      });

      // Pre-fix: the catch swallowed the rejection, so `await …` resolved
      // successfully and the UI thought the delete succeeded. Post-fix: the
      // store re-throws, matching createAgentNode / renameAgentNode.
      let didThrow = false;
      try {
        await useAgentNodeStore.getState().deleteAgentNode(43);
      } catch {
        didThrow = true;
      }
      expect(didThrow).toBe(true);
    });

    // Belt-and-braces: the kill_agent rejection path must still surface
    // errors. kill_agent is best-effort (warn-only), but a delete_agent_node
    // failure that follows it must not regress when we add the rollback.
    it('restores the row even when kill_agent rejects first', async () => {
      const node = makeNode({ id: 44 });
      useAgentNodeStore.setState({ agentNodes: [node], activeNodeId: 44 });
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_worktree_close_safety') return Promise.resolve(makeSafety());
        if (cmd === 'kill_agent') return Promise.reject(new Error('status update failed'));
        if (cmd === 'delete_agent_node') return Promise.reject(new Error('db locked'));
        return Promise.resolve(undefined);
      });

      await expect(
        useAgentNodeStore.getState().deleteAgentNode(44)
      ).rejects.toThrow('db locked');

      expect(useAgentNodeStore.getState().agentNodes).toEqual([node]);
    });

    // Issue #647: `disposeTerminal(id)` ran BEFORE `delete_agent_node` was
    // awaited, so on rejection the restored row re-mounted an xterm bound to
    // a dead PTY (the agent was already killed by the warn-only `kill_agent`
    // path). The terminal must outlive a failed delete so the user can
    // retry the close without losing scrollback. The fix moves
    // `disposeTerminal(id)` to AFTER the success path; on rejection it
    // must NOT run at all.
    it('does NOT dispose the terminal when delete_agent_node rejects (#647)', async () => {
      const node = makeNode({ id: 45 });
      useAgentNodeStore.setState({ agentNodes: [node], activeNodeId: 45 });
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_worktree_close_safety') return Promise.resolve(makeSafety());
        if (cmd === 'delete_agent_node') return Promise.reject(new Error('db locked'));
        return Promise.resolve(undefined);
      });

      await expect(
        useAgentNodeStore.getState().deleteAgentNode(45)
      ).rejects.toThrow('db locked');

      // The row is restored (issue #645 invariant) AND the terminal lives —
      // so a re-mount sees its scrollback + a usable PTY, not a blank
      // terminal bound to a dead agent.
      expect(useAgentNodeStore.getState().agentNodes).toEqual([node]);
      expect(mockDisposeTerminal).not.toHaveBeenCalled();
    });

    it('disposes the terminal AFTER delete_agent_node resolves successfully (#647)', async () => {
      // On the happy path the row IS gone for good, so the terminal must
      // still dispose — just after the IPC commits rather than before. Use
      // a controlled `delete_agent_node` promise so we can observe the
      // ordering against the store's optimistic remove + the IPC call.
      const node = makeNode({ id: 46 });
      useAgentNodeStore.setState({ agentNodes: [node], activeNodeId: 46 });
      const callOrder: string[] = [];
      let resolveDelete!: () => void;
      const deletePending = new Promise<void>((r) => { resolveDelete = r; });
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_worktree_close_safety') {
          return Promise.resolve(makeSafety());
        }
        if (cmd === 'kill_agent') {
          return Promise.resolve(undefined);
        }
        if (cmd === 'delete_agent_node') {
          callOrder.push('delete_agent_node:invoked');
          return deletePending.then(() => {
            callOrder.push('delete_agent_node:resolved');
          });
        }
        return Promise.resolve(undefined);
      });
      mockDisposeTerminal.mockImplementation(() => {
        callOrder.push('disposeTerminal');
      });

      const closePromise = useAgentNodeStore.getState().deleteAgentNode(46);

      // Optimistic remove happens before the IPC resolves (already covered
      // by the prior 'removes the node from the UI before the worktree
      // cleanup resolves' test), but the terminal must NOT be disposed yet —
      // the IPC has been invoked but not resolved.
      await vi.waitFor(() => {
        expect(useAgentNodeStore.getState().agentNodes).toHaveLength(0);
      });
      expect(mockDisposeTerminal).not.toHaveBeenCalled();

      resolveDelete();
      await closePromise;

      // After success: terminal is disposed, AND it ran strictly AFTER the
      // IPC resolved (not before the optimistic remove, which is the old
      // ordering the bug report calls out).
      expect(mockDisposeTerminal).toHaveBeenCalledTimes(1);
      expect(mockDisposeTerminal).toHaveBeenCalledWith(46);
      const disposeIdx = callOrder.indexOf('disposeTerminal');
      const resolveIdx = callOrder.indexOf('delete_agent_node:resolved');
      expect(disposeIdx).toBeGreaterThan(resolveIdx);
    });

    // Regression for #644 — pins the user-visible "stuck row" symptom.
    // Contract is owned by worktree-close-prompt-store.test.ts. Uses ids
    // 50/51 to avoid colliding with the #645 zombie-row tests (41-44).
    it('clears the prior closing flag when a second close supersedes the first prompt', async () => {
      const nodeA = makeNode({ id: 50, name: 'alpha' });
      const nodeB = makeNode({ id: 51, name: 'beta' });
      useAgentNodeStore.setState({ agentNodes: [nodeA, nodeB], activeNodeId: 50 });
      mockDeleteFlow(makeSafety({ has_unpushed: true }));
      // Use the real worktreeClosePromptStore resolver so this exercises the
      // actual orphan-promise code path.
      setWorktreeCloseActionResolverForTests();

      const closeA = useAgentNodeStore.getState().deleteAgentNode(50);
      await vi.waitFor(() => {
        expect(useAgentNodeStore.getState().closingNodeIds.has(50)).toBe(true);
      });

      // Before the fix: this would overwrite pending and orphan A's resolver.
      const closeB = useAgentNodeStore.getState().deleteAgentNode(51);

      // The promise returned to A must settle (as 'cancel'), allowing its
      // deleteAgentNode to clear closingNodeIds(50). If this never resolves
      // the test fails by timeout — the exact symptom of the bug.
      await closeA;

      expect(useAgentNodeStore.getState().closingNodeIds.has(50)).toBe(false);
      // A is still in the list because 'cancel' is a no-op for the actual
      // delete — only the closing flag clears.
      expect(useAgentNodeStore.getState().agentNodes.find(n => n.id === 50)).toBeDefined();

      // B's prompt is the one currently displayed. Dismiss it so closeB can
      // also settle — proving the dialog still works for the second node.
      useWorktreeClosePromptStore.getState().choose('cancel');
      await closeB;
      expect(useAgentNodeStore.getState().closingNodeIds.has(51)).toBe(false);
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

  describe('setNodePinned / toggleNodePinned (wayfinder #982 / #984)', () => {
    it('setNodePinned pins optimistically and adopts the backend-returned node', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 21, is_pinned: false, name: 'local-name' })] });
      // The backend's returned AgentNode is the source of truth — a
      // concurrent column change on the backend side must survive.
      mockInvoke.mockResolvedValueOnce(makeNode({ id: 21, is_pinned: true, name: 'backend-name' }));

      await useAgentNodeStore.getState().setNodePinned(21, true);

      expect(mockInvoke).toHaveBeenCalledWith('set_node_pinned', { nodeId: 21, pinned: true });
      const node = useAgentNodeStore.getState().agentNodes.find(n => n.id === 21)!;
      expect(node.is_pinned).toBe(true);
      expect(node.name).toBe('backend-name');
    });

    it('toggleNodePinned flips optimistically so the Pinned grid re-renders instantly', () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 22, is_pinned: false })] });
      // A never-settling IPC lets us observe the optimistic window directly.
      // (Left pending on purpose — an unresolved promise holds no timers and
      // can't outlive the test.)
      mockInvoke.mockReturnValueOnce(new Promise(() => {}));

      void useAgentNodeStore.getState().toggleNodePinned(22);

      expect(useAgentNodeStore.getState().agentNodes.find(n => n.id === 22)!.is_pinned).toBe(true);
      expect(mockInvoke).toHaveBeenCalledWith('toggle_node_pinned', { nodeId: 22 });
    });

    it('toggleNodePinned adopts the backend-returned node on success', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 22, is_pinned: true })] });
      mockInvoke.mockResolvedValueOnce(makeNode({ id: 22, is_pinned: false }));

      await useAgentNodeStore.getState().toggleNodePinned(22);

      expect(useAgentNodeStore.getState().agentNodes.find(n => n.id === 22)!.is_pinned).toBe(false);
    });

    it('toggleNodePinned rolls back ONLY is_pinned on rejection, preserving concurrent writes', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 23, is_pinned: false, status: 'idle' })] });
      mockInvoke.mockRejectedValueOnce(new Error('db locked'));

      const promise = useAgentNodeStore.getState().toggleNodePinned(23);
      // A concurrent write to another column (e.g. an attention status
      // flip from the orchestrator) lands while the IPC is in flight.
      useAgentNodeStore.setState(s => ({
        agentNodes: s.agentNodes.map(n => n.id === 23 ? { ...n, status: 'awaiting_input' as const } : n),
      }));

      await expect(promise).rejects.toThrow('db locked');

      const node = useAgentNodeStore.getState().agentNodes.find(n => n.id === 23)!;
      expect(node.is_pinned).toBe(false); // rolled back
      expect(node.status).toBe('awaiting_input'); // concurrent write preserved
      expect(useAgentNodeStore.getState().error).toContain('db locked');
    });

    it('setNodePinned rolls back and re-throws on rejection', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 24, is_pinned: true })] });
      mockInvoke.mockRejectedValueOnce(new Error('node not found'));

      await expect(
        useAgentNodeStore.getState().setNodePinned(24, false)
      ).rejects.toThrow('node not found');

      expect(useAgentNodeStore.getState().agentNodes.find(n => n.id === 24)!.is_pinned).toBe(true);
      expect(useAgentNodeStore.getState().error).toContain('node not found');
    });

    it('throws before any IPC when the node is not loaded', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 25 })] });

      await expect(useAgentNodeStore.getState().toggleNodePinned(999)).rejects.toThrow('not loaded');
      expect(mockInvoke).not.toHaveBeenCalled();
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

  describe('scheduleInput / cancelSchedule', () => {
    beforeEach(() => {
      vi.useFakeTimers();
    });

    afterEach(() => {
      vi.useRealTimers();
    });

    it('stores schedule metadata with a computed targetTime', () => {
      const now = Date.now();
      useAgentNodeStore.getState().scheduleInput(1, 300_000, 'still there?', '5m');

      const task = useAgentNodeStore.getState().schedules[1];
      expect(task).toMatchObject({ nodeId: 1, message: 'still there?', label: '5m' });
      expect(task.targetTime).toBeGreaterThanOrEqual(now + 300_000);
      expect(task.timeoutId).toBeDefined();
    });

    it('sends the message and clears the schedule when the timeout fires', async () => {
      // The scheduled node must be loaded — issue #1252's fire-handler
      // guard bails if the node is missing or archived. The real
      // SchedulingPopover only ever schedules against a node already
      // on screen, so this is the realistic shape.
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 2, status: 'running' })] });
      mockInvoke.mockResolvedValue(undefined);
      useAgentNodeStore.getState().scheduleInput(2, 1000, 'ping', '1m');

      await vi.advanceTimersByTimeAsync(1000);

      expect(mockInvoke).toHaveBeenCalledWith('send_to_agent', { sessionId: 2, input: 'ping' });
      expect(useAgentNodeStore.getState().schedules[2]).toBeUndefined();
    });

    it('writes a bare Enter instead of sendToAgent when message is empty', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 3, status: 'running' })] });
      mockInvoke.mockResolvedValue(undefined);
      useAgentNodeStore.getState().scheduleInput(3, 1000, '', 'usage_reset');

      await vi.advanceTimersByTimeAsync(1000);

      expect(mockInvoke).toHaveBeenCalledWith('write_to_agent', { sessionId: 3, data: '\n' });
    });

    it('replaces an existing schedule for the same node instead of stacking', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 4, status: 'running' })] });
      mockInvoke.mockResolvedValue(undefined);
      useAgentNodeStore.getState().scheduleInput(4, 1000, 'first', '1m');
      useAgentNodeStore.getState().scheduleInput(4, 2000, 'second', '2m');

      await vi.advanceTimersByTimeAsync(1000);
      // The first schedule was cancelled — nothing fires at its original delay.
      expect(mockInvoke).not.toHaveBeenCalledWith('send_to_agent', { sessionId: 4, input: 'first' });

      await vi.advanceTimersByTimeAsync(1000);
      expect(mockInvoke).toHaveBeenCalledWith('send_to_agent', { sessionId: 4, input: 'second' });
    });

    it('cancelSchedule clears the timeout and removes the entry', async () => {
      mockInvoke.mockResolvedValue(undefined);
      useAgentNodeStore.getState().scheduleInput(5, 1000, 'never sent', '1m');

      useAgentNodeStore.getState().cancelSchedule(5);
      expect(useAgentNodeStore.getState().schedules[5]).toBeUndefined();

      await vi.advanceTimersByTimeAsync(1000);
      expect(mockInvoke).not.toHaveBeenCalledWith('send_to_agent', { sessionId: 5, input: 'never sent' });
    });

    it('cancelSchedule is a no-op when there is no active schedule for the node', () => {
      expect(() => useAgentNodeStore.getState().cancelSchedule(999)).not.toThrow();
    });

    it('deleteAgentNode cancels an active schedule for the node being deleted', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 60 })], activeNodeId: 60 });
      mockDeleteFlow(makeSafety());
      useAgentNodeStore.getState().scheduleInput(60, 1000, 'ping', '1m');

      await useAgentNodeStore.getState().deleteAgentNode(60);

      expect(useAgentNodeStore.getState().schedules[60]).toBeUndefined();
      mockInvoke.mockClear();
      await vi.advanceTimersByTimeAsync(1000);
      expect(mockInvoke).not.toHaveBeenCalledWith('send_to_agent', expect.anything());
    });

    // Issue #1252 — a scheduled prompt must not fire at an archived (or
    // absent) node. Without the fetch-time cancellation the stale timer
    // would call `send_to_agent` against an archived node, the backend
    // would reject, `state.error` would be set, and App.tsx's generic
    // "System" toast pipeline would surface a spurious error minutes
    // after the user moved on. The `autopilot-node-closed` listener
    // (`stores/agentNodeListeners.ts`) routes through `fetchAgentNodes`,
    // so every archive transition sweeps schedules for free.
    it('cancels schedules whose node comes back archived from fetchAgentNodes', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 7, status: 'running' })] });
      useAgentNodeStore.getState().scheduleInput(7, 1000, 'still there?', '5m');
      // The next list_agent_nodes reflects node 7 as archived — the
      // shape the autopilot-node-closed handler triggers via
      // fetchAgentNodes.
      mockInvoke.mockResolvedValueOnce([makeNode({ id: 7, status: 'archived' })]);

      await useAgentNodeStore.getState().fetchAgentNodes();

      // Schedule cleared — the timer is dead and can't fire at the
      // archived node.
      expect(useAgentNodeStore.getState().schedules[7]).toBeUndefined();
      mockInvoke.mockClear();
      await vi.advanceTimersByTimeAsync(1000);
      expect(mockInvoke).not.toHaveBeenCalledWith('send_to_agent', expect.anything());
    });

    it('cancels schedules whose node is absent from fetchAgentNodes', async () => {
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 7, status: 'running' })] });
      useAgentNodeStore.getState().scheduleInput(7, 1000, 'still there?', '5m');
      // The next list_agent_nodes returns a different mesh's nodes —
      // node 7 has been deleted out from under the schedule.
      mockInvoke.mockResolvedValueOnce([makeNode({ id: 99 })]);

      await useAgentNodeStore.getState().fetchAgentNodes();

      expect(useAgentNodeStore.getState().schedules[7]).toBeUndefined();
      mockInvoke.mockClear();
      await vi.advanceTimersByTimeAsync(1000);
      expect(mockInvoke).not.toHaveBeenCalledWith('send_to_agent', expect.anything());
    });

    it('schedule-fire handler bails silently when the node is archived in state at fire time', async () => {
      // Belt-and-braces — even if the archive reaches state AFTER
      // scheduleInput was called (e.g. between the timer being set
      // and firing), the fire handler must re-check the node's
      // presence + status before sending. The cancelSchedule path
      // handles the common case; this guard covers the narrow race
      // where a refetch between scheduling and firing hasn't yet
      // completed when the timer resolves.
      useAgentNodeStore.setState({ agentNodes: [makeNode({ id: 8, status: 'running' })] });
      useAgentNodeStore.getState().scheduleInput(8, 1000, 'ping', '1m');
      // Node 8 transitions to archived before the timer fires (e.g.
      // an autopilot close landed between the schedule and the tick).
      useAgentNodeStore.setState({
        agentNodes: [makeNode({ id: 8, status: 'archived' })],
      });

      await vi.advanceTimersByTimeAsync(1000);

      expect(mockInvoke).not.toHaveBeenCalledWith('send_to_agent', expect.anything());
      // And the schedule entry is cleared so the (no-op) timer doesn't
      // surface a stale "scheduled" chip in the UI.
      expect(useAgentNodeStore.getState().schedules[8]).toBeUndefined();
    });
  });
});
