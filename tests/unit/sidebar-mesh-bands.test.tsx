/**
 * Issue #1939 — Mesh sidebar: meshes with open agent nodes sort into a
 * labelled top band.
 *
 * The real `Sidebar` renders against seeded mesh + agent-node stores and
 * the assertions target only what a user can observe: the order in which
 * mesh names appear, the single band label, and that inactive rows keep
 * their spawn affordance. No partition signature, memo boundary, or
 * component internal is asserted on.
 *
 * Shares the mock shape of `sidebar-render-count.test.tsx` (per-file
 * mocks; async per-mesh hooks pinned so no background fetch can move a
 * row mid-assertion). `useProviderList` stays real and the tests settle
 * it before asserting.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, act, waitFor, cleanup } from '@testing-library/react';
import type { ComponentProps } from 'react';
import { Sidebar } from '../../src/components/Sidebar/Sidebar';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';

// Pass-through around the real memoized `NodeItem` (same shape as
// `sidebar-render-count.test.tsx`): each invocation observes one parent
// `MeshItem` body execution for that row, without duplicating production
// comparator logic.
const { meshItemBodies } = vi.hoisted(() => ({
  meshItemBodies: {} as Record<number, number>,
}));

vi.mock('../../src/components/Sidebar/NodeItem', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/components/Sidebar/NodeItem')>();
  const RealNodeItem = actual.NodeItem;
  function CountingNodeItem(props: ComponentProps<typeof RealNodeItem>) {
    meshItemBodies[props.node.id] = (meshItemBodies[props.node.id] ?? 0) + 1;
    return <RealNodeItem {...props} />;
  }
  return { ...actual, NodeItem: CountingNodeItem };
});

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

// Manual drag order: alpha, beta, gamma, delta, epsilon, zeta.
const MESHES = [
  makeMesh(1, 'mesh-alpha', 0),
  makeMesh(2, 'mesh-beta', 1),
  makeMesh(3, 'mesh-gamma', 2),
  makeMesh(4, 'mesh-delta', 3),
  makeMesh(5, 'mesh-epsilon', 4),
  makeMesh(6, 'mesh-zeta', 5),
];

let nextNodeId = 100;

function makeNode(meshId: number, name: string, status: AgentNode['status'] = 'idle'): AgentNode {
  return {
    id: nextNodeId++,
    mesh_id: meshId,
    name,
    path: `/repos/node-${nextNodeId}`,
    branch: 'main',
    env: 'windows',
    provider: 'anthropic',
    status,
    use_worktree: true,
    position: 0,
    is_pinned: false,
    created_at: '2026-01-01',
  } as AgentNode;
}

/** Baseline: beta + delta (x2) + zeta open, epsilon archived-only, alpha + gamma empty. */
function baselineNodes(): AgentNode[] {
  nextNodeId = 100;
  return [
    makeNode(2, 'beta-one'),
    makeNode(4, 'delta-one'),
    makeNode(4, 'delta-two'),
    makeNode(5, 'epsilon-archived', 'archived'),
    makeNode(6, 'zeta-one'),
  ];
}

function seed(meshes: Mesh[] = MESHES, nodes: AgentNode[] = baselineNodes()) {
  useMeshStore.setState({
    meshes,
    meshesById: new Map(meshes.map((m) => [m.id, m])),
    selectedMeshId: null,
  });
  seedAgentNodes(nodes);
  useAgentNodeStore.setState({ circuitOwnerships: {}, closingNodeIds: new Set(), error: null, activeNodeId: null });
}

/** Mesh names in rendered sidebar order (band label excluded — it is not a mesh row). */
function meshOrder(): string[] {
  return Array.from(document.querySelectorAll('[id^="mesh-item-name-"]')).map(
    (el) => el.textContent ?? '',
  );
}

async function settleSidebar() {
  await waitFor(() => expect(screen.getByText('beta-one')).toBeTruthy());
  expect(screen.getByText('zeta-one')).toBeTruthy();
  for (let i = 0; i < 10; i++) {
    await act(async () => {});
  }
}

function clearBodyCounts() {
  for (const k of Object.keys(meshItemBodies)) delete meshItemBodies[k];
}

beforeEach(() => {
  cleanup();
  clearBodyCounts();
  seed();
});

describe('Sidebar mesh bands (issue #1939)', () => {
  it('renders meshes with open nodes above the rest, manual order kept within each band', async () => {
    render(<Sidebar />);
    await settleSidebar();

    expect(meshOrder()).toEqual([
      'mesh-beta',
      'mesh-delta',
      'mesh-zeta',
      'mesh-alpha',
      'mesh-gamma',
      'mesh-epsilon',
    ]);
  });

  it('treats an archived-only mesh as inactive and lists a multi-node mesh exactly once', async () => {
    render(<Sidebar />);
    await settleSidebar();

    const order = meshOrder();
    // Volume does not promote: delta has two open nodes but one row.
    expect(order.filter((n) => n === 'mesh-delta')).toHaveLength(1);
    // Archived nodes do not count as open: epsilon sits in the lower band.
    expect(order.indexOf('mesh-epsilon')).toBeGreaterThan(order.indexOf('mesh-zeta'));
    expect(screen.queryByText('epsilon-archived')).toBeNull();
  });

  it('renders one label for the active band and none for the inactive band', async () => {
    render(<Sidebar />);
    await settleSidebar();

    expect(screen.getAllByText('Active')).toHaveLength(1);
    // The label heads the list: it precedes the first active mesh row.
    const label = screen.getByText('Active');
    const firstMesh = document.getElementById('mesh-item-name-2');
    expect(label.compareDocumentPosition(firstMesh!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it('renders no label when no mesh has an open node, keeping manual order', async () => {
    seed(MESHES, [makeNode(5, 'only-archived', 'archived')]);
    render(<Sidebar />);
    await waitFor(() => expect(screen.getByText('mesh-alpha')).toBeTruthy());
    for (let i = 0; i < 10; i++) {
      await act(async () => {});
    }

    expect(screen.queryByText('Active')).toBeNull();
    expect(meshOrder()).toEqual([
      'mesh-alpha',
      'mesh-beta',
      'mesh-gamma',
      'mesh-delta',
      'mesh-epsilon',
      'mesh-zeta',
    ]);
  });

  it('promotes a mesh on its first open node and demotes it to its manual slot on the last close', async () => {
    render(<Sidebar />);
    await settleSidebar();

    // mesh-alpha starts empty in the lower band.
    expect(meshOrder()[3]).toBe('mesh-alpha');

    // Opening its first node promotes it ahead of beta (manual order wins within the band).
    const promoted = [...baselineNodes(), makeNode(1, 'alpha-one')];
    act(() => {
      seedAgentNodes(promoted);
    });
    expect(meshOrder().slice(0, 4)).toEqual([
      'mesh-alpha',
      'mesh-beta',
      'mesh-delta',
      'mesh-zeta',
    ]);

    // Closing that node drops alpha back to its manual slot between zeta and gamma.
    act(() => {
      seedAgentNodes(baselineNodes());
    });
    expect(meshOrder()).toEqual([
      'mesh-beta',
      'mesh-delta',
      'mesh-zeta',
      'mesh-alpha',
      'mesh-gamma',
      'mesh-epsilon',
    ]);
  });

  it('a demote and re-promote cycle returns the mesh to the same relative position', async () => {
    render(<Sidebar />);
    await settleSidebar();

    // Archiving beta's only open node demotes it into the lower band at its manual slot.
    act(() => {
      useAgentNodeStore.getState().patchAgentNode(100, { status: 'archived' });
    });
    expect(meshOrder()).toEqual([
      'mesh-delta',
      'mesh-zeta',
      'mesh-alpha',
      'mesh-beta',
      'mesh-gamma',
      'mesh-epsilon',
    ]);

    // Reopening it restores the exact baseline arrangement.
    act(() => {
      useAgentNodeStore.getState().patchAgentNode(100, { status: 'idle' });
    });
    expect(meshOrder()).toEqual([
      'mesh-beta',
      'mesh-delta',
      'mesh-zeta',
      'mesh-alpha',
      'mesh-gamma',
      'mesh-epsilon',
    ]);
  });

  it('keeps the spawn affordance on inactive meshes', async () => {
    render(<Sidebar />);
    await settleSidebar();

    // One provider picker per mesh row — de-emphasis never removes the `+ ▾` cluster.
    expect(screen.getAllByTitle('Choose provider')).toHaveLength(6);
  });

  it('a band-neutral node update re-executes only that mesh row (issue #1748 guarantee)', async () => {
    render(<Sidebar />);
    await settleSidebar();
    clearBodyCounts();

    // Flipping a status inside mesh-beta moves no mesh between bands.
    act(() => {
      useAgentNodeStore.getState().patchAgentNode(100, { status: 'awaiting_input' });
    });

    // Only beta's own row body executed: untouched meshes bail out past
    // both the `MeshItem` memo comparator and the sortable-items identity.
    expect(meshItemBodies[100] ?? 0).toBeGreaterThan(0);
    expect(meshItemBodies[101] ?? 0).toBe(0);
    expect(meshItemBodies[102] ?? 0).toBe(0);
    expect(meshItemBodies[104] ?? 0).toBe(0);
    expect(meshOrder()).toEqual([
      'mesh-beta',
      'mesh-delta',
      'mesh-zeta',
      'mesh-alpha',
      'mesh-gamma',
      'mesh-epsilon',
    ]);
  });
});
