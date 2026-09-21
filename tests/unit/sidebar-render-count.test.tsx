/**
 * Issue #1748 — Sidebar integration: one store update must commit the
 * minimum set of rows, and archived nodes must stay out of the list.
 *
 * The real `Sidebar` + real (memoized) `MeshItem` render here. Two
 * observation seams, each honest about what it measures:
 *
 *   - `CountingNodeItem` (a pass-through around the REAL memoized
 *     `NodeItem`) counts every render attempt the parent `MeshItem` makes
 *     per row. It observes `MeshItem` body execution, used by the
 *     `MeshItem`-level regroup tests below.
 *   - `CountingInlineEditableText` (a pass-through around the real name
 *     cell, which every `NodeItem` body renders exactly once) counts real
 *     `NodeItem` BODY executions. It observes the memoized component from
 *     INSIDE the memo boundary, so untouched siblings must read strictly
 *     zero — this is the signal the store-update tests assert on.
 *
 * Neither wrapper duplicates production comparator logic.
 *
 * The per-mesh async hooks (`useMeshHealth`, `useGitBranchStatus`,
 * `useMeshGitHubUrl`) are pinned to their static "clean mesh" values so no
 * background fetch can self-render a row inside the measured window — those
 * subscriptions are orthogonal to row memoization. `useProviderList` stays
 * real (its reference stability is part of the production contract) and the
 * tests settle it before baselining.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, act, waitFor } from '@testing-library/react';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import type { ComponentProps } from 'react';
import { DndContext } from '@dnd-kit/core';
import { SortableContext } from '@dnd-kit/sortable';
import { Sidebar } from '../../src/components/Sidebar/Sidebar';
import { MeshItem } from '../../src/components/Sidebar/MeshItem';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import type { SpawnOption } from '../../src/lib/groups';
import { seedAgentNodes } from './helpers/seedAgentNodes';

// Per-file mocks (do not affect the sibling sidebar-*.test.tsx files).
const { renderCounts, ietRenders } = vi.hoisted(() => ({
  renderCounts: {} as Record<number, number>,
  ietRenders: {} as Record<string, number>,
}));

vi.mock('../../src/components/Sidebar/NodeItem', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/components/Sidebar/NodeItem')>();
  const RealNodeItem = actual.NodeItem;
  function CountingNodeItem(props: ComponentProps<typeof RealNodeItem>) {
    renderCounts[props.node.id] = (renderCounts[props.node.id] ?? 0) + 1;
    return <RealNodeItem {...props} />;
  }
  return { ...actual, NodeItem: CountingNodeItem };
});

vi.mock('../../src/components/shared/InlineEditableText', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/components/shared/InlineEditableText')>();
  const RealInlineEditableText = actual.InlineEditableText;
  function CountingInlineEditableText(props: ComponentProps<typeof RealInlineEditableText>) {
    const key = typeof props.id === 'string' ? props.id : 'unknown';
    ietRenders[key] = (ietRenders[key] ?? 0) + 1;
    return <RealInlineEditableText {...props} />;
  }
  return { ...actual, InlineEditableText: CountingInlineEditableText };
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

const MESH_ONE: Mesh = {
  id: 1,
  name: 'mesh-one',
  path: '/repos/one',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
  scratchpad: '',
  sandbox: false,
};

const MESH_TWO: Mesh = {
  id: 2,
  name: 'mesh-two',
  path: '/repos/two',
  layout: 'single',
  position: 1,
  created_at: '2026-01-01',
  scratchpad: '',
  sandbox: false,
};

const PROVIDERS: SpawnOption[] = [
  { id: 'anthropic', label: 'Anthropic', color: 'bg-blue-500', icon: 'A', harness_id: 'anthropic', provider_id: null, is_proxied: false, group_key: 'anthropic' },
];

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 11,
    mesh_id: 1,
    name: 'alpha-one',
    path: '/repos/one',
    branch: 'main',
    env: 'windows',
    provider: 'anthropic',
    status: 'idle',
    use_worktree: true,
    position: 0,
    is_pinned: false,
    created_at: '2026-01-01',
    ...overrides,
  };
}

const A1 = () => makeNode({ id: 11, mesh_id: 1, name: 'alpha-one', path: '/repos/one' });
const A2 = () => makeNode({ id: 12, mesh_id: 1, name: 'alpha-two', path: '/repos/one', position: 1 });
const AX = () => makeNode({ id: 13, mesh_id: 1, name: 'alpha-archived', path: '/repos/one', position: 2, status: 'archived' });
const B1 = () => makeNode({ id: 21, mesh_id: 2, name: 'beta-one', path: '/repos/two' });
const B2 = () => makeNode({ id: 22, mesh_id: 2, name: 'beta-two', path: '/repos/two', position: 1 });

function seed() {
  useMeshStore.setState({
    meshes: [MESH_ONE, MESH_TWO],
    meshesById: new Map([[MESH_ONE.id, MESH_ONE], [MESH_TWO.id, MESH_TWO]]),
    selectedMeshId: null,
  });
  seedAgentNodes([A1(), A2(), AX(), B1(), B2()]);
  useAgentNodeStore.setState({ autopilotStates: {}, circuitOwnerships: {}, closingNodeIds: new Set(), error: null, activeNodeId: null });
}

function clearCounts() {
  for (const k of Object.keys(renderCounts)) delete renderCounts[k];
  for (const k of Object.keys(ietRenders)) delete ietRenders[k];
}

function totalCounts() {
  return Object.values(renderCounts).reduce((a, b) => a + b, 0);
}

/** `NodeItem` body executions for one row (name-cell id doubling as key). */
function ietCount(nodeId: number): number {
  return ietRenders[`node-item-name-${nodeId}`] ?? 0;
}

beforeEach(() => {
  clearCounts();
  seed();
});

type MeshItemProps = ComponentProps<typeof MeshItem>;

/** One stable callback set, reused across rerenders (what Sidebar guarantees). */
function stableMeshCallbacks() {
  return {
    onSelectMesh: vi.fn(),
    onNewNode: vi.fn(),
    onSelectProvider: vi.fn(),
    onOpenFilesProbe: vi.fn(),
    onOpenPropertiesProbe: vi.fn(),
    onOpenWorktreesProbe: vi.fn(),
    onOpenIssuesProbe: vi.fn(),
    onOpenSessionHistoryProbe: vi.fn(),
    onActivateNode: vi.fn(),
    selectMesh: vi.fn(),
    onDeleteNode: vi.fn(),
    getDefaultProvider: vi.fn().mockResolvedValue('anthropic'),
  };
}

// Shared across renders: `SortableContext` keys its context value on the
// `items` identity (see Sidebar's `sortableMeshIds`), so a fresh literal
// here would re-render the row via context no matter the props memo.
const SORTABLE_ITEMS = [MESH_ONE.id];

function renderMeshItem(props: MeshItemProps) {
  return render(
    <DndContext>
      <SortableContext items={SORTABLE_ITEMS}>
        <MeshItem {...props} />
      </SortableContext>
    </DndContext>,
  );
}

function meshProps(overrides: Partial<MeshItemProps> = {}): MeshItemProps {
  return {
    mesh: MESH_ONE,
    isSelected: false,
    isDropdownOpen: false,
    isSpawning: false,
    providerList: PROVIDERS,
    ...stableMeshCallbacks(),
    meshNodes: [],
    ...overrides,
  };
}

/** Settle the async provider-list snapshot, then baseline both counters. */
async function settleSidebar() {
  await waitFor(() => expect(screen.getByText('alpha-one')).toBeTruthy());
  expect(screen.getByText('beta-two')).toBeTruthy();
  let last = -1;
  for (let i = 0; i < 10; i++) {
    await act(async () => {});
    const total = totalCounts();
    if (total === last) break;
    last = total;
    if (i === 9) throw new Error('sidebar did not settle before baselining');
  }
  clearCounts();
}

describe('Sidebar render isolation (issue #1748)', () => {
  it('MeshItem skips a regroup that keeps identical node references', async () => {
    const n1 = A1();
    const n2 = A2();
    const props = meshProps({ meshNodes: [n1, n2] });
    const { rerender } = renderMeshItem(props);
    expect(screen.getByText('alpha-two')).toBeTruthy();
    await act(async () => {});
    clearCounts();

    // Fresh array, identical element references — what Sidebar's grouped map
    // produces on every unrelated store update. Callbacks stay stable
    // (Sidebar hoists them to `useCallback`).
    rerender(
      <DndContext>
        <SortableContext items={SORTABLE_ITEMS}>
          <MeshItem {...props} meshNodes={[n1, n2]} />
        </SortableContext>
      </DndContext>,
    );

    expect(totalCounts()).toBe(0);
  });

  it('MeshItem still re-renders when one of its nodes changes', async () => {
    const n1 = A1();
    const n2 = A2();
    const props = meshProps({ meshNodes: [n1, n2] });
    const { rerender } = renderMeshItem(props);
    await act(async () => {});
    clearCounts();

    // Control: a genuinely changed member (new reference) must render.
    rerender(
      <DndContext>
        <SortableContext items={SORTABLE_ITEMS}>
          <MeshItem {...props} meshNodes={[n1, { ...n2, status: 'awaiting_input' }]} />
        </SortableContext>
      </DndContext>,
    );

    expect(totalCounts()).toBeGreaterThan(0);
  });

  it('patching one node executes only that row body; archived nodes stay hidden', async () => {
    render(<Sidebar />);
    await settleSidebar();

    // Issue #788 — archived nodes live in the Archive probe tab, never in
    // the sidebar list.
    expect(screen.queryByText('alpha-archived')).toBeNull();

    act(() => {
      useAgentNodeStore.getState().patchAgentNode(11, { status: 'awaiting_input' });
    });

    // Only the patched row's body executed — the same-mesh sibling, the
    // other mesh, and the archived node all read strictly zero.
    expect(ietCount(11)).toBeGreaterThan(0);
    expect(ietCount(12)).toBe(0);
    expect(ietCount(21)).toBe(0);
    expect(ietCount(22)).toBe(0);
    expect(ietCount(13)).toBe(0);
    expect(screen.queryByText('alpha-archived')).toBeNull();
    // The status flip itself is visible on the changed row.
    expect(screen.getByTitle('Needs attention')).toBeTruthy();
  });

  it('activating a node executes only that row body', async () => {
    render(<Sidebar />);
    await settleSidebar();

    act(() => {
      useAgentNodeStore.getState().setActiveNode(22);
    });

    // No `MeshItem` re-rendered (the active bit is owned per row, not
    // drilled through the mesh), and only the activated row body executed.
    expect(totalCounts()).toBe(0);
    expect(ietCount(22)).toBe(1);
    expect(ietCount(11)).toBe(0);
    expect(ietCount(12)).toBe(0);
    expect(ietCount(21)).toBe(0);
  });

  it('Sidebar groups nodes in one memo pass instead of filtering per mesh in render', () => {
    // Source-level guard mirroring the issue #1246 precedent in
    // app-zustand-subscription.test.tsx: keeps the O(M x N) inline
    // `agentNodes.filter(...)` from creeping back into the render path.
    const sidebarPath = resolve(__dirname, '../../src/components/Sidebar/Sidebar.tsx');
    const src = readFileSync(sidebarPath, 'utf8');
    const stripped = stripJsComments(src);
    expect(stripped).not.toMatch(/agentNodes\.filter/);
    expect(stripped).toMatch(/useMemo/);
  });
});

/** Drop // and block comments so the guard above reads code, not prose. */
function stripJsComments(src: string): string {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/^\s*\/\/.*$/gm, '')
    .replace(/[ \t]+\/\/.*$/gm, '');
}
