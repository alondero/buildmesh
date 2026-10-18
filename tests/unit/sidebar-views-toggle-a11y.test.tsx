/**
 * Issue #2467 — the Workspace / Attention toggle in the Sidebar header
 * carries `aria-label="Sidebar views"`. On a generic unroled `<div>` that
 * label is ignored, so the pair announced as two unlabelled buttons with
 * no indication they are views of the same panel. `role="group"` gives the
 * container a role the name is allowed on.
 *
 * Mock shape follows `sidebar-mesh-bands.test.tsx`: the per-mesh async
 * hooks are pinned so no background fetch can move a row while the
 * accessible-tree assertion runs.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, within } from '@testing-library/react';
import { Sidebar } from '../../src/components/Sidebar/Sidebar';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
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

beforeEach(() => {
  cleanup();
  useMeshStore.setState({
    meshes: [mesh],
    meshesById: new Map([[mesh.id, mesh]]),
    selectedMeshId: null,
  });
  seedAgentNodes([]);
  useAgentNodeStore.setState({ circuitOwnerships: {}, closingNodeIds: new Set(), error: null, activeNodeId: null });
});
afterEach(() => { cleanup(); });

describe('Sidebar views toggle (#2467)', () => {
  it('exposes the Workspace / Attention toggle as a named group', () => {
    render(<Sidebar />);

    // `role="group"` is the whole fix: without it the aria-label is dropped.
    const group = screen.getByRole('group', { name: 'Sidebar views' });
    expect(group.tagName).toBe('DIV');
    // The name is on the group, and the two view buttons are its children —
    // that is what lets a screen reader announce them as one control set.
    expect(within(group).getByRole('button', { name: 'Workspace' })).toBeTruthy();
    expect(within(group).getByRole('button', { name: /^Attention/ })).toBeTruthy();
  });
});