/**
 * View Mode visibility rules (wayfinder #982 — state model #983, rendering
 * #986). Pins the pure helpers in `src/lib/viewModes.ts`: which nodes each
 * grid mode renders, in what order, and which node Single mode solos. These
 * are the same definitions keyboard traversal (#987) will consume, so the
 * contracts here are the seam that ticket builds on.
 */
import { describe, it, expect } from 'vitest';
import {
  deriveScope,
  isMeshScopedView,
  selectionMeshId,
  scopeNodesForMode,
  resolveSingleNode,
  type DerivedScope,
  type ScopeInput,
} from '../../src/lib/viewModes';
import type { NonSingleViewMode, ViewMode } from '../../src/stores/uiStore';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { Mesh } from '../../src/types/generated/Mesh';

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
    expect(scopeNodesForMode('all', NODES, null)).toEqual(NODES);
  });

  it("'pinned' returns pinned nodes across all meshes in canonical order", () => {
    // Cross-mesh by nature (ticket #983): the sidebar selection is
    // irrelevant to the pinned scope.
    const pinned = scopeNodesForMode('pinned', NODES, 10);
    expect(pinned.map(n => n.id)).toEqual([a2.id, b1.id]);
  });

  it("'pinned' ignores selectedMeshId entirely", () => {
    expect(scopeNodesForMode('pinned', NODES, null).map(n => n.id))
      .toEqual(scopeNodesForMode('pinned', NODES, 20).map(n => n.id));
  });

  it("'mesh' filters to the sidebar-selected mesh", () => {
    expect(scopeNodesForMode('mesh', NODES, 10).map(n => n.id)).toEqual([a1.id, a2.id]);
  });

  it("'mesh' has no scope when nothing is selected (#2071)", () => {
    // #2071 deleted the fallback chain: with no sidebar selection the Mesh
    // Grid has no Mesh to scope to, so it shows nothing and renders the
    // explicit "no mesh selected" empty state. Picking the focused node's
    // Mesh (or the first loaded Mesh) here would silently show a scope
    // the user never chose — so the focused node is a Mesh away and the
    // scope still reports no Mesh at all.
    const focusedElsewhere = deriveScope({
      viewMode: 'mesh',
      lastNonSingleMode: 'mesh',
      agentNodes: NODES,
      selectedMeshId: null,
      activeNodeId: b1.id,
      meshes: [MESH_10, MESH_20],
    });
    expect(focusedElsewhere.visibleNodes).toEqual([]);
    expect(focusedElsewhere.isMeshScoped).toBe(false);
    expect(focusedElsewhere.mesh).toBeNull();
    expect(scopeNodesForMode('mesh', NODES, null)).toEqual([]);
  });

  it("'mesh' returns an empty scope when no nodes are loaded", () => {
    expect(scopeNodesForMode('mesh', [], null)).toEqual([]);
  });

  it("'filtered' keeps only nodes matching the search query (#1609)", () => {
    const controls = { gridSearchQuery: 'A', gridProviderFilter: null, gridStatusFilter: null };
    expect(scopeNodesForMode('filtered', NODES, null, controls).map(n => n.id))
      .toEqual([a1.id, a2.id]);
  });

  it("'filtered' composes the search and status filters (#1609)", () => {
    // All fixtures share provider 'claude', so the discriminating axes here
    // are the name substring and the status.
    const controls = { gridSearchQuery: 'a', gridProviderFilter: null, gridStatusFilter: 'running' };
    expect(scopeNodesForMode('filtered', NODES, null, controls).map(n => n.id))
      .toEqual([a1.id, a2.id]);
    const providerOnly = { gridSearchQuery: '', gridProviderFilter: 'claude', gridStatusFilter: 'awaiting_input' };
    expect(scopeNodesForMode('filtered', NODES, null, providerOnly)).toEqual([]);
  });

  it("'filtered' ignores the sidebar selection — cross-mesh like Pinned (#1609)", () => {
    const controls = { gridSearchQuery: '', gridProviderFilter: null, gridStatusFilter: null };
    expect(scopeNodesForMode('filtered', NODES, 10, controls).map(n => n.id))
      .toEqual([a1.id, a2.id, b1.id, b2.id]);
  });

  it("'filtered' with the neutral controls is the All scope (#1609)", () => {
    // No search, no filters → the dedicated view shows every node — the
    // same contract 'all' pins, so the empty search never surprises.
    const controls = { gridSearchQuery: '   ', gridProviderFilter: null, gridStatusFilter: null };
    expect(scopeNodesForMode('filtered', NODES, null, controls)).toEqual(NODES);
  });

  it("'filtered' is whitespace-tolerant on the search and case-insensitive", () => {
    const controls = { gridSearchQuery: '  B1  ', gridProviderFilter: null, gridStatusFilter: null };
    expect(scopeNodesForMode('filtered', NODES, null, controls).map(n => n.id))
      .toEqual([b1.id]);
  });

  it("mesh/pinned/all scopes are unaffected by the controls argument", () => {
    // The Grid Controls belong to the Filtered view; the other scopes must
    // not narrow even when a stale search text is in the store.
    const controls = { gridSearchQuery: 'a1', gridProviderFilter: 'minimax', gridStatusFilter: 'error' };
    expect(scopeNodesForMode('all', NODES, null, controls)).toEqual(NODES);
    expect(scopeNodesForMode('pinned', NODES, null, controls).map(n => n.id))
      .toEqual([a2.id, b1.id]);
    expect(scopeNodesForMode('mesh', NODES, 10, controls).map(n => n.id))
      .toEqual([a1.id, a2.id]);
  });
});

// #2071 — the Mesh-scope fallback helper (`resolveMeshScopeId`) is gone. Its
// replacement is `deriveScope`: one pure read of "what is the scope right now"
// that every surface renders instead of re-deriving from the selection and the
// focused node.
const MESH_10 = { id: 10, name: 'alpha' } as Mesh;
const MESH_20 = { id: 20, name: 'beta' } as Mesh;
const MESHES = [MESH_10, MESH_20];

function scope(overrides: Partial<ScopeInput> = {}): DerivedScope {
  return deriveScope({
    viewMode: 'all',
    lastNonSingleMode: 'all',
    agentNodes: NODES,
    selectedMeshId: null,
    activeNodeId: null,
    meshes: MESHES,
    ...overrides,
  });
}

describe('deriveScope (#2071)', () => {
  it('maps each View Mode to its node set', () => {
    expect(scope({ viewMode: 'all' }).visibleNodes.map(n => n.id)).toEqual([a1.id, a2.id, b1.id, b2.id]);
    expect(scope({ viewMode: 'pinned' }).visibleNodes.map(n => n.id)).toEqual([a2.id, b1.id]);
    expect(scope({ viewMode: 'mesh', selectedMeshId: 20 }).visibleNodes.map(n => n.id)).toEqual([b1.id, b2.id]);
    expect(scope({
      viewMode: 'filtered',
      controls: { gridSearchQuery: 'a', gridProviderFilter: null, gridStatusFilter: null },
    }).visibleNodes.map(n => n.id)).toEqual([a1.id, a2.id]);
  });

  it('reports the active Mesh — and only for a Mesh-scoped View Mode', () => {
    const meshScope = scope({ viewMode: 'mesh', selectedMeshId: 20 });
    expect(meshScope.mesh).toEqual({ id: 20, name: 'beta' });
    expect(meshScope.isMeshScoped).toBe(true);

    // 'pinned' and 'filtered' are cross-Mesh by design (#983/#1609) and
    // 'all' clears the selection — so a sidebar selection never turns
    // one of them into a Mesh scope, even with the Mesh still selected.
    for (const viewMode of ['all', 'pinned', 'filtered'] as const) {
      const crossMesh = scope({ viewMode, selectedMeshId: 20 });
      expect(crossMesh.mesh).toBeNull();
      expect(crossMesh.isMeshScoped).toBe(false);
    }
  });

  it('counts the scope before the controls narrow it and what is visible now', () => {
    const filtered = scope({
      viewMode: 'filtered',
      controls: { gridSearchQuery: 'b1', gridProviderFilter: null, gridStatusFilter: null },
    });
    expect(filtered.scopedNodeCount).toBe(4);
    expect(filtered.visibleNodeCount).toBe(1);

    // A stale search must not narrow a Mesh-scoped view (#1609) — the
    // scope and the visible set stay the full Mesh.
    const meshScope = scope({
      viewMode: 'mesh',
      selectedMeshId: 20,
      controls: { gridSearchQuery: 'no such node', gridProviderFilter: null, gridStatusFilter: null },
    });
    expect(meshScope.scopedNodeCount).toBe(2);
    expect(meshScope.visibleNodeCount).toBe(2);
  });

  it("names the focused node's Mesh when it differs from the scope's", () => {
    // Single soloing another Mesh's node is the case this field exists
    // for: the grid scope is mesh 10, the soloed node lives in mesh 20.
    expect(scope({ viewMode: 'single', lastNonSingleMode: 'mesh', selectedMeshId: 10, activeNodeId: b1.id }).focusedNodeMesh)
      .toEqual({ id: 20, name: 'beta' });
    expect(scope({ viewMode: 'mesh', selectedMeshId: 10, activeNodeId: a2.id }).focusedNodeMesh).toBeNull();
    // A cross-Mesh scope has no Mesh of its own, so the focused node
    // always names the one it belongs to.
    expect(scope({ viewMode: 'all', activeNodeId: b2.id }).focusedNodeMesh).toEqual({ id: 20, name: 'beta' });
    expect(scope({ viewMode: 'all', activeNodeId: null }).focusedNodeMesh).toBeNull();
  });

  it('Mesh Grid with no selection has no Mesh and shows nothing, even with nodes loaded (#2071)', () => {
    // The focused node lives in mesh 20 and mesh 20 is loaded — the scope
    // is still empty. Nothing picks a Mesh on the user's behalf.
    const none = scope({ viewMode: 'mesh', selectedMeshId: null, activeNodeId: b1.id });
    expect(none.mesh).toBeNull();
    expect(none.isMeshScoped).toBe(false);
    expect(none.visibleNodes).toEqual([]);
    expect(none.visibleNodeCount).toBe(0);
    expect(none.scopedNodeCount).toBe(0);
  });

  it('Mesh Grid with no selection and no nodes loaded is the same empty scope (#2071)', () => {
    const none = scope({ viewMode: 'mesh', selectedMeshId: null, agentNodes: [], activeNodeId: null });
    expect(none.mesh).toBeNull();
    expect(none.isMeshScoped).toBe(false);
    expect(none.visibleNodes).toEqual([]);
    expect(none.visibleNodeCount).toBe(0);
  });

  it('reports the grid mode Single was entered from, without adopting the soloed node\'s Mesh', () => {
    // `lastNonSingleMode` is the Escape target; the Mesh Grid the user
    // returns to has no selection, so the scope stays empty — the
    // soloed node's Mesh is reported as a contribution, not as the scope.
    const single = scope({ viewMode: 'single', lastNonSingleMode: 'mesh', selectedMeshId: null, activeNodeId: b1.id });
    expect(single.viewMode).toBe('single');
    expect(single.gridMode).toBe('mesh');
    expect(single.mesh).toBeNull();
    expect(single.visibleNodes).toEqual([]);
    expect(single.focusedNodeMesh).toEqual({ id: 20, name: 'beta' });
  });

  it('reports a Mesh whose name is not loaded as id-only, never a guessed label', () => {
    const unloaded = scope({ viewMode: 'mesh', selectedMeshId: 30 });
    expect(unloaded.mesh).toEqual({ id: 30, name: null });
    expect(unloaded.isMeshScoped).toBe(true);
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

// ---------------------------------------------------------------------------
// #2070 review - the two questions more than one surface asks, kept as pure
// predicates in the derivation's own module so no consumer re-decides them.
//
// Before this, `scopeNotices` restated three `ScopeInput` fields as its own
// input type and called `deriveScope` with a fabricated `agentNodes: []` to
// read one boolean (correct only while `isMeshScoped` ignored nodes), and the
// Probe context resolver carried its own copy of the selection chain. Both
// now call the definition that `deriveScope` itself uses.
// ---------------------------------------------------------------------------

const SCOPE_CASES: Array<[ViewMode, NonSingleViewMode, number | null]> = [
  ['mesh', 'mesh', 10],
  ['mesh', 'mesh', null],
  ['all', 'all', 10],
  ['pinned', 'pinned', 10],
  ['filtered', 'filtered', 10],
  // Single reports the grid scope it was entered from, so a solo out of a
  // Mesh Grid is still a Mesh-anchored scope.
  ['single', 'mesh', 10],
  ['single', 'mesh', null],
  ['single', 'all', 10],
];

describe('isMeshScopedView', () => {
  it.each(SCOPE_CASES)(
    'viewMode %s over %s with selection %s ? %s',
    (viewMode, lastNonSingleMode, selectedMeshId) => {
      const expected = (viewMode === 'mesh' || (viewMode === 'single' && lastNonSingleMode === 'mesh'))
        && selectedMeshId !== null;
      expect(isMeshScopedView(viewMode, lastNonSingleMode, selectedMeshId)).toBe(expected);
    },
  );

  // The wiring that made `scopeNotices`' fabricated inputs "correct today":
  // the derivation must ANSWER with this predicate, or the predicate is a
  // second definition again.
  it.each(SCOPE_CASES)(
    'deriveScope reports the same Mesh-scoped answer for %s over %s with selection %s',
    (viewMode, lastNonSingleMode, selectedMeshId) => {
      const derived = scope({ viewMode, lastNonSingleMode, selectedMeshId });
      expect(derived.isMeshScoped).toBe(isMeshScopedView(viewMode, lastNonSingleMode, selectedMeshId));
      // A Mesh id and a Mesh-scoped scope are one fact, not two.
      expect(derived.mesh === null).toBe(!derived.isMeshScoped);
    },
  );
});

describe('selectionMeshId', () => {
  // This is a DESTINATION's subject, not the canvas scope: Pinned, Filtered
  // and All have no grid Mesh by construction, yet a Mesh-lens Probe
  // destination still acts on one. #2073 deleted the pins that could hold a
  // Mesh the user had not chosen, so the rule is "the chosen Mesh, else the
  // focused node's Mesh" - never a Mesh the user did not choose (#2071).
  it.each([
    ['all', 10],
    ['pinned', 10],
    ['filtered', 10],
    ['mesh', 10],
  ] as const)('keeps an explicitly selected Mesh authoritative in %s', (viewMode, expected) => {
    expect(selectionMeshId(viewMode, 10, 20)).toBe(expected);
  });

  it('lets a soloed node outrank the selection in Single - the solo IS the canvas there', () => {
    expect(selectionMeshId('single', 10, 20)).toBe(20);
  });

  it.each(['mesh', 'pinned', 'filtered', 'all'] as const)(
    "stands the focused node's Mesh in for %s when nothing is selected",
    (viewMode) => {
      expect(selectionMeshId(viewMode, null, 20)).toBe(20);
    },
  );

  it('is null when there is neither a selection nor a focused node', () => {
    for (const viewMode of ['all', 'pinned', 'filtered', 'mesh', 'single'] as const) {
      expect(selectionMeshId(viewMode, null, null)).toBeNull();
    }
  });
});