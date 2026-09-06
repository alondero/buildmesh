/**
 * View Mode visibility rules (wayfinder #982 — state model #983, rendering
 * #986). Pins the pure helpers in `src/lib/viewModes.ts`: which nodes each
 * grid mode renders, in what order, and which node Single mode solos. These
 * are the same definitions keyboard traversal (#987) will consume, so the
 * contracts here are the seam that ticket builds on.
 */
import { describe, it, expect } from 'vitest';
import {
  scopeNodesForMode,
  resolveSingleNode,
  resolveMeshScopeId,
} from '../../src/lib/viewModes';
import type { AgentNode } from '../../src/types/generated/AgentNode';

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 1,
    mesh_id: 1,
    name: 'agent-1',
    path: '/repo',
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
    ...overrides,
  };
}

// The canonical store order is (mesh_id, position) — the fixtures below are
// declared in that order, mirroring what `list_agent_nodes` returns and what
// `persistPositions` maintains in the store.
const a1 = makeNode({ id: 1, mesh_id: 10, position: 0, name: 'a1' });
const a2 = makeNode({ id: 2, mesh_id: 10, position: 1, name: 'a2', is_pinned: true });
const b1 = makeNode({ id: 3, mesh_id: 20, position: 0, name: 'b1', is_pinned: true });
const b2 = makeNode({ id: 4, mesh_id: 20, position: 1, name: 'b2' });
const NODES: AgentNode[] = [a1, a2, b1, b2];

describe('scopeNodesForMode (wayfinder #982)', () => {
  it("'all' returns every node in canonical (mesh_id, position) order", () => {
    expect(scopeNodesForMode('all', NODES, null, null)).toEqual(NODES);
  });

  it("'pinned' returns pinned nodes across all meshes in canonical order", () => {
    // Cross-mesh by nature (ticket #983): the sidebar selection is
    // irrelevant to the pinned scope.
    const pinned = scopeNodesForMode('pinned', NODES, 10, null);
    expect(pinned.map(n => n.id)).toEqual([a2.id, b1.id]);
  });

  it("'pinned' ignores selectedMeshId entirely", () => {
    expect(scopeNodesForMode('pinned', NODES, null, null).map(n => n.id))
      .toEqual(scopeNodesForMode('pinned', NODES, 20, null).map(n => n.id));
  });

  it("'mesh' filters to the sidebar-selected mesh", () => {
    expect(scopeNodesForMode('mesh', NODES, 10, null).map(n => n.id)).toEqual([a1.id, a2.id]);
  });

  it("'mesh' falls back to the active node's mesh when nothing is selected", () => {
    // Ticket #983: "the switcher's Mesh Grid segment uses selectedMeshId,
    // falling back to the active node's mesh".
    expect(scopeNodesForMode('mesh', NODES, null, b1.id).map(n => n.id)).toEqual([b1.id, b2.id]);
  });

  it("'mesh' falls back to the first node's mesh with no selection and no active node", () => {
    // A persisted 'mesh' boot must not land on an empty grid while nodes
    // exist — the fallback chain ends at the first loaded node's mesh.
    expect(scopeNodesForMode('mesh', NODES, null, null).map(n => n.id)).toEqual([a1.id, a2.id]);
  });

  it("'mesh' returns an empty scope when no nodes are loaded", () => {
    expect(scopeNodesForMode('mesh', [], null, null)).toEqual([]);
  });

  it("'filtered' keeps only nodes matching the search query (#1609)", () => {
    const controls = { gridSearchQuery: 'A', gridProviderFilter: null, gridStatusFilter: null };
    expect(scopeNodesForMode('filtered', NODES, null, null, controls).map(n => n.id))
      .toEqual([a1.id, a2.id]);
  });

  it("'filtered' composes the search and status filters (#1609)", () => {
    // All fixtures share provider 'claude', so the discriminating axes here
    // are the name substring and the status.
    const controls = { gridSearchQuery: 'a', gridProviderFilter: null, gridStatusFilter: 'running' };
    expect(scopeNodesForMode('filtered', NODES, null, null, controls).map(n => n.id))
      .toEqual([a1.id, a2.id]);
    const providerOnly = { gridSearchQuery: '', gridProviderFilter: 'claude', gridStatusFilter: 'awaiting_input' };
    expect(scopeNodesForMode('filtered', NODES, null, null, providerOnly)).toEqual([]);
  });

  it("'filtered' ignores the sidebar selection — cross-mesh like Pinned (#1609)", () => {
    const controls = { gridSearchQuery: '', gridProviderFilter: null, gridStatusFilter: null };
    expect(scopeNodesForMode('filtered', NODES, 10, null, controls).map(n => n.id))
      .toEqual([a1.id, a2.id, b1.id, b2.id]);
  });

  it("'filtered' with the neutral controls is the All scope (#1609)", () => {
    // No search, no filters → the dedicated view shows every node — the
    // same contract 'all' pins, so the empty search never surprises.
    const controls = { gridSearchQuery: '   ', gridProviderFilter: null, gridStatusFilter: null };
    expect(scopeNodesForMode('filtered', NODES, null, null, controls)).toEqual(NODES);
  });

  it("'filtered' is whitespace-tolerant on the search and case-insensitive", () => {
    const controls = { gridSearchQuery: '  B1  ', gridProviderFilter: null, gridStatusFilter: null };
    expect(scopeNodesForMode('filtered', NODES, null, null, controls).map(n => n.id))
      .toEqual([b1.id]);
  });

  it("mesh/pinned/all scopes are unaffected by the controls argument", () => {
    // The Grid Controls belong to the Filtered view; the other scopes must
    // not narrow even when a stale search text is in the store.
    const controls = { gridSearchQuery: 'a1', gridProviderFilter: 'minimax', gridStatusFilter: 'error' };
    expect(scopeNodesForMode('all', NODES, null, null, controls)).toEqual(NODES);
    expect(scopeNodesForMode('pinned', NODES, null, null, controls).map(n => n.id))
      .toEqual([a2.id, b1.id]);
    expect(scopeNodesForMode('mesh', NODES, 10, null, controls).map(n => n.id))
      .toEqual([a1.id, a2.id]);
  });
});

describe('resolveMeshScopeId', () => {
  it('prefers the sidebar selection', () => {
    expect(resolveMeshScopeId(NODES, 20, a1.id)).toBe(20);
  });

  it('falls back through active node → first node → null', () => {
    expect(resolveMeshScopeId(NODES, null, b2.id)).toBe(20);
    expect(resolveMeshScopeId(NODES, null, null)).toBe(10);
    expect(resolveMeshScopeId([], null, null)).toBeNull();
  });
});

describe('resolveSingleNode (wayfinder #982)', () => {
  it('solos the active node regardless of any mesh scope (explicit focus wins)', () => {
    // Active node lives in mesh 20; the "current scope" is mesh 10 — the
    // active node still wins. Single is cross-mesh by nature, like Pinned.
    expect(resolveSingleNode(NODES, b2.id, 'mesh', 10)).toBe(b2);
  });
  it('falls back to the first node of the scope Single was entered from', () => {
    // Entered from Pinned → first pinned node, not the first node overall.
    expect(resolveSingleNode(NODES, null, 'pinned', null)).toBe(a2);
    // Entered from Mesh (selection on 20) → first node of mesh 20.
    expect(resolveSingleNode(NODES, null, 'mesh', 20)).toBe(b1);
    // Entered from All → first node overall.
    expect(resolveSingleNode(NODES, null, 'all', null)).toBe(a1);
  });

  it('degrades to the first node overall when the remembered scope is empty', () => {
    // Entered Single from Pinned, but nothing is pinned anymore — any node
    // is a better solo target than the empty state while nodes exist.
    const unpinned = NODES.map(n => ({ ...n, is_pinned: false }));
    expect(resolveSingleNode(unpinned, null, 'pinned', null)).toEqual(unpinned[0]);
  });

  it('returns null when no nodes are loaded (the caller renders the splash)', () => {
    expect(resolveSingleNode([], null, 'all', null)).toBeNull();
  });

  it('ignores a stale activeNodeId whose node is gone', () => {
    // Deleted-while-soloed: the id no longer resolves, so the fallback
    // chain takes over exactly as if activeNodeId were null.
    expect(resolveSingleNode(NODES, 999, 'mesh', 10)).toBe(a1);
  });

  it('narrows the Filtered fallback by the Grid Controls (#1609)', () => {
    // Entered Single from Filtered with a search active, then the active
    // node vanished (deleted while soloed): the fallback must come from
    // the matching set, not the unfiltered store — the user would
    // otherwise land on a node the Filtered grid never showed.
    const controls = { gridSearchQuery: 'a', gridProviderFilter: null, gridStatusFilter: null };
    expect(resolveSingleNode(NODES, 999, 'filtered', null, controls)).toBe(a1);
    const tight = { gridSearchQuery: 'b', gridProviderFilter: null, gridStatusFilter: null };
    expect(resolveSingleNode(NODES, 999, 'filtered', null, tight)).toBe(b1);
  });
});
