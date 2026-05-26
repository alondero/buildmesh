import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { DndContext } from '@dnd-kit/core';
import { SortableContext } from '@dnd-kit/sortable';
import { MeshItem } from '../../src/components/Sidebar/MeshItem';
import type { Mesh } from '../../src/stores/meshStore';
import type { AgentNode } from '../../src/stores/agentNodeStore';
import type { ProviderEntry } from '../../src/components/Sidebar/ProviderDropdown';

const MESH: Mesh = {
  id: 3,
  name: 'my-mesh',
  path: '/tmp/my-mesh',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
};

const PROVIDERS: ProviderEntry[] = [
  { id: 'anthropic', label: 'Anthropic', color: 'bg-blue-500' },
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

type Props = React.ComponentProps<typeof MeshItem>;

function renderMeshItem(overrides: Partial<Props> = {}) {
  const props: Props = {
    mesh: MESH,
    isSelected: false,
    isDropdownOpen: false,
    providerList: PROVIDERS,
    onSelectMesh: vi.fn(),
    onNewNode: vi.fn(),
    onSelectProvider: vi.fn(),
    onOpenProperties: vi.fn(),
    onToggleFileExplorer: vi.fn(),
    meshNodes: [],
    activeNodeId: null,
    setActiveNode: vi.fn(),
    selectMesh: vi.fn(),
    onDeleteNode: vi.fn(),
    onOpenGitHubIssues: vi.fn(),
    onOpenSessionBrowser: vi.fn(),
    getDefaultProvider: vi.fn().mockResolvedValue('anthropic'),
    ...overrides,
  };
  const result = render(
    <DndContext>
      <SortableContext items={[MESH.id]}>
        <MeshItem {...props} />
      </SortableContext>
    </DndContext>,
  );
  return { ...result, props };
}

describe('MeshItem', () => {
  it('renders the mesh name and a drag handle', () => {
    renderMeshItem();
    expect(screen.getByText('my-mesh')).toBeTruthy();
    expect(screen.getByTitle('Drag to reorder')).toBeTruthy();
  });

  it('applies the selected styling only when selected', () => {
    const { rerender, props } = renderMeshItem({ isSelected: false });
    const header = screen.getByText('my-mesh').closest('div[class*="border-l-3"]')!;
    expect(header.className).not.toContain('bg-bg-card ');

    rerender(
      <DndContext>
        <SortableContext items={[MESH.id]}>
          <MeshItem {...props} isSelected />
        </SortableContext>
      </DndContext>,
    );
    const selectedHeader = screen.getByText('my-mesh').closest('div[class*="border-l-3"]')!;
    expect(selectedHeader.className).toContain('bg-bg-card');
  });

  it('calls onSelectMesh when the header is clicked', async () => {
    const { props } = renderMeshItem();
    await userEvent.click(screen.getByText('my-mesh'));
    expect(props.onSelectMesh).toHaveBeenCalledWith(3);
  });

  it('renders a NodeItem per mesh node and selects it on click', async () => {
    const { props } = renderMeshItem({ meshNodes: [makeNode()] });
    await userEvent.click(screen.getByText('node-a'));
    expect(props.setActiveNode).toHaveBeenCalledWith(10);
    expect(props.selectMesh).toHaveBeenCalledWith(3);
  });

  it('opens a context menu with periphery actions on right-click', () => {
    renderMeshItem();
    fireEvent.contextMenu(screen.getByText('my-mesh'));
    expect(screen.getByText('Properties')).toBeTruthy();
    expect(screen.getByText('File Explorer')).toBeTruthy();
    expect(screen.getByText('GitHub Issues')).toBeTruthy();
    expect(screen.getByText('Previous Sessions')).toBeTruthy();
  });

  it('invokes the matching handler when a context-menu action is chosen', async () => {
    const { props } = renderMeshItem();
    fireEvent.contextMenu(screen.getByText('my-mesh'));
    await userEvent.click(screen.getByText('GitHub Issues'));
    expect(props.onOpenGitHubIssues).toHaveBeenCalledWith(3);
  });
});
