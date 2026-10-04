import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, cleanup, act } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import type { ComponentProps } from 'react';
import { DndContext } from '@dnd-kit/core';
import { SortableContext } from '@dnd-kit/sortable';

// Row-body execution counter, mirroring the CountingNodeItem seam in
// sidebar-render-count.test.tsx: the mocked spawn form renders exactly once
// per CompactMeshItem body execution, so a skipped memo re-render reads as a
// flat counter across a rerender with fresh-but-equal props.
const rowBodyRenders = vi.hoisted(() => ({ count: 0 }));

vi.mock('../../src/components/Sidebar/NodeCreationForm', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/components/Sidebar/NodeCreationForm')>();
  const RealForm = actual.NodeCreationForm;
  function CountingForm(props: ComponentProps<typeof RealForm>) {
    rowBodyRenders.count += 1;
    return <RealForm {...props} />;
  }
  return { ...actual, NodeCreationForm: CountingForm };
});

// Pin the per-mesh async hooks to static values so no background fetch can
// self-render the row inside the memo measurement window.
vi.mock('../../src/hooks/useMeshHealth', () => ({
  useMeshHealth: () => ({ health: null, refresh: vi.fn() }),
}));

vi.mock('../../src/hooks/useGitBranchStatus', () => ({
  useGitBranchStatus: () => ({ branchStatus: null, refresh: vi.fn() }),
}));
import { CompactMeshItem } from '../../src/components/Sidebar/CompactMeshItem';
import type { Mesh } from '../../src/stores/meshStore';
import type { AgentNode } from '../../src/stores/agentNodeStore';
import type { SpawnOption } from '../../src/lib/groups';
import type { NodeActivityCluster } from '../../src/lib/nodeActivities';

const MESH: Mesh = {
  id: 3,
  name: 'my-mesh',
  path: '/tmp/my-mesh',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
  scratchpad: '',
  sandbox: false,
};

const PROVIDERS: SpawnOption[] = [
  { id: 'anthropic', label: 'Anthropic', color: 'bg-blue-500', icon: 'A', harness_id: 'anthropic', provider_id: null, is_proxied: false, group_key: 'anthropic' },
];

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 10,
    mesh_id: 3,
    name: 'node-a',
    path: '/tmp/my-mesh',
    branch: 'main',
    env: 'wsl',
    provider: 'anthropic',
    status: 'running',
    use_worktree: false,
    created_at: '2026-01-01',
    ...overrides,
  };
}

function loneCluster(node: AgentNode): NodeActivityCluster {
  return { root: node, members: [node], paired: false, handGrouped: false };
}

type Props = React.ComponentProps<typeof CompactMeshItem>;

// Stable across renders: a fresh items array would churn SortableContext and
// re-render the row underneath the memo boundary (issue #1748), which is not
// what the isolation test measures.
const SORTABLE_IDS = [MESH.id];

function renderCompact(overrides: Partial<Props> = {}) {
  const props: Props = {
    mesh: MESH,
    isSelected: false,
    isDropdownOpen: false,
    isSpawning: false,
    providerList: PROVIDERS,
    onSelectMesh: vi.fn(),
    onNewNode: vi.fn(),
    onSelectProvider: vi.fn(),
    onOpenFilesProbe: vi.fn(),
    onOpenPropertiesProbe: vi.fn(),
    onOpenWorktreesProbe: vi.fn(),
    onOpenIssuesProbe: vi.fn(),
    onOpenSessionHistoryProbe: vi.fn(),
    nodeClusters: [],
    onActivateNode: vi.fn(),
    selectMesh: vi.fn(),
    onDeleteNode: vi.fn(),
    getDefaultProvider: vi.fn().mockResolvedValue('anthropic'),
    ...overrides,
  };
  // `useSortable` needs the dnd-kit context Sidebar provides in production.
  const renderEl = (p: Props) => (
    <DndContext>
      <SortableContext items={SORTABLE_IDS}>
        <CompactMeshItem {...p} />
      </SortableContext>
    </DndContext>
  );
  const result = render(renderEl(props));
  return {
    ...result,
    props,
    rerenderWith: (o: Partial<Props>) => result.rerender(renderEl({ ...props, ...o })),
  };
}

describe('CompactMeshItem (prototype L)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockImplementation(() => Promise.resolve({}));
  });

  afterEach(() => {
    cleanup();
  });

  it('renders the mesh name with dots and a count, and no textual status labels', () => {
    renderCompact({ nodeClusters: [loneCluster(makeNode())] });
    expect(screen.getByText('my-mesh')).toBeTruthy();
    // Dots carry status without words: none of the status vocabulary appears as text.
    for (const word of ['Running', 'Idle', 'Needs attention', 'Starting', 'Ready', 'Suspended', 'Error']) {
      expect(screen.queryByText(word)).toBeNull();
    }
    expect(screen.getByText('1')).toBeTruthy();
  });

  it('starts collapsed for quiet meshes and expands via the dots line', async () => {
    renderCompact({ nodeClusters: [loneCluster(makeNode({ status: 'idle' }))] });
    expect(screen.queryByText('node-a')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Show agents for my-mesh' }));
    expect(screen.getByText('node-a')).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Hide agents for my-mesh' }));
    expect(screen.queryByText('node-a')).toBeNull();
  });

  it('starts expanded for meshes with a node needing attention', () => {
    renderCompact({ nodeClusters: [loneCluster(makeNode({ id: 11, name: 'node-b', status: 'awaiting_input' }))] });
    expect(screen.getByText('node-b')).toBeTruthy();
  });

  it('makes the colour bar both picker and reorder handle', () => {
    renderCompact();
    // One element, two gestures: the accessible name carries both.
    const bar = screen.getByRole('button', { name: 'Change mesh colour for my-mesh — drag to reorder' });
    expect(bar.title).toBe('Drag to reorder my-mesh · click to change mesh colour');
  });

  it('opens the picker on a mouse press-and-release in place', () => {
    renderCompact();
    const bar = screen.getByRole('button', { name: /Change mesh colour/ });
    fireEvent.pointerDown(bar, { clientX: 10, clientY: 10 });
    fireEvent.click(bar, { clientX: 10, clientY: 10, detail: 1 });
    expect(screen.getByText('Colour for my-mesh')).toBeTruthy();
  });

  it('swallows the trailing click after the pointer travelled (a drag ran)', () => {
    renderCompact();
    const bar = screen.getByRole('button', { name: /Change mesh colour/ });
    fireEvent.pointerDown(bar, { clientX: 10, clientY: 10 });
    // A real mouse click carries detail >= 1 (fireEvent defaults to 0, which
    // is the keyboard/AT path and must open).
    fireEvent.click(bar, { clientX: 60, clientY: 10, detail: 1 });
    expect(screen.queryByText('Colour for my-mesh')).toBeNull();
  });

  it('does not select the mesh on the trailing click of a travelled press', () => {
    const { props } = renderCompact();
    const bar = screen.getByRole('button', { name: /Change mesh colour/ });
    fireEvent.pointerDown(bar, { clientX: 10, clientY: 10 });
    fireEvent.click(bar, { clientX: 60, clientY: 10, detail: 1 });
    expect(props.onSelectMesh).not.toHaveBeenCalled();
  });

  it('opens the picker on AT activation even after an off-target travelled press', () => {
    renderCompact();
    const bar = screen.getByRole('button', { name: /Change mesh colour/ });
    fireEvent.pointerDown(bar, { clientX: 100, clientY: 100 });
    fireEvent.pointerUp(document.body, { clientX: 400, clientY: 100 });
    expect(screen.queryByText('Colour for my-mesh')).toBeNull();
    fireEvent.click(bar, { detail: 0 });
    expect(screen.getByText('Colour for my-mesh')).toBeTruthy();
  });

  it('keeps the header out of the tab order and announces the bar as sortable', () => {
    renderCompact({ nodeClusters: [loneCluster(makeNode())] });
    const bar = screen.getByRole('button', { name: 'Change mesh colour for my-mesh — drag to reorder' });
    expect(bar.getAttribute('aria-roledescription')).toBe('sortable');
    const header = bar.closest('[data-prototype-mesh]')?.firstElementChild;
    expect(header?.getAttribute('tabindex')).toBe('-1');
    expect(header?.getAttribute('role')).toBeNull();
  });

  it('skips re-render when clusters are fresh arrays with identical members (#1748)', async () => {
    const node = makeNode({ status: 'idle' });
    const { rerenderWith } = renderCompact({ nodeClusters: [loneCluster(node)] });
    await act(async () => {});
    rowBodyRenders.count = 0;

    // Fresh cluster wrappers, identical member references — what Sidebar's
    // grouped map produces on every unrelated store update.
    rerenderWith({ nodeClusters: [loneCluster(node)] });
    await act(async () => {});
    expect(rowBodyRenders.count).toBe(0);
  });

  it('still re-renders when a member actually changes', async () => {
    const node = makeNode({ status: 'idle' });
    const { rerenderWith } = renderCompact({ nodeClusters: [loneCluster(node)] });
    await act(async () => {});
    rowBodyRenders.count = 0;

    rerenderWith({ nodeClusters: [loneCluster({ ...node, status: 'running' })] });
    await act(async () => {});
    expect(rowBodyRenders.count).toBeGreaterThan(0);
  });

  it('expands when heat arrives after mount, but yields to a manual collapse', async () => {
    const hot = makeNode({ id: 11, name: 'node-hot', status: 'awaiting_input' });
    const { rerenderWith } = renderCompact({ nodeClusters: [] });
    expect(screen.queryByText('node-hot')).toBeNull();

    rerenderWith({ nodeClusters: [loneCluster(hot)] });
    expect(screen.getByText('node-hot')).toBeTruthy();

    await userEvent.click(screen.getByRole('button', { name: 'Hide agents for my-mesh' }));
    expect(screen.queryByText('node-hot')).toBeNull();
    rerenderWith({ nodeClusters: [loneCluster({ ...hot, id: 12, name: 'node-hotter' })] });
    expect(screen.queryByText('node-hotter')).toBeNull();
  });
});
