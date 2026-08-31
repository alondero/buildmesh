import { describe, it, expect, beforeEach, vi } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import type { useOpenPr as UseOpenPr } from '../../src/hooks/useOpenPr';
import type { useGitSummary as UseGitSummary } from '../../src/hooks/useGitSummary';
import type { useAgentNodeStore as UseAgentNodeStore, AgentNode } from '../../src/stores/agentNodeStore';

// Issue #1004: the `+N ~M -K` and `PR #N` chips in `GridNodeHeader` go stale
// on a freshly-spawned node. Stage-1 of the spawn commits the DB row with
// `worktree_name` already set, so both hooks mount against a worktree
// directory that does not exist yet and cache an authoritative-looking
// `null`. `watch_agent_node` skips its leading-edge `git-changed` emit while
// the path is missing (`src-tauri/src/commands/file_watcher.rs`), and once a
// real event finally arrives the cache's `minRefetchIntervalMs` window (60s
// for the PR, 2s for the summary) suppresses it. The store now invalidates
// both caches on `node-spawn-completed` / `autopilot-pr-created`, which is
// exactly the moment the cached `null` becomes structurally wrong.
//
// Every assertion below deliberately runs WITHOUT advancing the clock, so a
// refetch can only happen if the freshness window was reset.

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

const PR = { number: 42, url: 'https://github.com/o/r/pull/42', title: 'wrap-up', draft: false };
const SUMMARY = { total: 4, added: 3, modified: 1, deleted: 0 };

// `getNodeGitPath` resolves a worktree node to `<path>/.claude/worktrees/<name>`.
const GIT_PATH = '/repo/.claude/worktrees/fresh-node';

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 7, mesh_id: 1, name: 'fresh-node', path: '/repo', branch: 'feat',
    env: 'linux', provider: 'anthropic', status: 'pending', created_at: '',
    use_worktree: true, worktree_name: 'fresh-node', position: 0,
    ...overrides,
  } as AgentNode;
}

// The hooks keep module-level cache clients and the store keeps a once-only
// `listenersAttached` closure guard. Re-import the whole trio per test so the
// store's listeners are wired to the same fresh cache clients the hooks use.
let useOpenPr: typeof UseOpenPr;
let useGitSummary: typeof UseGitSummary;
let useAgentNodeStore: typeof UseAgentNodeStore;

beforeEach(async () => {
  vi.resetModules();
  ({ useOpenPr } = await import('../../src/hooks/useOpenPr'));
  ({ useGitSummary } = await import('../../src/hooks/useGitSummary'));
  ({ useAgentNodeStore } = await import('../../src/stores/agentNodeStore'));
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(null);
});

/** Seeds the node into the store and wires the event listeners. The
 *  `vi.resetModules()` re-import above means `useAgentNodeStore` is a
 *  fresh module instance — issue #1384 normalised state lives there,
 *  so we seed via the fresh instance directly. The shared
 *  `seedAgentNodes` helper holds a reference to the original (pre-reset)
 *  module, which would write to the wrong store. */
async function attachListeners(node: AgentNode = makeNode()) {
  useAgentNodeStore.setState({
    nodesById: { [node.id]: node },
    nodeIds: [node.id],
  });
  await useAgentNodeStore.getState().initAttentionListeners();
}

describe('node cache invalidation (issue #1004)', () => {
  it('refetches the open PR on node-spawn-completed, inside the 60s freshness window', async () => {
    // Mount-time fetch: the worktree does not exist yet, so the backend
    // reports no PR and the cache stamps that `null` as fresh.
    mockInvoke.mockResolvedValue(null);
    const { result } = renderHook(() => useOpenPr(7, GIT_PATH));
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('get_open_pr_for_node', { nodeId: 7 }));
    expect(result.current.pr).toBeNull();

    await attachListeners();

    // Stage-2 finished: the worktree exists and the branch now has a PR.
    mockInvoke.mockResolvedValue(PR);
    await act(async () => {
      await emit('node-spawn-completed', { node_id: 7 });
    });

    await waitFor(() => expect(result.current.pr).toEqual(PR));
  });

  it('refetches the git summary on node-spawn-completed, inside the 2s freshness window', async () => {
    mockInvoke.mockResolvedValue({ total: 0, added: 0, modified: 0, deleted: 0 });
    const { result } = renderHook(() => useGitSummary(GIT_PATH));
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('get_git_summary', { path: GIT_PATH }));
    expect(result.current.summary).toBeNull();

    await attachListeners();

    mockInvoke.mockResolvedValue(SUMMARY);
    await act(async () => {
      await emit('node-spawn-completed', { node_id: 7 });
    });

    await waitFor(() => expect(result.current.summary).toEqual(SUMMARY));
  });

  it('refetches the open PR on autopilot-pr-created (the v1.1 TODO in useOpenPr)', async () => {
    mockInvoke.mockResolvedValue(null);
    const { result } = renderHook(() => useOpenPr(7, GIT_PATH));
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('get_open_pr_for_node', { nodeId: 7 }));
    expect(result.current.pr).toBeNull();

    await attachListeners(makeNode({ status: 'running' }));

    // The autopilot wrap-up just opened the PR.
    mockInvoke.mockResolvedValue(PR);
    await act(async () => {
      await emit('autopilot-pr-created', { node_id: 7, pr_url: PR.url });
    });

    await waitFor(() => expect(result.current.pr).toEqual(PR));
  });

  it('leaves other nodes alone when one node finishes spawning', async () => {
    mockInvoke.mockResolvedValue(null);
    const other = renderHook(() => useOpenPr(8, '/repo/.claude/worktrees/other-node'));
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('get_open_pr_for_node', { nodeId: 8 }));

    await attachListeners();

    mockInvoke.mockResolvedValue(PR);
    const callsBefore = mockInvoke.mock.calls.length;
    await act(async () => {
      await emit('node-spawn-completed', { node_id: 7 });
    });

    expect(mockInvoke.mock.calls.length).toBe(callsBefore);
    expect(other.result.current.pr).toBeNull();
  });

  it('is a no-op when the completed node is not in the store', async () => {
    mockInvoke.mockResolvedValue(null);
    const { result } = renderHook(() => useOpenPr(7, GIT_PATH));
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('get_open_pr_for_node', { nodeId: 7 }));

    useAgentNodeStore.setState({ nodesById: {}, nodeIds: []});
    await useAgentNodeStore.getState().initAttentionListeners();

    mockInvoke.mockResolvedValue(PR);
    const callsBefore = mockInvoke.mock.calls.length;
    await act(async () => {
      await emit('node-spawn-completed', { node_id: 7 });
    });

    expect(mockInvoke.mock.calls.length).toBe(callsBefore);
    expect(result.current.pr).toBeNull();
  });
});
