import { describe, it, expect, beforeEach } from 'vitest';
import { nextAwaitingNodeId, jumpToNextAwaitingNode } from '../../src/lib/awaitingInputShortcuts';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { SessionStatus } from '../../src/types/generated/SessionStatus';

/**
 * Tests for the Ctrl/Cmd+. cycle logic (issue #64). The cycle is a pure
 * store-mutator seam (src/lib/awaitingInputShortcuts.ts) — same shape as
 * `toggleGridMaximize` in src/lib/gridShortcuts.ts — so we exercise it by
 * seeding the agentNodeStore and asserting on `activeNodeId` after each
 * call. No DOM, no IPC, no global-shortcut plugin: the App.tsx wiring is
 * pinned separately by `tests/unit/shortcut-catalog-binding.test.ts`.
 */

// Minimal-but-valid AgentNode factory. Mirrors the precedent in
// `tests/unit/grid-shortcuts.test.ts`. Spread defaults keep new fields from
// breaking existing tests — only the fields each test cares about need
// overriding.
function makeNode(overrides: Partial<AgentNode> & Pick<AgentNode, 'id' | 'mesh_id' | 'position'>): AgentNode {
  return {
    name: `agent-${overrides.id}`,
    path: '/repo',
    branch: 'main',
    env: 'wsl',
    provider: 'claude',
    status: 'running' as SessionStatus,
    use_worktree: false,
    created_at: '2026-07-19T00:00:00Z',
    scratchpad: '',
    sandbox: false,
    cli_session_id: null,
    worktree_name: null,
    source_issue: null,
    archived: false,
    head_repo_owner: null,
    head_repo_clone_url: null,
    source_pr_pinned_sha: null,
    ...overrides,
  };
}

describe('awaitingInputShortcuts — nextAwaitingNodeId (issue #64)', () => {
  beforeEach(() => {
    useAgentNodeStore.setState({ agentNodes: [], activeNodeId: null });
  });

  it('returns null when there is no active node', () => {
    // No-op case: the user hasn't selected a mesh/node yet, so we have no
    // scope to search within. Matches the arrow-traversal handler's policy.
    useAgentNodeStore.setState({
      agentNodes: [
        makeNode({ id: 1, mesh_id: 1, position: 0, status: 'awaiting_input' }),
        makeNode({ id: 2, mesh_id: 1, position: 1, status: 'awaiting_input' }),
      ],
      activeNodeId: null,
    });
    expect(nextAwaitingNodeId()).toBeNull();
  });

  it('returns null when no nodes are awaiting input', () => {
    // Acceptance criterion: "No-op when no nodes are awaiting input".
    useAgentNodeStore.setState({
      agentNodes: [
        makeNode({ id: 1, mesh_id: 1, position: 0, status: 'running' }),
        makeNode({ id: 2, mesh_id: 1, position: 1, status: 'idle' }),
        makeNode({ id: 3, mesh_id: 1, position: 2, status: 'error' }),
      ],
      activeNodeId: 2,
    });
    expect(nextAwaitingNodeId()).toBeNull();
  });

  it('returns the active node itself when it is the only awaiting one', () => {
    // Edge: mesh has exactly one awaiting node and it's the active one.
    // Cycle moves forward, wraps, and lands back on the same id. This is
    // intentional — "no awaiting nodes" returns null, but "the only
    // awaiting node is the one I'm on" returns that id rather than
    // leaving the user staring at a `Ctrl+. did nothing` dead key.
    useAgentNodeStore.setState({
      agentNodes: [makeNode({ id: 1, mesh_id: 1, position: 0, status: 'awaiting_input' })],
      activeNodeId: 1,
    });
    expect(nextAwaitingNodeId()).toBe(1);
  });

  it('scopes the search to the active node\'s mesh (other meshes ignored)', () => {
    // Multi-mesh invariant: switching meshes via the sidebar must not let
    // Ctrl+. leak into the previous mesh's awaiting nodes. The active node
    // here is in mesh 1, so mesh 2's awaiting node is invisible to the cycle.
    useAgentNodeStore.setState({
      agentNodes: [
        makeNode({ id: 1, mesh_id: 1, position: 0, status: 'running' }),
        makeNode({ id: 2, mesh_id: 1, position: 1, status: 'awaiting_input' }),
        makeNode({ id: 10, mesh_id: 2, position: 0, status: 'awaiting_input' }),
      ],
      activeNodeId: 1,
    });
    expect(nextAwaitingNodeId()).toBe(2);
  });

  it('picks the next awaiting node AFTER the active node\'s position', () => {
    // Core cycle: next-after-current. The active node is at position 0 and
    // itself awaiting; the next press must advance to the *following*
    // awaiting node, not stay on the current one (git log --skip convention).
    useAgentNodeStore.setState({
      agentNodes: [
        makeNode({ id: 1, mesh_id: 1, position: 0, status: 'awaiting_input' }),
        makeNode({ id: 2, mesh_id: 1, position: 1, status: 'running' }),
        makeNode({ id: 3, mesh_id: 1, position: 2, status: 'awaiting_input' }),
        makeNode({ id: 4, mesh_id: 1, position: 3, status: 'awaiting_input' }),
      ],
      activeNodeId: 1,
    });
    expect(nextAwaitingNodeId()).toBe(3);
  });

  it('wraps around when no later awaiting node exists', () => {
    // Acceptance criterion: "Wraps around when reaching the end of the node
    // list". Active is at position 3 (the last awaiting); cycle wraps to
    // position 0, which is the next awaiting node.
    useAgentNodeStore.setState({
      agentNodes: [
        makeNode({ id: 1, mesh_id: 1, position: 0, status: 'awaiting_input' }),
        makeNode({ id: 2, mesh_id: 1, position: 1, status: 'running' }),
        makeNode({ id: 3, mesh_id: 1, position: 2, status: 'running' }),
        makeNode({ id: 4, mesh_id: 1, position: 3, status: 'awaiting_input' }),
      ],
      activeNodeId: 4,
    });
    expect(nextAwaitingNodeId()).toBe(1);
  });

  it('walks by on-screen position, not by id or array order', () => {
    // Defensive: the source array from the backend may not be in `position`
    // order (it's filtered/sorted by the App.tsx handlers). The function
    // MUST sort by `position` so the cycle matches the user's visual
    // mental model.
    useAgentNodeStore.setState({
      // Intentionally scrambled insertion order.
      agentNodes: [
        makeNode({ id: 4, mesh_id: 1, position: 3, status: 'awaiting_input' }),
        makeNode({ id: 1, mesh_id: 1, position: 0, status: 'running' }),
        makeNode({ id: 3, mesh_id: 1, position: 2, status: 'awaiting_input' }),
        makeNode({ id: 2, mesh_id: 1, position: 1, status: 'awaiting_input' }),
      ],
      activeNodeId: 1,
    });
    // Sorted by position: 1(running), 2(awaiting), 3(awaiting), 4(awaiting).
    // Next after position 0 → position 1 → id 2.
    expect(nextAwaitingNodeId()).toBe(2);
  });

  it('returns null when activeNodeId points at a node that no longer exists', () => {
    // Edge: a stale activeNodeId (e.g. a node that was just deleted while
    // the React tree hadn't re-rendered). `getActiveNode()` returns null in
    // this case, which the function treats identically to "no active
    // node" — a no-op. This matches the arrow-traversal handler's policy
    // and keeps the cycle strictly scoped to a known active node.
    useAgentNodeStore.setState({
      agentNodes: [
        makeNode({ id: 1, mesh_id: 1, position: 0, status: 'awaiting_input' }),
        makeNode({ id: 2, mesh_id: 1, position: 1, status: 'awaiting_input' }),
      ],
      activeNodeId: 999, // not in agentNodes
    });
    expect(nextAwaitingNodeId()).toBeNull();
  });

  it('skips nodes that are not awaiting_input (running, idle, error, ...)', () => {
    // Only `status === 'awaiting_input'` qualifies. Every other SessionStatus
    // variant is invisible to the cycle, even if the node is between the
    // active node and a later awaiting one.
    useAgentNodeStore.setState({
      agentNodes: [
        makeNode({ id: 1, mesh_id: 1, position: 0, status: 'running' }),
        makeNode({ id: 2, mesh_id: 1, position: 1, status: 'idle' }),
        makeNode({ id: 3, mesh_id: 1, position: 2, status: 'error' }),
        makeNode({ id: 4, mesh_id: 1, position: 3, status: 'suspended' }),
        makeNode({ id: 5, mesh_id: 1, position: 4, status: 'completed' }),
        makeNode({ id: 6, mesh_id: 1, position: 5, status: 'awaiting_input' }),
      ],
      activeNodeId: 1,
    });
    expect(nextAwaitingNodeId()).toBe(6);
  });
});

describe('jumpToNextAwaitingNode (issue #64 — mutating entry point)', () => {
  beforeEach(() => {
    useAgentNodeStore.setState({ agentNodes: [], activeNodeId: null });
  });

  it('sets activeNodeId to the next awaiting node', () => {
    useAgentNodeStore.setState({
      agentNodes: [
        makeNode({ id: 1, mesh_id: 1, position: 0, status: 'running' }),
        makeNode({ id: 2, mesh_id: 1, position: 1, status: 'awaiting_input' }),
      ],
      activeNodeId: 1,
    });
    jumpToNextAwaitingNode();
    expect(useAgentNodeStore.getState().activeNodeId).toBe(2);
  });

  it('does NOT mutate activeNodeId when no awaiting nodes exist', () => {
    // The "no-op" acceptance criterion applied to the mutating entry
    // point: pressing Ctrl+. with nothing awaiting must leave the active
    // node alone. The terminal-focus effect would otherwise steal focus
    // from the user's current prompt if we set null.
    useAgentNodeStore.setState({
      agentNodes: [makeNode({ id: 1, mesh_id: 1, position: 0, status: 'running' })],
      activeNodeId: 1,
    });
    jumpToNextAwaitingNode();
    expect(useAgentNodeStore.getState().activeNodeId).toBe(1);
  });

  it('returns the id it set, or null when it no-opped (handy for future observability hooks)', () => {
    // Two-return paths in one test: with a target → returns the id;
    // without → returns null. The handler doesn't use the return value yet
    // but pinning it now means a future toast/status-line consumer can't
    // accidentally regress the contract.
    useAgentNodeStore.setState({
      agentNodes: [
        makeNode({ id: 1, mesh_id: 1, position: 0, status: 'awaiting_input' }),
      ],
      activeNodeId: 1,
    });
    expect(jumpToNextAwaitingNode()).toBe(1);

    useAgentNodeStore.setState({
      agentNodes: [makeNode({ id: 1, mesh_id: 1, position: 0, status: 'running' })],
      activeNodeId: 1,
    });
    expect(jumpToNextAwaitingNode()).toBeNull();
  });

  it('cycles correctly across repeated presses (acceptance criterion: "next awaiting")', () => {
    // Sequence of presses on the same mesh should walk the awaiting nodes
    // in on-screen order, wrapping at the end. This is the user-facing
    // contract pinned in one test so a regression to "always picks the
    // same node" or "skips an awaiting node" is caught here.
    useAgentNodeStore.setState({
      agentNodes: [
        makeNode({ id: 1, mesh_id: 1, position: 0, status: 'awaiting_input' }),
        makeNode({ id: 2, mesh_id: 1, position: 1, status: 'running' }),
        makeNode({ id: 3, mesh_id: 1, position: 2, status: 'awaiting_input' }),
      ],
      activeNodeId: 1,
    });

    // Press 1: active=1 (awaiting) → next is 3.
    jumpToNextAwaitingNode();
    expect(useAgentNodeStore.getState().activeNodeId).toBe(3);

    // Press 2: active=3 (awaiting) → wrap → 1.
    jumpToNextAwaitingNode();
    expect(useAgentNodeStore.getState().activeNodeId).toBe(1);

    // Press 3: same as Press 1.
    jumpToNextAwaitingNode();
    expect(useAgentNodeStore.getState().activeNodeId).toBe(3);
  });
});
