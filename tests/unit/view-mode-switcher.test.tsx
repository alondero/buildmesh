/**
 * ViewModeSwitcher (wayfinder #982 / #983 / #986) — the four-segment
 * control in the canvas header that drives uiStore.viewMode. Pins the
 * segment rendering, ARIA semantics, and the deliberate sidebar-sync
 * round-trips (Mesh selects a fallback mesh and lets the uiStore sync
 * flip the mode; All clears the selection the same way the sidebar
 * re-click-deselect does).
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { ViewModeSwitcher } from '../../src/components/ViewModeSwitcher/ViewModeSwitcher';
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
  useUIStore.setState({ viewMode: 'all', lastNonSingleMode: 'all' });
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
    });

    it('marks exactly the active segment with aria-pressed=true', () => {
      useUIStore.setState({ viewMode: 'pinned' });
      render(<ViewModeSwitcher />);
      const segments = ['Single', 'Mesh Grid', 'Pinned', 'All Nodes'];
      const activeIndexes = segments
        .map((s) => screen.getByRole('button', { name: new RegExp(s, 'i') }))
        .map((btn) => btn.getAttribute('aria-pressed') === 'true');
      expect(activeIndexes).toEqual([false, false, true, false]);
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

    it('clicking Mesh Grid with no selection falls back to the active node\'s mesh and syncs', () => {
      // The "fallback to the active node's mesh" branch (ticket #983).
      seedAgentNodes([NODE_A, NODE_B], NODE_B.id);
      useMeshStore.setState({
        meshes: [MESH_1, MESH_2],
        meshesById: new Map([[1, MESH_1], [2, MESH_2]]),
        selectedMeshId: null,
      });
      render(<ViewModeSwitcher />);
      fireEvent.click(screen.getByRole('button', { name: /mesh grid/i }));
      expect(useMeshStore.getState().selectedMeshId).toBe(MESH_2.id);
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
  });
});
