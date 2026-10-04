import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { DndContext } from '@dnd-kit/core';
import { SortableContext } from '@dnd-kit/sortable';
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
  const result = render(
    <DndContext>
      <SortableContext items={[MESH.id]}>
        <CompactMeshItem {...props} />
      </SortableContext>
    </DndContext>,
  );
  return { ...result, props };
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

  it('opens the picker on a press-and-release in place', () => {
    renderCompact();
    const bar = screen.getByRole('button', { name: /Change mesh colour/ });
    fireEvent.pointerDown(bar, { clientX: 10, clientY: 10 });
    fireEvent.click(bar, { clientX: 10, clientY: 10 });
    expect(screen.getByText('Colour for my-mesh')).toBeTruthy();
  });

  it('swallows the trailing click after the pointer travelled (a drag ran)', () => {
    renderCompact();
    const bar = screen.getByRole('button', { name: /Change mesh colour/ });
    fireEvent.pointerDown(bar, { clientX: 10, clientY: 10 });
    fireEvent.click(bar, { clientX: 60, clientY: 10 });
    expect(screen.queryByText('Colour for my-mesh')).toBeNull();
  });

});
