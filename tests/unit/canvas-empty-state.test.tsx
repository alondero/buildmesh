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

function input(overrides: Partial<CanvasEmptyStateInput> = {}): CanvasEmptyStateInput {
  return {
    meshCount: 1,
    totalNodeCount: 0,
    scopedCount: 0,
    filteredCount: 0,
    viewMode: 'all',
    selectedMeshId: null,
    harnessReady: true,
    ...overrides,
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
    expect(branch(input({ viewMode: 'pinned', meshCount: 0, totalNodeCount: 0 }))).toBe('no-meshes');
    expect(branch(input({ viewMode: 'pinned', meshCount: 0, totalNodeCount: 3 }))).toBe('no-meshes');
  });

  it('pinned wins next — meshes exist but user is in pinned mode', () => {
    expect(branch(input({ viewMode: 'pinned', meshCount: 5, totalNodeCount: 3 }))).toBe('pinned-empty');
    expect(branch(input({ viewMode: 'pinned', meshCount: 1, totalNodeCount: 0 }))).toBe('pinned-empty');
  });

  it('no-meshes wins over everything when meshCount is 0', () => {
    expect(branch(input({ meshCount: 0, totalNodeCount: 0 }))).toBe('no-meshes');
    expect(branch(input({ meshCount: 0, totalNodeCount: 2 }))).toBe('no-meshes');
    expect(branch(input({ viewMode: 'mesh', meshCount: 0 }))).toBe('no-meshes');
    expect(branch(input({ viewMode: 'filtered', meshCount: 0, gridSearchQuery: 'x' }))).toBe('no-meshes');
  });

  it('returns selected-empty only when viewMode=mesh AND scopedCount=0 AND selectedMeshId is set', () => {
    const decision = classifyCanvasEmpty(
      input({ viewMode: 'mesh', scopedCount: 0, totalNodeCount: 3, selectedMeshId: 42 }),
    );
    expect(decision.branch).toBe('selected-empty');
    // The structured decision carries the mesh id — no `as number`
    // cast at the renderer call site (senior-review fix).
    expect(decision.branch === 'selected-empty' && decision.meshId).toBe(42);
  });

  it('mesh view without an explicit selection does NOT route to selected-empty (active-node-mesh fallback is unreachable)', () => {
    // `scopeNodesForMode` falls back to agentNodes[0].mesh_id when
    // selectedMeshId is null, so `scopedCount` is non-zero whenever
    // any agent exists. The mesh-without-selection + scopedCount=0
    // input is unreachable in production; the classifier correctly
    // routes through to other branches (filters-exclude-all when
    // filteredCount=0 and totalNodeCount>0).
    expect(
      branch(input({ viewMode: 'mesh', scopedCount: 0, totalNodeCount: 3, selectedMeshId: null })),
    ).toBe('filters-exclude-all');
  });

  it('does NOT return selected-empty when the all view has zero nodes — that is all-empty', () => {
    expect(branch(input({ viewMode: 'all', scopedCount: 0, totalNodeCount: 0 }))).toBe('all-empty');
  });

  it('returns all-empty when meshes exist but no nodes globally', () => {
    expect(branch(input({ meshCount: 2, totalNodeCount: 0 }))).toBe('all-empty');
  });

  it('returns filters-exclude-all when scoped nodes exist but none survive the filter', () => {
    expect(
      branch(input({ totalNodeCount: 5, scopedCount: 5, filteredCount: 0 })),
    ).toBe('filters-exclude-all');
  });
});

describe('CanvasEmptyState (issue #1536)', () => {
  it('no-meshes branch: heading + New mesh CTA that fires onCreateMesh', () => {
    const cbs = { ...noopCallbacks, onCreateMesh: vi.fn() };
    render(<CanvasEmptyState input={input({ meshCount: 0 })} callbacks={cbs} />);

    expect(screen.getByText('Buildmesh')).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-create-mesh'));
    expect(cbs.onCreateMesh).toHaveBeenCalledTimes(1);
  });

  it('all-empty branch: heading + Spawn agent CTA that fires onOpenSpawnMenu(null)', () => {
    const cbs = { ...noopCallbacks, onOpenSpawnMenu: vi.fn() };
    render(<CanvasEmptyState input={input({ meshCount: 1, totalNodeCount: 0 })} callbacks={cbs} />);

    expect(screen.getByText('No agents yet')).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-spawn-agent'));
    expect(cbs.onOpenSpawnMenu).toHaveBeenCalledWith(null);
  });

  it('selected-empty branch: heading + Spawn agent CTA that passes the selected mesh id', () => {
    const cbs = { ...noopCallbacks, onOpenSpawnMenu: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({ meshCount: 2, totalNodeCount: 3, viewMode: 'mesh', selectedMeshId: 42 })}
        callbacks={cbs}
      />,
    );

    expect(screen.getByText('No agents in this mesh')).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-spawn-agent'));
    expect(cbs.onOpenSpawnMenu).toHaveBeenCalledWith(42);
  });

  it('mesh view with selectedMeshId=null surfaces filters-exclude-all when nodes exist', () => {
    // Senior-review round 4: the previous mesh-without-selection
    // branch rendered "No agents yet" while 3 agents existed — a
    // blatant lie. The branch is gone; this case routes through to
    // `filters-exclude-all` (the documented "Clear filters" CTA),
    // which is at least truthful even though it assumes a filter
    // is active when it isn't (the input classifier only knows
    // `filteredCount` — the caller's grid controls know whether
    // filters are active, but that's not part of the input shape).
    const cbs = { ...noopCallbacks, onClearFilters: vi.fn(), onOpenSpawnMenu: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({ meshCount: 2, totalNodeCount: 3, scopedCount: 3, filteredCount: 0, viewMode: 'mesh', selectedMeshId: null })}
        callbacks={cbs}
      />,
    );

    expect(screen.queryByText('No agents in this mesh')).toBeNull();
    expect(screen.queryByText('No agents yet')).toBeNull();
    expect(screen.getByText('No nodes match')).toBeTruthy();
    expect(screen.getByTestId('canvas-empty-clear-filters')).toBeTruthy();
  });

  it('filters-exclude-all branch: Clear search & filters CTA fires onClearFilters', () => {
    const cbs = { ...noopCallbacks, onClearFilters: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({ meshCount: 2, totalNodeCount: 5, scopedCount: 5, filteredCount: 0 })}
        callbacks={cbs}
      />,
    );

    expect(screen.getByText('No nodes match')).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-clear-filters'));
    expect(cbs.onClearFilters).toHaveBeenCalledTimes(1);
  });

  // The Pinned-empty branch was previously covered by a standalone
  // test file against a now-removed `PinnedEmptyState` component.
  // Its contract now lives here — the same heading, body, and
  // "View All Nodes" CTA wired through the `onViewAll` callback
  // (so the parent's `setViewMode('all')` doesn't leak into the
  // component itself).
  it('pinned-empty branch: heading + body + View All Nodes CTA fires onViewAll', () => {
    const cbs = { ...noopCallbacks, onViewAll: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({ meshCount: 2, totalNodeCount: 5, viewMode: 'pinned' })}
        callbacks={cbs}
      />,
    );

    expect(screen.getByText('No pinned nodes')).toBeTruthy();
    expect(screen.getByText(/Pin agents from any mesh/)).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-view-all'));
    expect(cbs.onViewAll).toHaveBeenCalledTimes(1);
  });

  it('selected-empty + no harness: routes to Open Settings instead of Spawn', () => {
    const cbs = { ...noopCallbacks, onOpenSetup: vi.fn(), onOpenSpawnMenu: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({ meshCount: 2, totalNodeCount: 3, viewMode: 'mesh', selectedMeshId: 7, harnessReady: false })}
        callbacks={cbs}
      />,
    );

    expect(screen.getByText('No agents in this mesh')).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-open-setup'));
    expect(cbs.onOpenSetup).toHaveBeenCalledTimes(1);
    expect(cbs.onOpenSpawnMenu).not.toHaveBeenCalled();
  });

  it('all-empty + no harness: routes to Open Settings too', () => {
    const cbs = { ...noopCallbacks, onOpenSetup: vi.fn(), onOpenSpawnMenu: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({ meshCount: 2, totalNodeCount: 0, harnessReady: false })}
        callbacks={cbs}
      />,
    );

    fireEvent.click(screen.getByTestId('canvas-empty-open-setup'));
    expect(cbs.onOpenSetup).toHaveBeenCalledTimes(1);
    expect(cbs.onOpenSpawnMenu).not.toHaveBeenCalled();
  });
});
