/**
 * Issue #2072 — clicking the Mesh you are already in does nothing.
 *
 * The gesture under test lives in `Sidebar` (the row delegates to
 * `handleSelectMesh`), so this suite drives the real `Sidebar` against
 * seeded mesh + agent-node stores and asserts only what a user can
 * observe afterwards: the Mesh selection and the View Mode the canvas
 * renders in. It inverts the pre-#2072 re-click-deselect case rather than
 * dropping it.
 *
 * `ViewModeSwitcher` is mounted alongside only for the All Nodes case,
 * because All Nodes is the one remaining route out of Mesh scope and the
 * claim under test is that the *gesture* clears the selection.
 *
 * Mock shape copied from `sidebar-mesh-bands.test.tsx`: the async per-mesh
 * hooks are pinned so no background fetch can move a row mid-assertion.
 * `useProviderList` stays real and is settled before asserting.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, act, waitFor, cleanup, fireEvent } from '@testing-library/react';
import { Sidebar } from '../../src/components/Sidebar/Sidebar';
import { ViewModeSwitcher } from '../../src/components/ViewModeSwitcher/ViewModeSwitcher';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useUIStore, type ViewMode } from '../../src/stores/uiStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';

vi.mock('../../src/hooks/useMeshHealth', () => ({
  useMeshHealth: () => ({ health: null, refresh: vi.fn() }),
}));

vi.mock('../../src/hooks/useGitBranchStatus', () => ({
  useGitBranchStatus: () => ({ branchStatus: null, refresh: vi.fn() }),
}));

vi.mock('../../src/hooks/useMeshGitHubUrl', () => ({
  useMeshGitHubUrl: () => ({ url: null, loading: false, refresh: vi.fn() }),
}));

function makeMesh(id: number, name: string, position: number): Mesh {
  return {
    id,
    name,
    path: `/repos/${name}`,
    layout: 'single',
    position,
    created_at: '2026-01-01',
    scratchpad: '',
    sandbox: false,
  } as Mesh;
}

const ALPHA = makeMesh(1, 'mesh-alpha', 0);
const BETA = makeMesh(2, 'mesh-beta', 1);
const GAMMA = makeMesh(3, 'mesh-gamma', 2);
const MESHES = [ALPHA, BETA, GAMMA];

/**
 * Seed the stores the way the app boots into a given scope: the Mesh
 * selection first (it flips the View Mode through the mesh→mode
 * subscription), then the View Mode the user is actually looking at.
 */
function seed(viewMode: ViewMode, selectedMeshId: number | null) {
  useMeshStore.setState({
    meshes: MESHES,
    meshesById: new Map(MESHES.map((m) => [m.id, m])),
    selectedMeshId: null,
  });
  seedAgentNodes([]);
  useAgentNodeStore.setState({ circuitOwnerships: {}, closingNodeIds: new Set(), error: null, activeNodeId: null });
  useUIStore.setState({ viewMode: 'all', lastNonSingleMode: 'all' });
  if (selectedMeshId !== null) useMeshStore.getState().selectMesh(selectedMeshId);
  useUIStore.setState({ viewMode, lastNonSingleMode: viewMode === 'single' ? 'all' : viewMode });
}

async function renderSidebar() {
  render(<Sidebar />);
  await waitFor(() => expect(screen.getByText('mesh-alpha')).toBeTruthy());
  for (let i = 0; i < 5; i++) {
    await act(async () => {});
  }
}

/** The row click a user makes — the mesh-name span inside the clickable header. */
function clickMeshRow(id: number) {
  const name = document.getElementById(`mesh-item-name-${id}`);
  if (!name) throw new Error(`mesh row ${id} not rendered`);
  fireEvent.click(name);
}

beforeEach(() => {
  cleanup();
});

afterEach(() => {
  cleanup();
});

describe('Sidebar Mesh selection gesture (issue #2072)', () => {
  it('re-clicking the highlighted Mesh while its grid is showing changes nothing', async () => {
    seed('mesh', GAMMA.id);
    expect(useUIStore.getState().viewMode).toBe('mesh');
    await renderSidebar();

    clickMeshRow(GAMMA.id);
    for (let i = 0; i < 5; i++) await act(async () => {});

    // Neither side of the scope moved: the selection survives and the
    // canvas stays on that Mesh's grid. Pre-#2072 this click cleared the
    // selection and bounced the user out to All Nodes.
    expect(useMeshStore.getState().selectedMeshId).toBe(GAMMA.id);
    expect(useUIStore.getState().viewMode).toBe('mesh');

    // Positive control in the same test: the row click is live, so the
    // assertions above are not passing against a dead handler.
    clickMeshRow(ALPHA.id);
    for (let i = 0; i < 5; i++) await act(async () => {});
    expect(useMeshStore.getState().selectedMeshId).toBe(ALPHA.id);
    expect(useUIStore.getState().viewMode).toBe('mesh');
  });

  // Pinned and Filtered keep the Mesh highlighted — the sidebar shows the
  // Mesh the user would come back to — so clicking it must return the
  // canvas to that Mesh's grid. Single is included deliberately: Single is
  // explicit focus on one Agent Node, not a Mesh scope.
  it.each(['pinned', 'filtered', 'single'] as const)(
    're-clicking the highlighted Mesh from %s returns the canvas to its Mesh Grid',
    async (mode) => {
      seed(mode, GAMMA.id);
      expect(useUIStore.getState().viewMode).toBe(mode);
      await renderSidebar();

      clickMeshRow(GAMMA.id);
      for (let i = 0; i < 5; i++) await act(async () => {});

      expect(useUIStore.getState().viewMode).toBe('mesh');
      // The Mesh is the same one, so the selection is untouched — the
      // mesh→mode subscription short-circuits on an unchanged id and the
      // View Mode setter is what actually restores Mesh Grid.
      expect(useMeshStore.getState().selectedMeshId).toBe(GAMMA.id);
    },
  );

  it('clicking a different Mesh selects it and returns the canvas to its Mesh Grid', async () => {
    seed('pinned', BETA.id);
    await renderSidebar();

    clickMeshRow(GAMMA.id);
    for (let i = 0; i < 5; i++) await act(async () => {});

    expect(useMeshStore.getState().selectedMeshId).toBe(GAMMA.id);
    expect(useUIStore.getState().viewMode).toBe('mesh');
  });

  it('picking a Mesh from All Nodes selects it and returns the canvas to its Mesh Grid', async () => {
    seed('all', null);
    await renderSidebar();

    clickMeshRow(BETA.id);
    for (let i = 0; i < 5; i++) await act(async () => {});

    expect(useMeshStore.getState().selectedMeshId).toBe(BETA.id);
    expect(useUIStore.getState().viewMode).toBe('mesh');
  });

  it('All Nodes is the way out of Mesh scope, and it clears the Mesh selection', async () => {
    seed('mesh', GAMMA.id);
    render(
      <>
        <Sidebar />
        <ViewModeSwitcher />
      </>,
    );
    await waitFor(() => expect(screen.getByText('mesh-alpha')).toBeTruthy());
    for (let i = 0; i < 5; i++) await act(async () => {});

    // Re-clicking the highlighted Mesh first: scope must still be held.
    clickMeshRow(GAMMA.id);
    for (let i = 0; i < 5; i++) await act(async () => {});
    expect(useMeshStore.getState().selectedMeshId).toBe(GAMMA.id);

    fireEvent.click(screen.getByRole('button', { name: /all nodes/i }));
    for (let i = 0; i < 5; i++) await act(async () => {});

    expect(useUIStore.getState().viewMode).toBe('all');
    expect(useMeshStore.getState().selectedMeshId).toBeNull();
  });
});
