/**
 * Issue #2468 — the empty workspace Sidebar rendered two mesh-creation CTAs
 * in the same 256px column: the inline "+ New mesh" button under the "No
 * meshes yet" message, and the sticky footer "New mesh" button. A first-run
 * user saw both at once.
 *
 * First creation belongs to the canvas. The sidebar offers its persistent
 * footer once the workspace holds a mesh.
 *
 * Mock shape follows `sidebar-mesh-bands.test.tsx`: the per-mesh
 * async hooks are pinned so no background fetch can move a row while the
 * assertions run.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, act } from '@testing-library/react';
import { Sidebar } from '../../src/components/Sidebar/Sidebar';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useUIStore } from '../../src/stores/uiStore';
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

const mesh = {
  id: 1,
  name: 'API repository',
  path: '/api',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
  scratchpad: '',
  sandbox: false,
} as Mesh;

/** Mesh-creation CTAs rendered by the sidebar. */
function meshCtas(): HTMLElement[] {
  return screen.queryAllByRole('button', { name: /new mesh/i });
}

function seedMeshes(meshes: Mesh[]) {
  useMeshStore.setState({
    meshes,
    meshesById: new Map(meshes.map((m) => [m.id, m])),
    selectedMeshId: null,
  });
}

beforeEach(() => {
  cleanup();
  seedMeshes([]);
  seedAgentNodes([]);
  useAgentNodeStore.setState({ circuitOwnerships: {}, closingNodeIds: new Set(), error: null, activeNodeId: null });
  useUIStore.setState({ createMeshOpen: false });
});
afterEach(() => { cleanup(); });

describe('Sidebar empty-workspace CTA (issue #2468)', () => {
  it('leaves first creation to the canvas on an empty workspace', () => {
    render(<Sidebar />);

    expect(meshCtas()).toHaveLength(0);
    expect(screen.queryByRole('separator', { name: 'Resize sidebar' })).toBeNull();
    expect(screen.queryByText(/No meshes yet/)).toBeNull();
  });

  it('keeps the sticky footer CTA on a populated workspace', () => {
    seedMeshes([mesh]);
    render(<Sidebar />);

    const ctas = meshCtas();
    expect(ctas).toHaveLength(1);
    expect(ctas[0].textContent).toBe('New mesh');
    expect(screen.getByRole('separator', { name: 'Resize sidebar' })).toBeTruthy();
    // The empty-state message is gone with the mesh list.
    expect(screen.queryByText(/No meshes yet/)).toBeNull();

    fireEvent.click(ctas[0]);
    expect(useUIStore.getState().createMeshOpen).toBe(true);
  });

  it('adds the sidebar CTA when the first mesh arrives', () => {
    render(<Sidebar />);
    expect(meshCtas()).toHaveLength(0);

    // Subsequent creation belongs in the sidebar.
    act(() => {
      seedMeshes([mesh]);
    });

    const ctas = meshCtas();
    expect(ctas).toHaveLength(1);
    expect(ctas[0].textContent).toBe('New mesh');

    fireEvent.click(ctas[0]);
    expect(useUIStore.getState().createMeshOpen).toBe(true);
  });
});
