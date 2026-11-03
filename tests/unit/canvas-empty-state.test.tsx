/**
 * Issue #1536 — CanvasEmptyState unit tests.
 *
 * The component is a pure renderer over a discriminated input shape and
 * five callbacks, so the tests can assert each branch's copy and CTA
 * target without mounting AgentNodeView or any store. The classifier
 * (`classifyCanvasEmpty`) is also tested directly so the decision
 * order is locked in independent of the render output — that catches
 * a future reorder mistake before it ships.
 *
 * The "production boundary" contract (issue #1536's verification spec)
 * is: clicking a CTA fires the matching callback with the right mesh
 * id. The callback assertions are the test for "New Mesh opens the
 * MeshCreateModal" / "Spawn opens the Spawn Menu" — those side-effects
 * live above the component and are wired by AgentNodeView (covered by
 * the integration tests in agent-node-view-grid-controls.test.tsx).
 *
 * The Pinned-empty branch's contract (which previously lived in a
 * standalone `pinned-empty-state.test.tsx` against a now-removed
 * `PinnedEmptyState` component) is covered here against the canonical
 * `CanvasEmptyState` implementation.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import {
  CanvasEmptyState,
  classifyCanvasEmpty,
  type CanvasEmptyStateCallbacks,
  type CanvasEmptyStateInput,
} from '../../src/components/AgentNodeView/CanvasEmptyState';
import { deriveScope, resolveSingleNode, type FilterControls } from '../../src/lib/viewModes';
import type { NonSingleViewMode, ViewMode } from '../../src/stores/uiStore';
import type { AgentNode } from '../../src/types/generated/AgentNode';

const noopCallbacks: CanvasEmptyStateCallbacks = {
  onCreateMesh: vi.fn(),
  onOpenSpawnMenu: vi.fn(),
  onClearFilters: vi.fn(),
  onOpenSetup: vi.fn(),
  onViewAll: vi.fn(),
};

beforeEach(() => {
  vi.clearAllMocks();
});

/** A node in some Mesh. The fields the scope derivation and the grid
 *  filters read are the ones that matter here — `mesh_id`, `name`,
 *  `provider`, `status` — so the rest are the generated type's defaults. */
function nodeInMesh(meshId: number, overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 1,
    mesh_id: meshId,
    name: 'agent-1',
    path: '/repo/agent-1',
    branch: 'main',
    env: 'wsl',
    provider: 'claude',
    status: 'running',
    cli_session_id: null,
    worktree_name: null,
    use_worktree: false,
    is_pinned: false,
    source_issue: null,
    source_pr: null,
    head_repo_owner: null,
    head_repo_clone_url: null,
    source_pr_pinned_sha: null,
    signal_health: null,
    position: 0,
    created_at: '2026-07-16T00:00:00Z',
    worktree_path: null,
    ...overrides,
  };
}

/** `count` nodes in one Mesh, with ids unique inside the list so the store
 *  reads as one store rather than as a pile of same-id rows. `firstId` keeps
 *  the ids deterministic, so a fixture that adds nodes to a second Mesh can
 *  continue the numbering instead of colliding with it. */
function nodesInMesh(meshId: number, count: number, firstId = 1): AgentNode[] {
  return Array.from({ length: count }, (_, i) =>
    nodeInMesh(meshId, { id: firstId + i, position: i, name: `agent-${firstId + i}` }));
}

/** Overrides the caller states the way the stores hold them — the View Mode
 *  and the sidebar selection — plus the Mesh count. `scope` is DERIVED here
 *  from the node list (#2070 review), so a test can never hand the classifier
 *  a View Mode and a Mesh identity that the derivation would not produce. */
interface InputOverrides {
  /** The Mesh store's size. It is the one count the node list cannot back —
   *  a Mesh can exist with no nodes and nodes can outlive their Mesh — so it
   *  stays an explicit input. */
  meshCount?: number;
  viewMode?: ViewMode;
  lastNonSingleMode?: NonSingleViewMode;
  selectedMeshId?: number | null;
  harnessReady?: boolean;
  /** The store's node list, which the scope is derived from. Empty by
   *  default, which is the same scope every Mesh-scoped case here needs. */
  agentNodes?: AgentNode[];
  /** The focused node's id — STALE when that node has been deleted. */
  activeNodeId?: number | null;
  /** The active Grid Controls, which narrow the scope's visible nodes.
   *  Production derives the scope WITH them (`useScope` reads the UI store's
   *  controls and passes them to `deriveScope`), so a fixture that wants
   *  "the scope holds nodes the filters hide" states them here. */
  controls?: FilterControls;
}

function input(overrides: InputOverrides = {}): CanvasEmptyStateInput {
  const {
    meshCount = 1,
    viewMode = 'all',
    lastNonSingleMode = viewMode === 'single' ? 'mesh' : viewMode,
    selectedMeshId = null,
    harnessReady = true,
    agentNodes = [],
    activeNodeId = null,
    controls,
  } = overrides;
  // The classifier reads the Mesh identity from the derived scope, never
  // from `viewMode`/`selectedMeshId` re-tested here (#2071's one
  // derivation).
  const scope = deriveScope({
    viewMode,
    lastNonSingleMode,
    agentNodes,
    selectedMeshId,
    activeNodeId,
    controls,
  });
  return {
    meshCount,
    // BOTH counts come from the same store array the production container
    // reads: `totalNodeCount` is `agentNodes.length` and `scopedCount` is the
    // derived scope's own count (`CanvasEmptyStateContainer` passes
    // `scope.scopedNodeCount`). Neither is an override, so a test can never
    // hand the classifier a count its store does not hold — and a regression
    // in how the count is derived fails a test here instead of passing.
    totalNodeCount: agentNodes.length,
    scopedCount: scope.scopedNodeCount,
    harnessReady,
    scope,
  };
}

describe('classifyCanvasEmpty (issue #1536)', () => {
  // `.branch` accessor — the classifier returns a structured
  // `CanvasEmptyDecision` (each branch carries its required data,
  // e.g. `selected-empty` carries `meshId: number`). Asserting on
  // the string discriminator alone (without `as number` casts in
  // the renderer) is the type-safe shape.
  const branch = (i: Parameters<typeof classifyCanvasEmpty>[0]) => classifyCanvasEmpty(i).branch;

  // Senior-review round 4: the precedence is now
  //   no-meshes → pinned → selected-empty → all-empty → filters-exclude-all
  // (no-meshes wins over pinned, brand-new user with 0 meshes in
  // pinned mode gets the New mesh CTA, not the "Pin agents from any
  // mesh" CTA which doesn't apply).
  it('no-meshes wins over pinned for a fresh user (0 meshes in pinned mode)', () => {
    expect(branch(input({ viewMode: 'pinned', meshCount: 0 }))).toBe('no-meshes');
    // Even with nodes loaded — the Mesh store can be empty while the node
    // store is not — the branch is the same, because "Add a repository" is
    // the only CTA that can fix it.
    expect(
      branch(input({ viewMode: 'pinned', meshCount: 0, agentNodes: nodesInMesh(1, 3) })),
    ).toBe('no-meshes');
  });

  it('pinned wins next — meshes exist but user is in pinned mode', () => {
    // Three nodes are loaded and none of them is pinned, so the pinned
    // scope holds nothing: the branch is the Pinned CTA, not "no agents".
    expect(
      branch(input({ viewMode: 'pinned', meshCount: 5, agentNodes: nodesInMesh(1, 3) })),
    ).toBe('pinned-empty');
    expect(branch(input({ viewMode: 'pinned', meshCount: 1 }))).toBe('pinned-empty');
  });

  it('no-meshes wins over everything when meshCount is 0', () => {
    expect(branch(input({ meshCount: 0 }))).toBe('no-meshes');
    expect(branch(input({ meshCount: 0, agentNodes: nodesInMesh(1, 2) }))).toBe('no-meshes');
    expect(branch(input({ viewMode: 'mesh', meshCount: 0 }))).toBe('no-meshes');
    expect(branch(input({ viewMode: 'all', meshCount: 0 }))).toBe('no-meshes');
  });

  it('returns selected-empty only when viewMode=mesh AND scopedCount=0 AND selectedMeshId is set', () => {
    // Three nodes are loaded, all in another Mesh: Mesh 42 holds nothing, so
    // the scope's own count is 0 and the branch is that Mesh's own CTA.
    const decision = classifyCanvasEmpty(
      input({ viewMode: 'mesh', selectedMeshId: 42, agentNodes: nodesInMesh(7, 3) }),
    );
    expect(decision.branch).toBe('selected-empty');
    // The structured decision carries the mesh id — no `as number`
    // cast at the renderer call site (senior-review fix).
    expect(decision.branch === 'selected-empty' && decision.meshId).toBe(42);
  });

  it('routes Mesh Grid with no selection to the explicit no-mesh-selected branch (#2071)', () => {
    // #2071 removed the Mesh-scope fallback chain, so Mesh Grid with no
    // selection is now a reachable production state — and it is exactly
    // the case the old "clear your filters" CTA lied about. Naming the
    // missing selection is the honest answer, with or without nodes
    // loaded elsewhere in the app.
    expect(
      branch(input({ viewMode: 'mesh', selectedMeshId: null, agentNodes: nodesInMesh(7, 3) })),
    ).toBe('no-mesh-selected');
    expect(
      branch(input({ viewMode: 'mesh', selectedMeshId: null })),
    ).toBe('no-mesh-selected');
  });

  it('does NOT return selected-empty when the all view has zero nodes — that is all-empty', () => {
    expect(branch(input({ viewMode: 'all' }))).toBe('all-empty');
  });

  it('returns all-empty when meshes exist but no nodes globally', () => {
    expect(branch(input({ meshCount: 2 }))).toBe('all-empty');
  });

  it('returns filters-exclude-all when the scope holds nodes the filters hide', () => {
    // The classifier reads no narrowed count: the caller only classifies an
    // EMPTY canvas, so "nothing in the scope is on it" is the fallback and
    // this branch names it. The scope usually holds nodes the filters hide
    // — but not always; see the Single case below.
    //
    // The fixture states that state rather than asserting it: five nodes are
    // loaded and in scope, and the active search hides every one of them, so
    // the scope holds five while the grid shows none. The counts are the
    // scope's own, so a regression in how they are derived fails here.
    const store = [...nodesInMesh(1, 3), ...nodesInMesh(2, 2, 4)];
    const hidden = input({
      meshCount: 2,
      agentNodes: store,
      controls: { gridSearchQuery: 'no-such-node', gridProviderFilter: null, gridStatusFilter: null },
    });
    expect(hidden.scope.scopedNodeCount).toBe(5);
    expect(hidden.scope.visibleNodeCount).toBe(0);
    expect(hidden.totalNodeCount).toBe(5);
    expect(hidden.scopedCount).toBe(5);
    expect(branch(hidden)).toBe('filters-exclude-all');
  });

  it('reaches filters-exclude-all in Single when the scope has nothing left to show', () => {
    // A soloed node deleted while its Mesh is empty, with a search active:
    // `resolveSingleNode` finds nothing and refuses to broaden past the
    // filter, so the canvas is empty while the scope holds no node at all.
    // The branch is still the right one — the filters are what keep the
    // scope empty — but it is reached with scopedCount === 0, so the
    // classifier must not claim the scope holds nodes.
    //
    // The reachability is asserted, not just described. The store below
    // holds three nodes, every one of them in another Mesh: the soloed
    // node is gone, so its id resolves to nothing, and Mesh 42 holds no
    // node either. The active search is therefore the ONLY thing that
    // empties the scope — a fallback that broadened past the filter would
    // have a node to return, and the assertion below rules that out.
    const store = [
      nodeInMesh(7, { id: 1, position: 0, name: 'alpha' }),
      nodeInMesh(7, { id: 2, position: 1, name: 'beta' }),
      nodeInMesh(9, { id: 3, position: 0, name: 'gamma' }),
    ];
    const searchActive = {
      gridSearchQuery: 'no-such-node',
      gridProviderFilter: null,
      gridStatusFilter: null,
    };
    // The soloed node was deleted: its id no longer resolves.
    expect(resolveSingleNode(store, 999, 'mesh', 42, searchActive)).toBeNull();
    // Without the search the fallback still finds a node to solo, so this
    // state is filter-dependent and not merely "Mesh 42 is empty".
    expect(resolveSingleNode(store, 999, 'mesh', 42)).toBe(store[0]);

    const single = input({
      viewMode: 'single', lastNonSingleMode: 'mesh', selectedMeshId: 42,
      agentNodes: store, activeNodeId: 999,
      meshCount: 3,
    });
    expect(single.scope.gridMode).toBe('mesh');
    expect(single.scope.isMeshScoped).toBe(true);
    // The scope holds no node at all, and the count the classifier is given
    // is that scope's own count rather than a number written by hand.
    expect(single.scope.scopedNodeCount).toBe(0);
    expect(single.totalNodeCount).toBe(3);
    expect(single.scopedCount).toBe(0);
    expect(classifyCanvasEmpty(single).branch).toBe('filters-exclude-all');
  });

  // #2070 review — the classifier now reads the derived scope instead of
  // re-testing `viewMode === 'mesh'` against `selectedMeshId`. That makes the
  // two questions the derivation already separates ("what is the grid mode?"
  // and "is there a Mesh in scope?") visible here: in Single the grid scope
  // can still BE a Mesh while the canvas shows one soloed node, and claiming
  // that Mesh is empty would be a lie. So the Mesh branches stay keyed on the
  // View Mode the user set, and the Mesh identity comes from the scope.
  it('does not call a Mesh empty when the scope is Mesh-scoped only because Single was entered from a Mesh grid', () => {
    const single = input({ viewMode: 'single', lastNonSingleMode: 'mesh', selectedMeshId: 42 });
    expect(single.scope.isMeshScoped).toBe(true);
    expect(single.scope.mesh).toEqual({ id: 42, name: null });
    expect(classifyCanvasEmpty(single).branch).toBe('all-empty');
  });

  it("does not claim 'no mesh selected' in Single over an empty Mesh grid — that state is Mesh Grid's own", () => {
    const single = input({ viewMode: 'single', lastNonSingleMode: 'mesh', selectedMeshId: null });
    expect(single.scope.gridMode).toBe('mesh');
    expect(single.scope.isMeshScoped).toBe(false);
    expect(classifyCanvasEmpty(single).branch).toBe('all-empty');
  });
});

describe('CanvasEmptyState (issue #1536)', () => {
  it('no-meshes branch: heading + New mesh CTA that fires onCreateMesh', () => {
    const cbs = { ...noopCallbacks, onCreateMesh: vi.fn() };
    render(<CanvasEmptyState input={input({ meshCount: 0 })} callbacks={cbs} />);

    expect(screen.getByRole('heading', { name: 'Add a repository' })).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-create-mesh'));
    expect(cbs.onCreateMesh).toHaveBeenCalledTimes(1);
  });

  it('all-empty branch: heading + Spawn agent CTA that fires onOpenSpawnMenu(null)', () => {
    const cbs = { ...noopCallbacks, onOpenSpawnMenu: vi.fn() };
    render(<CanvasEmptyState input={input({ meshCount: 1 })} callbacks={cbs} />);

    expect(screen.getByText('No agents yet')).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-spawn-agent'));
    expect(cbs.onOpenSpawnMenu).toHaveBeenCalledWith(null);
  });

  it('selected-empty branch: heading + Spawn agent CTA that passes the selected mesh id', () => {
    const cbs = { ...noopCallbacks, onOpenSpawnMenu: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({ meshCount: 2, viewMode: 'mesh', selectedMeshId: 42, agentNodes: nodesInMesh(7, 3) })}
        callbacks={cbs}
      />,
    );

    expect(screen.getByText('No agents in this mesh')).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-spawn-agent'));
    expect(cbs.onOpenSpawnMenu).toHaveBeenCalledWith(42);
  });

  it('no-mesh-selected branch names the missing selection and offers the All Nodes way out (#2071)', () => {
    // Previously this state rendered "No nodes match" with a Clear-filters
    // CTA — a lie, since no filter was active. The branch states what is
    // missing and hands the user a real way out of Mesh Grid.
    const cbs = { ...noopCallbacks, onViewAll: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({ meshCount: 2, viewMode: 'mesh', selectedMeshId: null, agentNodes: nodesInMesh(7, 3) })}
        callbacks={cbs}
      />,
    );

    expect(screen.getByText('No mesh selected')).toBeTruthy();
    expect(screen.queryByText('No nodes match')).toBeNull();
    expect(screen.queryByText('No agents in this mesh')).toBeNull();
    fireEvent.click(screen.getByTestId('canvas-empty-view-all'));
    expect(cbs.onViewAll).toHaveBeenCalledTimes(1);
  });

  it('filters-exclude-all branch: Clear search & filters CTA fires onClearFilters', () => {
    // Five nodes in scope, all hidden by the active search — the same state
    // the classifier test above names, reached through the renderer.
    const cbs = { ...noopCallbacks, onClearFilters: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({
          meshCount: 2,
          agentNodes: [...nodesInMesh(1, 3), ...nodesInMesh(2, 2, 4)],
          controls: { gridSearchQuery: 'no-such-node', gridProviderFilter: null, gridStatusFilter: null },
        })}
        callbacks={cbs}
      />,
    );

    expect(screen.getByText('No nodes match')).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-clear-filters'));
    expect(cbs.onClearFilters).toHaveBeenCalledTimes(1);
  });

  // The Pinned-empty branch was previously covered by a standalone
  // test file against a now-removed `PinnedEmptyState` component.
  // Its contract now lives here — the same heading and
  // "View All Nodes" CTA wired through the `onViewAll` callback
  // (so the parent's `setViewMode('all')` doesn't leak into the
  // component itself).
  it('pinned-empty branch: heading + View All Nodes CTA fires onViewAll', () => {
    const cbs = { ...noopCallbacks, onViewAll: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({
          meshCount: 2,
          viewMode: 'pinned',
          agentNodes: [...nodesInMesh(1, 3), ...nodesInMesh(2, 2, 4)],
        })}
        callbacks={cbs}
      />,
    );

    expect(screen.getByText('No pinned nodes')).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-view-all'));
    expect(cbs.onViewAll).toHaveBeenCalledTimes(1);
  });

  it('selected-empty + no harness: routes to Open Settings instead of Spawn', () => {
    const cbs = { ...noopCallbacks, onOpenSetup: vi.fn(), onOpenSpawnMenu: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({
          meshCount: 2,
          viewMode: 'mesh',
          selectedMeshId: 7,
          harnessReady: false,
          agentNodes: nodesInMesh(9, 3),
        })}
        callbacks={cbs}
      />,
    );

    expect(screen.getByRole('heading', { name: 'Set up an agent' })).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-open-setup'));
    expect(cbs.onOpenSetup).toHaveBeenCalledTimes(1);
    expect(cbs.onOpenSpawnMenu).not.toHaveBeenCalled();
  });

  it('all-empty + no harness: routes to Open Settings too', () => {
    const cbs = { ...noopCallbacks, onOpenSetup: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({ meshCount: 2, harnessReady: false })}
        callbacks={cbs}
      />,
    );

    fireEvent.click(screen.getByTestId('canvas-empty-open-setup'));
    expect(cbs.onOpenSetup).toHaveBeenCalledTimes(1);
  });
});
