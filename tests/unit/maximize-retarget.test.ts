/**
 * Issue: clicking a sidebar NodeItem while a node is soloed should
 * retarget the solo view to the clicked node (not exit the solo view, not
 * stick on the previous node). Migrated to View Modes in wayfinder #982
 * (#983): Single mode renders the active node, so the retarget is now
 * just `setActiveNode` — and the handler must NOT call `selectMesh` while
 * in Single, or the uiStore mesh-sync would break the user out of the
 * solo view. This file pins the store-level contract that the click
 * handler in `src/components/Sidebar/MeshItem.tsx` must produce.
 *
 * Patterned on `tests/unit/grid-shortcuts.test.ts` — real Zustand stores,
 * imperative calls, no React render.
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { useUIStore } from '../../src/stores/uiStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { Mesh } from '../../src/types/generated/Mesh';
import { seedAgentNodes } from './helpers/seedAgentNodes';

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
  is_pinned: false,
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
  if (useUIStore.getState().viewMode !== 'single') {
    useMeshStore.getState().selectMesh(node.mesh_id);
  }
}

describe('sidebar click and View Modes (wayfinder #982)', () => {
  beforeEach(() => {
    // Reset all three stores to a known baseline. meshStore goes FIRST:
    // the uiStore mesh-subscription fires synchronously on a
    // selectedMeshId change and would clobber a viewMode set before it.
    useMeshStore.setState({
      meshes: [mesh10, mesh20],
      meshesById: new Map([[10, mesh10], [20, mesh20]]),
      selectedMeshId: null,
      loading: false,
      error: null,
    });
    useUIStore.setState({ viewMode: 'all', lastNonSingleMode: 'all' });
    seedAgentNodes([nodeA, nodeB, nodeC], null);
  });

  it('same-mesh retarget: clicking B while A is soloed keeps Single and retargets to B', () => {
    // Given: Single mode (entered from Mesh Grid), mesh 10 selected, A active.
    useMeshStore.setState({ selectedMeshId: mesh10.id });
    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'mesh' });
    useAgentNodeStore.setState({ activeNodeId: nodeA.id });

    // When: user clicks B (same mesh).
    clickSidebarNode(nodeB);

    // Then: Single survives and follows the click to B — Single renders the
    // active node, so setActiveNode IS the retarget. The sidebar mesh
    // selection is untouched (Single never writes it).
    expect(useUIStore.getState().viewMode).toBe('single');
    expect(useAgentNodeStore.getState().activeNodeId).toBe(nodeB.id);
    expect(useMeshStore.getState().selectedMeshId).toBe(mesh10.id);
  });

  it('cross-mesh retarget: clicking C (different mesh) keeps Single AND keeps the selection', () => {
    // The View Modes contract (#983): Single shows the active node
    // regardless of mesh scope — a cross-mesh sidebar click retargets the
    // solo view without breaking out of it. The handler must skip
    // selectMesh in Single, or the uiStore mesh-sync would flip the canvas
    // to Mesh Grid (and move the sidebar selection away from the mesh the
    // user was browsing).
    useMeshStore.setState({ selectedMeshId: mesh10.id });
    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'mesh' });
    useAgentNodeStore.setState({ activeNodeId: nodeA.id });

    clickSidebarNode(nodeC);

    expect(useUIStore.getState().viewMode).toBe('single');
    expect(useAgentNodeStore.getState().activeNodeId).toBe(nodeC.id);
    expect(useMeshStore.getState().selectedMeshId).toBe(mesh10.id);
  });

  it('self-click on the soloed node is a no-op', () => {
    // Clicking the already-soloed node must not exit Single (matches the
    // visual — the user is already looking at it; avoid accidental exits).
    useMeshStore.setState({ selectedMeshId: mesh10.id });
    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'mesh' });
    useAgentNodeStore.setState({ activeNodeId: nodeA.id });

    clickSidebarNode(nodeA);

    expect(useUIStore.getState().viewMode).toBe('single');
    expect(useAgentNodeStore.getState().activeNodeId).toBe(nodeA.id);
    expect(useMeshStore.getState().selectedMeshId).toBe(mesh10.id);
  });

  it('click from a grid mode selects the mesh and flips the canvas to Mesh Grid', () => {
    // The non-Single path preserves today's behaviour: the click selects
    // the node's mesh, and the uiStore mesh-sync subscription (one filter,
    // two controls) flips the canvas to Mesh Grid.
    useUIStore.setState({ viewMode: 'all', lastNonSingleMode: 'all' });
    useMeshStore.setState({ selectedMeshId: null });
    useAgentNodeStore.setState({ activeNodeId: null });

    clickSidebarNode(nodeB);

    expect(useAgentNodeStore.getState().activeNodeId).toBe(nodeB.id);
    expect(useMeshStore.getState().selectedMeshId).toBe(mesh10.id);
    expect(useUIStore.getState().viewMode).toBe('mesh');
  });
});
