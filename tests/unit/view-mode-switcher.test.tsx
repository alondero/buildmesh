/**
 * ViewModeSwitcher (wayfinder #982 / #983 / #986; Filtered #1609; Mesh Grid
 * fallback deleted by #2071) — the five-segment control in the canvas header
 * that drives uiStore.viewMode. Pins the segment rendering, ARIA semantics,
 * and the deliberate sidebar-sync round-trips (All clears the selection the
 * same way the sidebar re-click-deselect does). Since #2071 the Mesh Grid
 * segment only sets the mode — it never picks a Mesh for the user, so the
 * segment lands in the Mesh Grid's "no Mesh selected" empty state when
 * nothing is selected. The Filtered segment additionally arms the
 * focus-grid-search request so the first click lands the user in the
 * search box.
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { ViewModeSwitcher } from '../../src/components/ViewModeSwitcher/ViewModeSwitcher';
import { ScopeIndicator } from '../../src/components/TitleBar/ScopeIndicator';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';

const MESH_1: Mesh = {
  id: 1, name: 'demo-1', path: '/repo/1', branch: 'main', position: 0,
  color: null, layout: 'grid', use_worktree: false, created_at: '2026-01-01',
  github_owner: null, github_repo: null, github_last_synced: null, pre_spawn_pool_size: 0,
};
const MESH_2: Mesh = { ...MESH_1, id: 2, name: 'demo-2', path: '/repo/2', position: 1 };

const NODE_A: AgentNode = {
  id: 1, mesh_id: 1, name: 'agent-a', path: '/repo/1', branch: 'main',
  env: 'wsl', provider: 'claude', status: 'running', cli_session_id: null,
  use_worktree: false, source_issue: null, worktree_name: null,
  created_at: '2026-01-01', position: 0, scratchpad: '', sandbox: false,
  source_pr: null, head_repo_owner: null, head_repo_clone_url: null,
  source_pr_pinned_sha: null, is_pinned: false, archived: false,
};
const NODE_B: AgentNode = { ...NODE_A, id: 2, mesh_id: 2, name: 'agent-b' };

beforeEach(() => {
  // meshStore FIRST: the uiStore mesh-subscription fires synchronously
  // and would otherwise clobber the viewMode set below.
  useMeshStore.setState({
    meshes: [],
    meshesById: new Map(),
    selectedMeshId: null,
    loading: false,
    error: null,
  });
  useUIStore.setState({ viewMode: 'all', lastNonSingleMode: 'all', focusGridSearchRequest: 0 });
});

describe('ViewModeSwitcher (wayfinder #982 / #983 / #986)', () => {
  describe('rendering', () => {
    it('renders one segment per ViewMode under a role="group" with the canonical aria-label', () => {
      render(<ViewModeSwitcher />);
      const group = screen.getByRole('group', { name: /view mode/i });
      expect(group).toBeTruthy();
      expect(screen.getByRole('button', { name: /single/i })).toBeTruthy();
      expect(screen.getByRole('button', { name: /mesh grid/i })).toBeTruthy();
      expect(screen.getByRole('button', { name: /pinned/i })).toBeTruthy();
      expect(screen.getByRole('button', { name: /all nodes/i })).toBeTruthy();
      expect(screen.getByRole('button', { name: /filtered/i })).toBeTruthy();
    });

    it('marks exactly the active segment with aria-pressed=true', () => {
      useUIStore.setState({ viewMode: 'pinned' });
      render(<ViewModeSwitcher />);
      const segments = ['Single', 'Mesh Grid', 'Pinned', 'All Nodes', 'Filtered'];
      const activeIndexes = segments
        .map((s) => screen.getByRole('button', { name: new RegExp(s, 'i') }))
        .map((btn) => btn.getAttribute('aria-pressed') === 'true');
      expect(activeIndexes).toEqual([false, false, true, false, false]);
    });
  });

  describe('segment clicks', () => {
    it('clicking Pinned switches the canvas to Pinned mode', () => {
      render(<ViewModeSwitcher />);
      fireEvent.click(screen.getByRole('button', { name: /pinned/i }));
      expect(useUIStore.getState().viewMode).toBe('pinned');
      expect(useUIStore.getState().lastNonSingleMode).toBe('pinned');
    });

    it('clicking Single switches the canvas to Single mode', () => {
      render(<ViewModeSwitcher />);
      fireEvent.click(screen.getByRole('button', { name: /single/i }));
      expect(useUIStore.getState().viewMode).toBe('single');
    });

    it('clicking All Nodes clears the mesh selection (re-click-deselect semantics)', () => {
      // The All segment follows the one-filter-two-controls invariant:
      // selecting All via the switcher is the same gesture as clearing
      // the sidebar selection (both route through selectMesh(null)).
      useMeshStore.setState({
        meshes: [MESH_1, MESH_2],
        meshesById: new Map([[1, MESH_1], [2, MESH_2]]),
        selectedMeshId: 2,
      });
      useUIStore.setState({ viewMode: 'mesh' });
      render(<ViewModeSwitcher />);
      fireEvent.click(screen.getByRole('button', { name: /all nodes/i }));
      expect(useMeshStore.getState().selectedMeshId).toBeNull();
      expect(useUIStore.getState().viewMode).toBe('all');
    });

    it('clicking All Nodes with no selection flips mode directly (sidebar already cleared)', () => {
      // With nothing selected, selectMesh(null) is a no-op that would
      // miss the subscription — the switcher therefore calls
      // setViewMode('all') directly to keep the mode in sync.
      render(<ViewModeSwitcher />);
      fireEvent.click(screen.getByRole('button', { name: /all nodes/i }));
      expect(useUIStore.getState().viewMode).toBe('all');
    });

    it('clicking Mesh Grid with no selection sets the mode and selects nothing (#2071)', () => {
      // #2071 deleted the fallback chain: the segment only sets the View
      // Mode. Picking the focused node's Mesh (or the first loaded Mesh)
      // here silently chose a scope for the user — the Mesh Grid now
      // renders its own "no Mesh selected" empty state instead.
      seedAgentNodes([NODE_A, NODE_B], NODE_B.id);
      useMeshStore.setState({
        meshes: [MESH_1, MESH_2],
        meshesById: new Map([[1, MESH_1], [2, MESH_2]]),
        selectedMeshId: null,
      });
      render(<ViewModeSwitcher />);
      fireEvent.click(screen.getByRole('button', { name: /mesh grid/i }));
      expect(useMeshStore.getState().selectedMeshId).toBeNull();
      expect(useUIStore.getState().viewMode).toBe('mesh');
    });

    it('clicking Mesh Grid while a mesh is already selected doesn\'t re-select (no churn)', () => {
      // The "selection already present" branch — write-on-change is the
      // contract; we leave the mesh alone and let the existing sync
      // keep the mode in step.
      useMeshStore.setState({
        meshes: [MESH_1, MESH_2],
        meshesById: new Map([[1, MESH_1], [2, MESH_2]]),
        selectedMeshId: 1,
      });
      useUIStore.setState({ viewMode: 'all' });
      render(<ViewModeSwitcher />);
      fireEvent.click(screen.getByRole('button', { name: /mesh grid/i }));
      expect(useMeshStore.getState().selectedMeshId).toBe(1);
      expect(useUIStore.getState().viewMode).toBe('mesh');
    });

    it('clicking Filtered switches to the filtered mode AND arms the search-focus request (#1609)', () => {
      // The segment owns both halves of the gesture: the mode flip makes
      // TitleBar mount the search bar, the request counter focuses it once
      // mounted. The counter must bump even though the mode was already
      // 'filtered' in a prior interaction — the click means "get me to the
      // search box".
      render(<ViewModeSwitcher />);
      fireEvent.click(screen.getByRole('button', { name: /filtered/i }));
      expect(useUIStore.getState().viewMode).toBe('filtered');
      expect(useUIStore.getState().lastNonSingleMode).toBe('filtered');
      expect(useUIStore.getState().focusGridSearchRequest).toBe(1);
    });

    it('re-clicking Filtered while already filtered re-arms the focus request without a mode churn', () => {
      useUIStore.setState({ viewMode: 'filtered', lastNonSingleMode: 'filtered' });
      render(<ViewModeSwitcher />);
      fireEvent.click(screen.getByRole('button', { name: /filtered/i }));
      expect(useUIStore.getState().focusGridSearchRequest).toBe(1);
    });
  });

  // #2074 — the switcher picks the View Mode, the indicator beside it names
  // the scope that mode produces. They are two views of ONE derived scope
  // (#2071), so a segment click must re-label the indicator in the same act.
  describe('scope indicator agreement (#2074)', () => {
    /** Render the switcher and the indicator as they sit in the title bar. */
    function renderToolbar() {
      return render(
        <>
          <ViewModeSwitcher />
          <ScopeIndicator />
        </>,
      );
    }

    it('re-labels the indicator for each cross-Mesh segment, count included', () => {
      seedAgentNodes([NODE_A, NODE_B]);
      renderToolbar();
      expect(screen.getByTestId('scope-indicator').getAttribute('aria-label')).toBe('All meshes · 2 nodes');

      fireEvent.click(screen.getByRole('button', { name: /pinned/i }));
      // Neither fixture node is pinned: an honest 0, which is what tells an
      // empty scope apart from a broken render.
      expect(screen.getByTestId('scope-indicator').getAttribute('aria-label')).toBe('Pinned across meshes · 0 nodes');

      fireEvent.click(screen.getByRole('button', { name: /filtered/i }));
      expect(screen.getByTestId('scope-indicator').getAttribute('aria-label')).toBe('Filtered across meshes · 2 of 2 nodes');
    });

    it('names the Mesh Grid scope only once a Mesh is selected — the segment still picks nothing', () => {
      seedAgentNodes([NODE_A, NODE_B]);
      useMeshStore.setState({
        meshes: [MESH_1, MESH_2],
        meshesById: new Map([[1, MESH_1], [2, MESH_2]]),
        selectedMeshId: null,
      });
      renderToolbar();

      fireEvent.click(screen.getByRole('button', { name: /mesh grid/i }));
      expect(useMeshStore.getState().selectedMeshId).toBeNull();
      expect(screen.getByTestId('scope-indicator').getAttribute('aria-label')).toBe('No mesh selected');

      // The sidebar's selection is the only thing that names a Mesh scope
      // (#2072 made the selection sticky), and the indicator reads it.
      act(() => {
        useMeshStore.getState().selectMesh(2);
      });
      expect(screen.getByTestId('scope-indicator').getAttribute('aria-label')).toBe('demo-2');
    });
  });
});
