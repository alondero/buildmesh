/**
 * Paired agents (a Node Activity) cluster in the mesh sidebar under a connector
 * rail, so a pairing created in the grid is visible in the sidebar too.
 *
 * Two claims are pinned here:
 *
 *  1. **Rendering** — a paired cluster draws one root row plus an indented,
 *     rail-joined sub-row per member, while an unpaired node renders exactly
 *     the bare row it always did. Members stay individually clickable, because
 *     a paired member is still an independent Agent Node.
 *  2. **Agreement** — the sidebar's clusters match the grid's cards for the
 *     same nodes, because both resolve through `activityRootId`. This is the
 *     regression that matters: if the two surfaces ever disagree, the sidebar
 *     would advertise a pairing the grid does not have (or hide one it does).
 *
 * The grouping itself is also covered as a pure function in
 * `tests/unit/node-activities.test.tsx`; this file is about what the user sees.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { DndContext } from '@dnd-kit/core';
import { SortableContext } from '@dnd-kit/sortable';
import { MeshItem } from '../../src/components/Sidebar/MeshItem';
import { activityMemberIds, clusterActivityNodes } from '../../src/lib/nodeActivities';
import { useNodeActivityStore } from '../../src/stores/nodeActivityStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import type { Mesh } from '../../src/stores/meshStore';
import type { CircuitAgentOwnership } from '../../src/types/generated/CircuitAgentOwnership';
import type { SpawnOption } from '../../src/lib/groups';
import type { NodeActivityCluster } from '../../src/lib/nodeActivities';

vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn() }));
vi.mock('../../src/hooks/useMeshHealth', () => ({ useMeshHealth: () => ({ health: null, refresh: vi.fn() }) }));
vi.mock('../../src/hooks/useGitBranchStatus', () => ({ useGitBranchStatus: () => ({ branchStatus: null, refresh: vi.fn() }) }));
vi.mock('../../src/hooks/useMeshGitHubUrl', () => ({ useMeshGitHubUrl: () => ({ url: null, loading: false, refresh: vi.fn() }) }));

const MESH = {
  id: 3, name: 'my-mesh', path: '/tmp/my-mesh', layout: 'single',
  position: 0, created_at: '2026-01-01', scratchpad: '', sandbox: false,
} as Mesh;

const PROVIDERS: SpawnOption[] = [
  { id: 'anthropic', label: 'Anthropic', color: 'bg-blue-500', icon: 'A', harness_id: 'anthropic', provider_id: null, is_proxied: false, group_key: 'anthropic' },
];

function makeNode(id: number, overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id, mesh_id: 3, name: `node-${id}`, path: '/tmp/my-mesh', branch: 'main',
    env: 'windows', provider: 'anthropic', status: 'idle', use_worktree: true,
    position: id, is_pinned: false, created_at: '2026-01-01',
    ...overrides,
  } as AgentNode;
}

/** A review-circuit parent/child pair — the implementer↔reviewer lineage that
 *  `circuitRootId` walks via `circuitOwnerships`. */
function ownership(id: number, parent: number | null): CircuitAgentOwnership {
  return { node_id: id, run_id: 1, circuit_id: 7, circuit_name: 'Review', state: 'running', parent_node_id: parent };
}

function renderMeshItem(clusters: NodeActivityCluster[], onActivateNode = vi.fn()) {
  const props: React.ComponentProps<typeof MeshItem> = {
    mesh: MESH, isSelected: false, isDropdownOpen: false, isSpawning: false,
    providerList: PROVIDERS, onSelectMesh: vi.fn(), onNewNode: vi.fn(),
    onSelectProvider: vi.fn(), onOpenFilesProbe: vi.fn(), onOpenPropertiesProbe: vi.fn(),
    onOpenWorktreesProbe: vi.fn(), onOpenIssuesProbe: vi.fn(), onOpenSessionHistoryProbe: vi.fn(),
    nodeClusters: clusters, onActivateNode, selectMesh: vi.fn(), onDeleteNode: vi.fn(),
    getDefaultProvider: vi.fn().mockResolvedValue('anthropic'),
  };
  return {
    props,
    ...render(
      <DndContext>
        <SortableContext items={[MESH.id]}>
          <MeshItem {...props} />
        </SortableContext>
      </DndContext>,
    ),
  };
}


describe('sidebar node-activity clusters', () => {
  beforeEach(() => {
    useNodeActivityStore.setState({ groups: [] });
    useAgentNodeStore.setState({ circuitOwnerships: {}, activeNodeId: null });
  });

  it('renders an unpaired node as a bare row with no rail', () => {
    const node = makeNode(1);
    const { container } = renderMeshItem(clusterActivityNodes([node], {}, []));
    expect(screen.getByText('node-1')).toBeTruthy();
    // No cluster chrome at all for the common unpaired case.
    expect(container.querySelector('[data-node-cluster-id]')).toBeNull();
    expect(screen.queryByLabelText('Paired agents')).toBeNull();
  });

  it('clusters a circuit-paired reviewer under a rail beneath its implementer', () => {
    const root = makeNode(1, { name: 'implementer', status: 'running' });
    const reviewer = makeNode(2, { name: 'reviewer', status: 'awaiting_input' });
    const ownerships = { 1: ownership(1, null), 2: ownership(2, 1) };

    const clusters = clusterActivityNodes([root, reviewer], ownerships, []);
    expect(clusters).toHaveLength(1);
    expect(clusters[0].paired).toBe(true);

    const { container } = renderMeshItem(clusters);
    expect(container.querySelector('[data-node-cluster-id="1"]')).not.toBeNull();
    // Both members are present and individually addressable.
    expect(screen.getByText('implementer')).toBeTruthy();
    expect(screen.getByText('reviewer')).toBeTruthy();
    // The sub-row list is the pairing affordance itself.
    expect(screen.getByLabelText('Paired agents')).toBeTruthy();
  });

  it('marks the pair with a combined status reflecting the worst member', () => {
    const root = makeNode(1, { name: 'implementer', status: 'running' });
    // A failing reviewer must surface on the cluster marker rather than hide
    // behind the implementer's healthy "Running" row.
    const reviewer = makeNode(2, { name: 'reviewer', status: 'error' });
    const ownerships = { 1: ownership(1, null), 2: ownership(2, 1) };

    renderMeshItem(clusterActivityNodes([root, reviewer], ownerships, []));
    expect(screen.getByLabelText('Paired group of 2 agents — Needs attention')).toBeTruthy();
  });

  it('keeps each paired member individually clickable', () => {
    const root = makeNode(1, { name: 'implementer' });
    const reviewer = makeNode(2, { name: 'reviewer' });
    const ownerships = { 1: ownership(1, null), 2: ownership(2, 1) };
    const onActivateNode = vi.fn();

    renderMeshItem(clusterActivityNodes([root, reviewer], ownerships, []), onActivateNode);
    fireEvent.click(screen.getByText('reviewer'));
    expect(onActivateNode).toHaveBeenCalledWith(2);
  });

  it('resolves a hand-persisted group the way drag-to-group persists it', () => {
    const a = makeNode(1, { name: 'alpha' });
    const b = makeNode(2, { name: 'beta' });
    // `buildmesh.node-groups` is what drag-to-group writes.
    const clusters = clusterActivityNodes([a, b], {}, [[1, 2]]);
    expect(clusters).toHaveLength(1);
    expect(clusters[0].paired).toBe(true);
    // Hand grouping has no implementer/reviewer semantics, so it is flagged as
    // such and members keep their own names (see `activityMemberRole`).
    expect(clusters[0].handGrouped).toBe(true);
  });

  it('agrees with the grid card membership for the same nodes', () => {
    // The core contract: the sidebar's clusters and the grid's cards are two
    // views of one resolution. If this drifts, the sidebar would show a pairing
    // the grid does not have.
    const nodes = [makeNode(1), makeNode(2), makeNode(3)];
    const ownerships = { 1: ownership(1, null), 2: ownership(2, 1) };
    const manualGroups = [[3, 1]] as number[][];

    const sidebar = clusterActivityNodes(nodes, ownerships, manualGroups)
      .map(c => c.members.map(m => m.id).sort((x, y) => x - y))
      .sort((x, y) => x[0] - y[0]);
    // `activityMemberIds` is what `AgentNodeView` feeds to `NodeCard`.
    const grid = Object.values(activityMemberIds(nodes, ownerships, manualGroups))
      .map(ids => [...ids].sort((x, y) => x - y))
      .sort((x, y) => x[0] - y[0]);
    expect(sidebar).toEqual(grid);
  });

  it('anchors a cluster at the representative position, not a hidden member', () => {
    // The reviewer sits at position 1 while the card is anchored at the
    // implementer (position 5). A cluster must not jump to slot 1 merely
    // because some member has a lower position.
    const root = makeNode(1, { position: 5 });
    const reviewer = makeNode(2, { position: 1 });
    const other = makeNode(3, { position: 9 });
    const ownerships = { 1: ownership(1, null), 2: ownership(2, 1) };

    const clusters = clusterActivityNodes([root, reviewer, other], ownerships, []);
    expect(clusters.map(c => c.root.id)).toEqual([1, 3]);
  });

  it('splits a cross-mesh group into one cluster per mesh', () => {
    const a = makeNode(1, { mesh_id: 3 });
    const b = makeNode(2, { mesh_id: 4 });
    const clusters = clusterActivityNodes([a, b], {}, [[1, 2]]);
    expect(clusters).toHaveLength(2);
    expect(clusters.every(c => !c.paired)).toBe(true);
  });
});
