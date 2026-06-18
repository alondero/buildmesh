import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
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
  scratchpad: '',
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
    onOpenFilesProbe: vi.fn(),
    onOpenPropertiesProbe: vi.fn(),
    // Issue #378 — the right-click "GitHub Issues" / "Previous Agent Nodes"    // entries route through the Probe Panel via the new probe-tab
    // handlers. The legacy `onOpenGitHubIssues` / `onOpenSessionBrowser`
    // props are gone; the modal components stay on disk but no
    // consumer wires them up.
    onOpenIssuesProbe: vi.fn(),
    onOpenSessionHistoryProbe: vi.fn(),
    meshNodes: [],
    activeNodeId: null,
    setActiveNode: vi.fn(),
    selectMesh: vi.fn(),
    onDeleteNode: vi.fn(),
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
    expect(screen.getByText('Previous Agent Nodes')).toBeTruthy();
  });

  it('invokes the matching handler when a context-menu action is chosen', async () => {
    const { props } = renderMeshItem();
    fireEvent.contextMenu(screen.getByText('my-mesh'));
    await userEvent.click(screen.getByText('GitHub Issues'));
    expect(props.onOpenIssuesProbe).toHaveBeenCalledWith(3);
  });

  it('routes the right-click "GitHub Issues" entry to the Probe Panel (issue #378)', async () => {
    // Issue #378 — the "GitHub Issues" right-click item used to mount
    // the legacy `GitHubIssuesModal` via `onOpenGitHubIssues`. After the
    // port it calls `onOpenIssuesProbe`, which Sidebar wires to
    // `openProbeTab('issues')`.
    const { props } = renderMeshItem();
    fireEvent.contextMenu(screen.getByText('my-mesh'));
    await userEvent.click(screen.getByText('GitHub Issues'));
    expect(props.onOpenIssuesProbe).toHaveBeenCalledTimes(1);
    expect(props.onOpenIssuesProbe).toHaveBeenCalledWith(3);
  });

  it('routes the right-click "Previous Agent Nodes" entry to the Probe Panel (issue #378)', async () => {
    // Issue #378 — the "Previous Agent Nodes" right-click item used to
    // mount the legacy `SessionBrowserModal` via `onOpenSessionBrowser`.
    // After the port it calls `onOpenSessionHistoryProbe`, which
    // Sidebar wires to `openProbeTab('sessions')`.
    const { props } = renderMeshItem();
    fireEvent.contextMenu(screen.getByText('my-mesh'));
    await userEvent.click(screen.getByText('Previous Agent Nodes'));
    expect(props.onOpenSessionHistoryProbe).toHaveBeenCalledTimes(1);
    expect(props.onOpenSessionHistoryProbe).toHaveBeenCalledWith(3);
  });

  it('opens the probe panel on the files tab when "File Explorer" is chosen (#376)', async () => {
    // Issue #376 — the "File Explorer" right-click item used to call the
    // legacy `onToggleFileExplorer` (which opened FileExplorerPanel in the
    // SessionView left pane). After the port it calls `onOpenFilesProbe`,
    // which Sidebar wires to `openProbeTab('files')`.
    const { props } = renderMeshItem();
    fireEvent.contextMenu(screen.getByText('my-mesh'));
    await userEvent.click(screen.getByText('File Explorer'));
    expect(props.onOpenFilesProbe).toHaveBeenCalledTimes(1);
  });

  it('routes the right-click "Properties" entry to the Probe Panel (issue #375)', async () => {
    // The legacy right-rail drawer is no longer triggered from the
    // sidebar — the click now opens the Probe Panel on the ⚙️ tab.
    const { props } = renderMeshItem();
    fireEvent.contextMenu(screen.getByText('my-mesh'));
    await userEvent.click(screen.getByText('Properties'));
    expect(props.onOpenPropertiesProbe).toHaveBeenCalledWith(3);
  });

  it('routes the drift `!` badge to the Probe Panel (issue #375)', async () => {
    // Force the drift health snapshot so the badge renders, then click
    // it and assert the new probe-routing handler fires.
    vi.mocked(invoke).mockImplementation((cmd: string) =>
      cmd === 'get_mesh_health'
        ? Promise.resolve({
            is_dirty: false,
            is_drifted: true,
            unpushed_ahead: 0,
            base_branch_holder: null,
            local_base_branch: 'main',
            current_branch: 'feature/x',
            current_short_sha: 'abc1234',
            authenticated: false,
          })
        : Promise.resolve({}),
    );
    const { props } = renderMeshItem();
    const badge = await screen.findByLabelText('Mesh health issue');
    await userEvent.click(badge);
    expect(props.onOpenPropertiesProbe).toHaveBeenCalledWith(3);
  });

  it('renders a sync button in the header to the left of the Add Node form', async () => {
    renderMeshItem();
    expect(await screen.findByTitle('Sync from upstream')).toBeTruthy();
  });

  it('shows a behind-count badge when the branch is behind upstream', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) =>
      cmd === 'get_git_branch_status'
        ? Promise.resolve({ name: 'main', ahead: 0, behind: 17 })
        : Promise.resolve({}),
    );
    renderMeshItem();
    expect(await screen.findByText('↓17')).toBeTruthy();
  });

  it('hides the behind-count badge when the branch is up to date', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) =>
      cmd === 'get_git_branch_status'
        ? Promise.resolve({ name: 'main', ahead: 0, behind: 0 })
        : Promise.resolve({}),
    );
    renderMeshItem();
    await screen.findByTitle('Sync from upstream');
    expect(screen.queryByText(/↓/)).toBeNull();
  });

  it('runs gitSync, spins, and shows the result message when the sync button is clicked', async () => {
    let resolveSync!: (v: unknown) => void;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'git_sync') return new Promise((res) => { resolveSync = res; });
      return Promise.resolve({});
    });
    renderMeshItem();

    await userEvent.click(await screen.findByTitle('Sync from upstream'));

    // While the pull is in flight the button reflects the syncing state.
    const spinning = screen.getByTitle('Syncing…');
    expect(spinning.querySelector('.animate-spin')).toBeTruthy();

    resolveSync({ fetched: true, pulled: true, new_commits: 3, message: 'Pulled 3 commits' });

    expect(await screen.findByText('Pulled 3 commits')).toBeTruthy();
    expect(vi.mocked(invoke)).toHaveBeenCalledWith('git_sync', { path: '/tmp/my-mesh' });
  });
});
