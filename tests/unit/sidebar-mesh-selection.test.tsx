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
import { executeOmnibarItem } from '../../src/components/CommandOmnibar/omnibarActions';
import { seedAgentNodes } from './helpers/seedAgentNodes';

// Mutable so the Worktree Manager destination can be exercised: the drift
// badge that opens it only renders when the Mesh reports drift, which the
// default `null` health suppresses.
const meshHealth = vi.hoisted(() => ({ value: null as { status: string } | null }));

vi.mock('../../src/hooks/useMeshHealth', () => ({
  useMeshHealth: () => ({ health: meshHealth.value, refresh: vi.fn() }),
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

/**
 * #2081 review — "enter Mesh scope for Mesh X" is one store operation, and
 * every entrypoint that names a Mesh must go through it. The sidebar's
 * context-menu Probe destinations used to call bare `selectMesh`, so the same
 * intent landed differently depending on where the user clicked: from the
 * command palette, opening a Mesh's properties while the canvas sat in
 * Pinned returned to that Mesh's grid; from the sidebar it did not.
 *
 * "Identical" here is concrete and user-observable — the three facts a user
 * can see after the gesture: which Mesh is selected, which View Mode the
 * canvas renders in, and which Probe tab is showing. This suite pins the
 * sidebar half against the palette half on all three.
 */
describe('Sidebar Probe destinations agree with the palette about Mesh scope (#2081)', () => {
  /** What the sidebar's context-menu **Properties** entry did, read the same
   *  way a user reads it: the selection, the View Mode, the open tab. */
  function sidebarOutcome() {
    return {
      selectedMeshId: useMeshStore.getState().selectedMeshId,
      viewMode: useUIStore.getState().viewMode,
      probeTab: useUIStore.getState().probeTab,
    };
  }

  /** Open the sidebar's right-click menu on a Mesh row — the real gesture,
   *  through the real MeshItem menu. */
  function openMeshMenu(meshId: number) {
    const row = document.getElementById(`mesh-item-name-${meshId}`);
    if (!row) throw new Error(`mesh row ${meshId} not rendered`);
    fireEvent.contextMenu(row.parentElement!);
  }

  /** Open the sidebar's right-click menu on a Mesh row and choose
   *  **Properties** — the real gesture, through the real MeshItem menu. */
  async function openPropertiesFromSidebar(meshId: number) {
    await renderSidebar();
    openMeshMenu(meshId);
    fireEvent.click(screen.getByRole('menuitem', { name: /properties/i }));
    for (let i = 0; i < 5; i++) await act(async () => {});
  }

  it('lands on the same scope from the sidebar as the command palette does', async () => {
    // --- sidebar half ---
    seed('pinned', GAMMA.id);
    expect(useUIStore.getState().viewMode).toBe('pinned');
    await openPropertiesFromSidebar(GAMMA.id);
    const fromSidebar = sidebarOutcome();

    cleanup();
    vi.clearAllMocks();

    // --- palette half: the same Mesh's properties, same starting scope ---
    seed('pinned', GAMMA.id);
    expect(useUIStore.getState().viewMode).toBe('pinned');
    act(() => {
      executeOmnibarItem(`probe-in-mesh:properties:${GAMMA.id}`, {
        meshes: MESHES,
        spawnOptions: [],
        setViewMode: useUIStore.getState().setViewMode,
        openProbeTab: useUIStore.getState().openProbeTab,
      });
    });
    const fromPalette = sidebarOutcome();

    // The parity claim, stated as literals rather than as "the two agree" —
    // an assertion that only compared the two could pass on two wrong
    // answers. Pinned + the same Mesh selected is the exact state the
    // pre-#2081 sidebar silently left on Pinned.
    expect(fromSidebar).toEqual({
      selectedMeshId: GAMMA.id,
      viewMode: 'mesh',
      probeTab: 'properties',
    });
    expect(fromPalette).toEqual(fromSidebar);
  });

  // The other three destinations carry the same Mesh id for the same reason
  // (a Mesh-lens tab resolves its subject from `selectedMeshId`), so they
  // move together. Driven through the real gestures, not by calling the
  // handlers: Properties and GitHub Issues are context-menu entries,
  // Archive is too, and the Worktree Manager is the drift badge (which needs
  // a drifted Mesh before it renders).
  it.each([
    ['issues', 'GitHub Issues', 'menuitem'],
    ['sessions', 'Archive', 'menuitem'],
  ] as const)(
    'the sidebar %s destination enters Mesh scope like the palette one',
    async (tab, menuLabel, role) => {
      seed('pinned', GAMMA.id);
      await renderSidebar();
      openMeshMenu(GAMMA.id);
      fireEvent.click(screen.getByRole(role, { name: new RegExp(menuLabel, 'i') }));
      for (let i = 0; i < 5; i++) await act(async () => {});

      expect(useMeshStore.getState().selectedMeshId).toBe(GAMMA.id);
      expect(useUIStore.getState().viewMode).toBe('mesh');
      expect(useUIStore.getState().probeTab).toBe(tab);
    },
  );

  it('the sidebar Worktree Manager badge enters Mesh scope too', async () => {
    // The drift badge is a Mesh-scoped destination like the menu entries, so
    // it must move the canvas the same way — otherwise the one gesture a
    // user reaches for *because* their Mesh is broken leaves the canvas
    // cross-Mesh.
    meshHealth.value = { status: 'drift' };
    try {
      seed('pinned', GAMMA.id);
      await renderSidebar();
      // One badge per drifted row, so name the Mesh rather than the pattern.
      fireEvent.click(screen.getByRole('button', { name: `Mesh health issue for ${GAMMA.name}` }));
      for (let i = 0; i < 5; i++) await act(async () => {});

      expect(useMeshStore.getState().selectedMeshId).toBe(GAMMA.id);
      expect(useUIStore.getState().viewMode).toBe('mesh');
      expect(useUIStore.getState().probeTab).toBe('worktrees');
    } finally {
      meshHealth.value = null;
    }
  });
});
