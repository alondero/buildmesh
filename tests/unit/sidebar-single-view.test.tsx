/**
 * The desktop sidebar has one view: the mesh list.
 *
 * The Workspace / Attention toggle and the grouped attention view it opened
 * are gone (see ADR-0048). This file pins the removal so neither can creep
 * back: the toggle would again cost a chrome row above the list, and the
 * attention view would again duplicate what the mesh rows, the node status
 * marks and Agent History already report.
 *
 * Mock shape follows `sidebar-mesh-bands.test.tsx`: the per-mesh async
 * hooks are pinned so no background fetch can move a row while the
 * assertions run.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
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

describe('Sidebar single view (ADR-0048)', () => {
  it('offers no view toggle above the mesh list', () => {
    render(<Sidebar />);

    // The toggle was a named group of two view buttons. Neither the group
    // nor either button is rendered now.
    expect(screen.queryByRole('group', { name: 'Sidebar views' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Workspace' })).toBeNull();
    expect(screen.queryByRole('button', { name: /^Attention/ })).toBeNull();
  });

  it('shows the mesh list and no retired attention groups', () => {
    render(<Sidebar />);

    expect(screen.getByText('API repository')).toBeTruthy();
    // The grouped attention headings belonged to the retired view. The
    // assertion above (no toggle group, no view buttons) is what fails when
    // the change is reverted; this one pins the surviving content.
    expect(screen.queryByText('Failed sessions')).toBeNull();
    expect(screen.queryByText('Needs input')).toBeNull();
    expect(screen.queryByText('Paused sessions')).toBeNull();
  });
});
