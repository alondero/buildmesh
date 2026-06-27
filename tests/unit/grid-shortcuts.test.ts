import { describe, it, expect, beforeEach } from 'vitest';
import { toggleGridMaximize } from '../../src/lib/gridShortcuts';
import { useUIStore } from '../../src/stores/uiStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import type { AgentNode } from '../../src/types/generated/AgentNode';

// Minimal valid AgentNode — we never invoke any backend; `getActiveNode` only
// scans the in-memory array. Mirrors the shape used by sibling shortcut tests
// (`tests/unit/grid-node-header.test.tsx`) so the layout stays familiar.
// `env` must be a valid `EnvType` (`'windows' | 'wsl'`); vitest transpiles
// tests with esbuild and doesn't strict-typecheck them, so a bogus value
// would slip past CI today — but the test stays correct if/when the
// vitest config adds typecheck.
const NODE: AgentNode = {
  id: 7,
  mesh_id: 1,
  name: 'agent-7',
  path: '/repo',
  branch: 'main',
  env: 'wsl',
  provider: 'claude',
  status: 'running',
  use_worktree: false,
  position: 0,
  created_at: '2026-06-27T00:00:00Z',
  scratchpad: '',
  sandbox: false,
  cli_session_id: null,
  worktree_name: null,
  source_issue: null,
  archived: false,
};

describe('toggleGridMaximize (#668 Alt+G / Cmd+G)', () => {
  beforeEach(() => {
    // Reset both stores to a known empty baseline; tests opt-in to state.
    useUIStore.setState({ maximizedNodeId: null });
    useAgentNodeStore.setState({ agentNodes: [], activeNodeId: null });
  });

  it('no-ops when there is no active node', () => {
    // Acceptance criterion #1: pressing Alt+G with no active node is a no-op,
    // not an error and not a phantom maximize of a non-existent id.
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: null });
    toggleGridMaximize();
    expect(useUIStore.getState().maximizedNodeId).toBeNull();
  });

  it('no-ops when there are no nodes at all', () => {
    toggleGridMaximize();
    expect(useUIStore.getState().maximizedNodeId).toBeNull();
  });

  it('maximizes the active node when nothing is maximized', () => {
    // Acceptance criterion #2: non-maximized active node → maximized solo view.
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
    toggleGridMaximize();
    expect(useUIStore.getState().maximizedNodeId).toBe(NODE.id);
  });

  it('restores the grid when something is already maximized', () => {
    // Acceptance criterion #3: second press clears `maximizedNodeId`,
    // regardless of which node was active at the time of the press — the
    // restore path is keyed on `maximizedNodeId`, not the active node, so
    // the user always gets out of whatever solo view they're in.
    useUIStore.setState({ maximizedNodeId: NODE.id });
    // Note: we deliberately leave `activeNodeId` pointing at a different
    // node to prove the restore doesn't depend on the active node matching.
    useAgentNodeStore.setState({
      agentNodes: [NODE, { ...NODE, id: 9, name: 'agent-9', position: 1 }],
      activeNodeId: 9,
    });
    toggleGridMaximize();
    expect(useUIStore.getState().maximizedNodeId).toBeNull();
  });

  it('toggles back and forth: maximize → restore → maximize', () => {
    // Sequence covers all three acceptance criteria in one call chain.
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });

    toggleGridMaximize();
    expect(useUIStore.getState().maximizedNodeId).toBe(NODE.id);

    toggleGridMaximize();
    expect(useUIStore.getState().maximizedNodeId).toBeNull();

    toggleGridMaximize();
    expect(useUIStore.getState().maximizedNodeId).toBe(NODE.id);
  });

  it('leaves the active node alone when restoring (passive Esc-style exit)', () => {
    // The Ctrl+Arrow "exit-and-move" path in App.tsx swaps `activeNodeId`
    // onto the previously-maximized node (issue #669). Alt+G is the
    // *toggle*, not the navigation — it must not mutate `activeNodeId`,
    // because the user is in solo-view of the node they're already on.
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
    toggleGridMaximize();
    toggleGridMaximize();
    expect(useAgentNodeStore.getState().activeNodeId).toBe(NODE.id);
  });
});