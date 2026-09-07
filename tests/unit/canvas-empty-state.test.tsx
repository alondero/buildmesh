/**
 * Issue #1536 — CanvasEmptyState unit tests.
 *
 * The component is a pure renderer over a discriminated input shape and
 * four callbacks, so the tests can assert each branch's copy and CTA
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
    harnessReady: true,
    ...overrides,
  };
}

describe('classifyCanvasEmpty (issue #1536)', () => {
  it('returns pinned-empty whenever the view mode is pinned', () => {
    expect(classifyCanvasEmpty(input({ viewMode: 'pinned', meshCount: 0 }))).toBe('pinned-empty');
    expect(classifyCanvasEmpty(input({ viewMode: 'pinned', meshCount: 5, totalNodeCount: 3 }))).toBe('pinned-empty');
  });

  it('returns no-meshes when no meshes exist (regardless of nodes)', () => {
    expect(classifyCanvasEmpty(input({ meshCount: 0, totalNodeCount: 0 }))).toBe('no-meshes');
    // Defensive — orphan nodes shouldn't be possible, but if they
    // were, no-meshes still wins so the user can add a mesh to host them.
    expect(classifyCanvasEmpty(input({ meshCount: 0, totalNodeCount: 2 }))).toBe('no-meshes');
  });

  it('returns selected-empty only when viewMode=mesh AND that mesh has zero nodes', () => {
    expect(classifyCanvasEmpty(input({ viewMode: 'mesh', scopedCount: 0, totalNodeCount: 3 }))).toBe('selected-empty');
  });

  it('does NOT return selected-empty when the all view has zero nodes — that is all-empty', () => {
    // viewMode=all with scopedCount=0 must surface as all-empty so
    // the CTA offers a spawn action into any mesh (vs the no-meshes
    // branch which offers to create a mesh).
    expect(classifyCanvasEmpty(input({ viewMode: 'all', scopedCount: 0, totalNodeCount: 0 }))).toBe('all-empty');
  });

  it('returns all-empty when meshes exist but no nodes globally', () => {
    expect(classifyCanvasEmpty(input({ meshCount: 2, totalNodeCount: 0 }))).toBe('all-empty');
  });

  it('returns filters-exclude-all when scoped nodes exist but none survive the filter', () => {
    expect(
      classifyCanvasEmpty(input({ totalNodeCount: 5, scopedCount: 5, filteredCount: 0 })),
    ).toBe('filters-exclude-all');
  });
});

describe('CanvasEmptyState (issue #1536)', () => {
  it('no-meshes branch: heading + New mesh CTA that fires onCreateMesh', () => {
    const cbs = { ...noopCallbacks, onCreateMesh: vi.fn() };
    render(<CanvasEmptyState input={input({ meshCount: 0 })} callbacks={cbs} selectedMeshId={null} />);

    expect(screen.getByText('Buildmesh')).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-create-mesh'));
    expect(cbs.onCreateMesh).toHaveBeenCalledTimes(1);
  });

  it('all-empty branch: heading + Spawn agent CTA that fires onOpenSpawnMenu(null)', () => {
    const cbs = { ...noopCallbacks, onOpenSpawnMenu: vi.fn() };
    render(<CanvasEmptyState input={input({ meshCount: 1, totalNodeCount: 0 })} callbacks={cbs} selectedMeshId={null} />);

    expect(screen.getByText('No agents yet')).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-spawn-agent'));
    expect(cbs.onOpenSpawnMenu).toHaveBeenCalledWith(null);
  });

  it('selected-empty branch: heading + Spawn agent CTA that passes the selected mesh id', () => {
    const cbs = { ...noopCallbacks, onOpenSpawnMenu: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({ meshCount: 2, totalNodeCount: 3, viewMode: 'mesh' })}
        callbacks={cbs}
        selectedMeshId={42}
      />,
    );

    expect(screen.getByText('No agents in this mesh')).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-spawn-agent'));
    expect(cbs.onOpenSpawnMenu).toHaveBeenCalledWith(42);
  });

  it('filters-exclude-all branch: Clear search & filters CTA fires onClearFilters', () => {
    const cbs = { ...noopCallbacks, onClearFilters: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({ meshCount: 2, totalNodeCount: 5, scopedCount: 5, filteredCount: 0 })}
        callbacks={cbs}
        selectedMeshId={null}
      />,
    );

    expect(screen.getByText('No nodes match')).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-clear-filters'));
    expect(cbs.onClearFilters).toHaveBeenCalledTimes(1);
  });

  it('pinned-empty branch: View All Nodes CTA fires onViewAll', () => {
    const cbs = { ...noopCallbacks, onViewAll: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({ meshCount: 2, totalNodeCount: 5, viewMode: 'pinned' })}
        callbacks={cbs}
        selectedMeshId={null}
      />,
    );

    expect(screen.getByText('No pinned nodes')).toBeTruthy();
    fireEvent.click(screen.getByTestId('canvas-empty-view-all'));
    expect(cbs.onViewAll).toHaveBeenCalledTimes(1);
  });

  it('selected-empty + no harness: routes to Open Settings instead of Spawn', () => {
    // The harness-ready=false branch must route the CTA to Setup so a
    // user with no installed CLI / keyed provider isn't stranded on a
    // Spawn button that opens an empty menu.
    const cbs = { ...noopCallbacks, onOpenSetup: vi.fn(), onOpenSpawnMenu: vi.fn() };
    render(
      <CanvasEmptyState
        input={input({ meshCount: 2, totalNodeCount: 3, viewMode: 'mesh', harnessReady: false })}
        callbacks={cbs}
        selectedMeshId={7}
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
        selectedMeshId={null}
      />,
    );

    fireEvent.click(screen.getByTestId('canvas-empty-open-setup'));
    expect(cbs.onOpenSetup).toHaveBeenCalledTimes(1);
    expect(cbs.onOpenSpawnMenu).not.toHaveBeenCalled();
  });
});
