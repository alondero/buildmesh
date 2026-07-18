/**
 * Issue: clicking a sidebar NodeItem while a node is maximised should
 * retarget the solo view to the clicked node (not exit maximise, not stick
 * on the previous node). This file pins the store-level contract that the
 * click handler in `src/components/Sidebar/MeshItem.tsx` must produce.
 *
 * Patterned on `tests/unit/grid-shortcuts.test.ts` — real Zustand stores,
 * imperative calls, no React render. The auto-clear race in
 * `AgentNodeView.tsx:202-208` is exercised visually by `/verify-ui`.
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { useUIStore } from '../../src/stores/uiStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { Mesh } from '../../src/types/generated/Mesh';

// Minimal AgentNode for store setup — mirrors the shape used by sibling
// shortcut tests (`grid-shortcuts.test.ts`, `grid-node-header.test.tsx`).
// The store never round-trips through the backend in these tests, so any
// shape that satisfies the type is fine.
const nodeA: AgentNode = {
  id: 1,
  mesh_id: 10,
  name: 'agent-A',
  path: '/repo/a',
  branch: 'main',
  env: 'wsl',
  provider: 'claude',
  status: 'running',
  use_worktree: false,
  position: 0,
  created_at: '2026-07-16T00:00:00Z',
  scratchpad: '',
  sandbox: false,
  cli_session_id: null,
  worktree_name: null,
  source_issue: null,
  archived: false,
};

const nodeB: AgentNode = {
  ...nodeA,
  id: 2,
  mesh_id: 10,
  name: 'agent-B',
  position: 1,
};

const nodeC: AgentNode = {
  ...nodeA,
  id: 3,
  mesh_id: 20,
  name: 'agent-C',
  path: '/repo/c',
  position: 0,
};

// Mesh shape used by `useMeshStore` — also mirrors what `fetchMeshes`
// populates after a real `listMeshes()` call. The fields we touch are
// `id` and `name`; the rest is presence-only.
const mesh10: Mesh = {
  id: 10,
  name: 'mesh-10',
  path: '/repo/a',
  branch: 'main',
  position: 0,
  color: null,
  layout: 'grid',
  use_worktree: false,
  created_at: '2026-07-16T00:00:00Z',
  github_owner: null,
  github_repo: null,
  github_last_synced: null,
  pre_spawn_pool_size: 0,
};
const mesh20: Mesh = { ...mesh10, id: 20, name: 'mesh-20', path: '/repo/c', position: 1 };

/**
 * Simulates exactly what the `onSelect` callback in `MeshItem.tsx` does
 * for a sidebar click. Stays in sync with the production handler — if the
 * call order / set list changes there, this must change too. Keep them
 * visually identical so a reviewer can diff them.
 */
function clickSidebarNode(node: AgentNode): void {
  useAgentNodeStore.getState().setActiveNode(node.id);
  useMeshStore.getState().selectMesh(node.mesh_id);
  const currentMaximized = useUIStore.getState().maximizedNodeId;
  if (currentMaximized !== null && currentMaximized !== node.id) {
    useUIStore.getState().setMaximizedNode(node.id);
  }
}

describe('sidebar click while maximised', () => {
  beforeEach(() => {
    // Reset all three stores to a known baseline.
    useUIStore.setState({ maximizedNodeId: null });
    useAgentNodeStore.setState({
      agentNodes: [nodeA, nodeB, nodeC],
      activeNodeId: null,
    });
    useMeshStore.setState({
      meshes: [mesh10, mesh20],
      meshesById: new Map([[10, mesh10], [20, mesh20]]),
      selectedMeshId: null,
      loading: false,
      error: null,
    });
  });

  it('same-mesh retarget: clicking B while A is maximised flips maximise to B', () => {
    // Given: A is maximised, mesh 10 is selected, no active node yet.
    useUIStore.setState({ maximizedNodeId: nodeA.id });
    useMeshStore.setState({ selectedMeshId: mesh10.id });
    useAgentNodeStore.setState({ activeNodeId: null });

    // When: user clicks B (same mesh).
    clickSidebarNode(nodeB);

    // Then: maximise follows the click to B; mesh stays on 10; activeNode moves.
    expect(useUIStore.getState().maximizedNodeId).toBe(nodeB.id);
    expect(useMeshStore.getState().selectedMeshId).toBe(mesh10.id);
    expect(useAgentNodeStore.getState().activeNodeId).toBe(nodeB.id);
  });

  it('cross-mesh retarget: clicking C (different mesh) keeps maximise ON', () => {
    // The pre-fix bug: cross-mesh click would *exit* maximise because
    // the auto-clear effect saw `selectedMeshId` flipped but
    // `maximizedNodeId` still on the now-filtered-out A. This test pins
    // that the click handler leaves `maximizedNodeId` on the new node so
    // the auto-clear predicate (`maximizedNodeId != null && maximizedNode
    // == null`) is false on the next render.
    useUIStore.setState({ maximizedNodeId: nodeA.id });
    useMeshStore.setState({ selectedMeshId: mesh10.id });
    useAgentNodeStore.setState({ activeNodeId: nodeA.id });

    clickSidebarNode(nodeC);

    expect(useUIStore.getState().maximizedNodeId).toBe(nodeC.id);
    expect(useMeshStore.getState().selectedMeshId).toBe(mesh20.id);
    expect(useAgentNodeStore.getState().activeNodeId).toBe(nodeC.id);
  });

  it('self-click on the maximised node is a no-op for maximise', () => {
    // Per user decision: clicking the already-maximised node should NOT
    // toggle off maximise (matches the visual — the user is already
    // looking at it; avoid accidental exits). The store-level
    // idempotency guard in `setMaximizedNode` is the second line of
    // defence; the click handler also short-circuits before calling it.
    useUIStore.setState({ maximizedNodeId: nodeA.id });
    useMeshStore.setState({ selectedMeshId: mesh10.id });
    useAgentNodeStore.setState({ activeNodeId: nodeA.id });

    clickSidebarNode(nodeA);

    expect(useUIStore.getState().maximizedNodeId).toBe(nodeA.id);
    expect(useMeshStore.getState().selectedMeshId).toBe(mesh10.id);
    expect(useAgentNodeStore.getState().activeNodeId).toBe(nodeA.id);
  });

  it('click while nothing is maximised leaves maximise null (current behaviour)', () => {
    // Regression guard: the new wiring must not flip an empty maximise
    // state — preserve the pre-fix "click selects, nothing else".
    useUIStore.setState({ maximizedNodeId: null });
    useMeshStore.setState({ selectedMeshId: null });
    useAgentNodeStore.setState({ activeNodeId: null });

    clickSidebarNode(nodeB);

    expect(useUIStore.getState().maximizedNodeId).toBe(null);
    expect(useMeshStore.getState().selectedMeshId).toBe(mesh10.id);
    expect(useAgentNodeStore.getState().activeNodeId).toBe(nodeB.id);
  });
});
