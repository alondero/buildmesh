import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor, cleanup, act } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import type { ComponentProps } from 'react';
import {
  DndContext,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import {
  SortableContext,
  sortableKeyboardCoordinates,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { MeshItem } from '../../src/components/Sidebar/MeshItem';
import type { Mesh } from '../../src/stores/meshStore';
import type { AgentNode } from '../../src/stores/agentNodeStore';
import type { SpawnOption } from '../../src/lib/groups';
import type { NodeActivityCluster } from '../../src/lib/nodeActivities';

// `@tauri-apps/plugin-opener`'s `openUrl` shells out to the OS. Mock it
// at file scope so the new "View on GitHub" click test (and any
// future tests that need to assert the click route) can spy on the
// call. `vi.hoisted` so the mock factory can capture the spy ref
// before the `vi.mock` call hoists the module replacement — same
// pattern as git-issues-tab.test.tsx:33-38.
const { openUrlMock } = vi.hoisted(() => ({
  openUrlMock: vi.fn<[], Promise<void>>().mockResolvedValue(undefined),
}));
vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: openUrlMock,
}));

// Row-body execution counter for the #1748 memo tests below, mirroring
// the CountingNodeItem seam in sidebar-render-count.test.tsx: the mocked
// spawn form renders exactly once per MeshItem body execution, so a
// skipped memo re-render reads as a flat counter across a rerender with
// fresh-but-equal props. The wrapper renders the real form, so every
// other test in this file still sees the real spawn affordance.
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

/** A cluster of one — what `clusterActivityNodes` returns for an unpaired node,
 *  and the shape the sidebar renders as a bare row (no rail, no indent). */
function loneCluster(node: AgentNode): NodeActivityCluster {
  return { root: node, members: [node], paired: false, handGrouped: false };
}

type Props = React.ComponentProps<typeof MeshItem>;

// Stable across renders: a fresh items array would churn SortableContext
// and re-render the row underneath the memo boundary (issue #1748), which
// is not what the isolation test measures.
const SORTABLE_IDS = [MESH.id];

/**
 * The clickable header inside this mesh's card — the card root's single
 * direct child div (colour bar + text column + spawn form), and the
 * element the context menu is anchored to and returns focus to. Reached
 * through the mesh-name span so it stays unambiguous; the pre-fold-in
 * row carried a `border-l-3` left accent to hook onto and this one has
 * the `data-mesh-card` seam instead.
 */
function headerEl(): HTMLElement {
  const header = screen.getByText('my-mesh').closest('div[data-mesh-card] > div');
  if (!header) throw new Error('mesh header div not found');
  return header as HTMLElement;
}

/**
 * The colour bar — BOTH the recolour picker and the dnd-kit reorder
 * handle (the separate `⋮⋮` glyph and the round swatch were folded into
 * it). Looked up by regex prefix everywhere the name is only a locator;
 * the exact accessible name is pinned by its own test below.
 */
function colourBar(name = 'my-mesh') {
  return screen.getByRole('button', { name: new RegExp(`^Change mesh colour for ${name} `) });
}

function renderMeshItem(overrides: Partial<Props> = {}) {
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
    // Issue #378 — the right-click "GitHub Issues" / "Archive" entries route
    // through the Probe Panel via the new probe-tab handlers. The legacy
    // `onOpenGitHubIssues` / `onOpenSessionBrowser` props are gone; the
    // modal components stay on disk but no consumer wires them up.
    onOpenIssuesProbe: vi.fn(),
    onOpenSessionHistoryProbe: vi.fn(),
    // Paired agents are clustered by Node Activity in `Sidebar` (via
    // `clusterActivityNodes`) and handed down as clusters, so a mesh with no
    // pairings passes an empty cluster list rather than a flat node list.
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
        <MeshItem {...p} />
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

describe('MeshItem', () => {
  beforeEach(() => {
    // Reset the file-scoped openUrl mock between tests so a per-test
    // `mockImplementationOnce` from a sibling doesn't leak into the
    // next "View on GitHub" click test (and silently mask an
    // assertion failure). Mirrors git-issues-tab.test.tsx:124-126.
    openUrlMock.mockReset();
    openUrlMock.mockResolvedValue(undefined);
    // Reset the global `invoke` mock implementation so per-test
    // `mockImplementation` calls (e.g. the URL-returning mock the
    // "View on GitHub" tests install) don't leak into the next
    // sibling — without this, the pre-existing keyboard-nav tests
    // (which assume 5 menuitems) see a 6-item menu because the
    // previous test's URL mock is still active. `mockReset` wipes
    // the implementation AND call history, so we re-apply the
    // default `Promise.resolve({})` afterwards. `vi.clearAllMocks`
    // in the global setup only clears history, not implementation.
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockImplementation((_cmd: string) => Promise.resolve({}));
  });

  // RTL doesn't auto-unmount in this vitest setup, so the previous
  // render's DOM (with its own context menu + keydown listener) would
  // still be in the document — `getAllByRole('[role="menuitem"]')`
  // then returns 2× the items and ArrowDown's focus lands on the
  // wrong element. The PR-tab test file does the same. Mirrors
  // git-pull-requests-tab.test.tsx:181-183.
  afterEach(() => {
    cleanup();
  });

  it('renders the mesh name and a drag handle', () => {
    renderMeshItem();
    expect(screen.getByText('my-mesh')).toBeTruthy();
    // The colour bar is the reorder handle — the separate `⋮⋮` glyph is
    // gone, so the handle is found through the bar's title.
    expect(screen.getByTitle('Drag to reorder my-mesh · click or Enter to change mesh colour')).toBeTruthy();
  });

  it('applies the selected styling only when selected', async () => {
    const { rerenderWith } = renderMeshItem({ isSelected: false });
    expect(headerEl().className.split(/\s+/)).not.toContain('bg-bg-card');

    rerenderWith({ isSelected: true });
    // Polled, not raced: the commit that follows a `rerender` can land a
    // tick late when a sibling's async hook resolves at the same moment.
    await waitFor(() => expect(headerEl().className.split(/\s+/)).toContain('bg-bg-card'));
  });

  it('calls onSelectMesh when the header is clicked', async () => {
    const { props } = renderMeshItem();
    await userEvent.click(screen.getByText('my-mesh'));
    expect(props.onSelectMesh).toHaveBeenCalledWith(3);
  });

  it('renders a NodeItem per mesh node and selects it on click', async () => {
    const { props } = renderMeshItem({ nodeClusters: [loneCluster(makeNode())] });
    // The seeded node is `running`, which is not "hot", so the card
    // starts collapsed and the cluster is not in the DOM yet. Open the
    // dots line first so the assertion is about the row, not the toggle.
    expect(screen.queryByText('node-a')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Show agents for my-mesh' }));
    await userEvent.click(screen.getByText('node-a'));
    expect(props.onActivateNode).toHaveBeenCalledWith(10);
    expect(props.selectMesh).toHaveBeenCalledWith(3);
  });

  it('opens a context menu with periphery actions on right-click', () => {
    renderMeshItem();
    fireEvent.contextMenu(screen.getByText('my-mesh'));
    expect(screen.getByText('Properties')).toBeTruthy();
    expect(screen.getByText('File Explorer')).toBeTruthy();
    expect(screen.getByText('GitHub Issues')).toBeTruthy();
    expect(screen.getByText('Archive')).toBeTruthy();
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

  it('routes the right-click "Archive" entry to the Probe Panel (issue #378)', async () => {
    // Issue #378 — the "Archive" (formerly "Previous Agent Nodes")
    // right-click item used to mount the legacy `SessionBrowserModal`
    // via `onOpenSessionBrowser`. After the port it calls
    // `onOpenSessionHistoryProbe`, which Sidebar wires to
    // `openProbeTab('sessions')`. The button shows the short "Archive"
    // label with a longer "Archived Nodes" tooltip.
    const { props } = renderMeshItem();
    fireEvent.contextMenu(screen.getByText('my-mesh'));
    await userEvent.click(screen.getByText('Archive'));
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

  it('routes the drift `!` badge to the Worktree Manager probe (issue #767)', async () => {
    // Issue #767 — the badge previously opened the ⚙️ Mesh Properties
    // tab (issue #375), but the recovery actions (Restore/Free the
    // hostage branch, prune remote-tracking refs) live on the 🌳
    // Worktree Manager tab. The right-click "Properties" item still
    // routes to ⚙️; only the badge moves.
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
    const badge = await screen.findByLabelText(/^Mesh health issue/);
    await userEvent.click(badge);
    expect(props.onOpenWorktreesProbe).toHaveBeenCalledWith(3);
  });

  // Companion to the drift case above: the badge's predicate is an OR
  // (`is_drifted || base_branch_holder !== null`). Pre-#767 both branches
  // routed to Properties (a dead-end for the hostage case), so this gap
  // didn't matter; post-#767 they share a destination, but a regression
  // that breaks rendering when only the hostage branch is set would now
  // slip past CI. Lock both halves of the OR.
  it('routes the hostage-branch `!` badge to the Worktree Manager probe (issue #767)', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) =>
      cmd === 'get_mesh_health'
        ? Promise.resolve({
            is_dirty: false,
            is_drifted: false,
            unpushed_ahead: 0,
            base_branch_holder: { path: '/tmp/other-wt', name: 'other-wt', is_active: true },
            local_base_branch: 'main',
            current_branch: 'main',
            current_short_sha: 'abc1234',
            authenticated: false,
          })
        : Promise.resolve({}),
    );
    const { props } = renderMeshItem();
    const badge = await screen.findByLabelText(/^Mesh health issue/);
    await userEvent.click(badge);
    expect(props.onOpenWorktreesProbe).toHaveBeenCalledWith(3);
  });

  // The header carries no *always-on* sync button — syncs are automatic
  // (background sync + spawn-time auto-sync) and a permanent button
  // duplicated the Regenerate icon for an action the user rarely needs
  // (ADR 0020). The card has no textual status line either, so the
  // failure-only badge IS the retry affordance — it must appear only
  // once the backend has actually reported a failed sync for this mesh.
  it('renders no always-on sync control in the header — the retry badge appears only after a failed sync', async () => {
    /** Header buttons that read as a sync control (matched on the visible
     *  label, so a future rename of the badge's copy can't silently make
     *  this pass again). */
    const syncControls = () => Array.from(headerEl().querySelectorAll('button')).filter(
      (b) => /sync/i.test(`${b.title} ${b.getAttribute('aria-label') ?? ''}`),
    );

    renderMeshItem();
    expect(syncControls()).toHaveLength(0);
    expect(screen.queryByLabelText(/^Sync failed for /)).toBeNull();

    await act(async () => {
      await emit('mesh-sync-warning', {
        node_id: 10,
        mesh_path: MESH.path,
        outcome: 'fetch_failed',
        new_commits: null,
        pr_number: null,
        head_ref: null,
        expected_sha: null,
        actual_sha: null,
        fallback_base_ref: null,
        head_repo_owner: null,
        head_repo_clone_url: null,
        message: 'fetch failed: network down',
      });
    });

    const retry = await screen.findByLabelText(/^Sync failed for /);
    // It is the same element the header grew — not a card-level banner.
    expect(headerEl().contains(retry)).toBe(true);
    expect(syncControls()).toHaveLength(1);
  });

  it('lights the sync-failure badge when the backend reports a failed sync for this mesh', async () => {
    // Spawn-time auto-sync failures (fetch failed, diverged history, …)
    // arrive as a `mesh-sync-warning` Tauri event carrying the mesh path
    // that failed. The card has no textual status, so the badge is a
    // button: click it to retry the same `git_sync` the context menu's
    // "Force sync from upstream" runs.
    renderMeshItem();
    expect(screen.queryByLabelText(/^Sync failed for /)).toBeNull();

    await act(async () => {
      await emit('mesh-sync-warning', {
        node_id: 10,
        mesh_path: MESH.path,
        outcome: 'fetch_failed',
        new_commits: null,
        pr_number: null,
        head_ref: null,
        expected_sha: null,
        actual_sha: null,
        fallback_base_ref: null,
        head_repo_owner: null,
        head_repo_clone_url: null,
        message: 'fetch failed: network down',
      });
    });

    const badge = await screen.findByLabelText('Sync failed for my-mesh — click to retry');
    expect(badge.tagName).toBe('BUTTON');
    expect(badge.getAttribute('title')).toBe('Last sync from upstream failed — click to retry.');
  });

  it('ignores mesh-sync-warning events for other meshes', async () => {
    renderMeshItem();
    await act(async () => {
      await emit('mesh-sync-warning', {
        node_id: 10,
        mesh_path: '/tmp/some-other-mesh',
        outcome: 'diverged',
        new_commits: null,
        pr_number: null,
        head_ref: null,
        expected_sha: null,
        actual_sha: null,
        fallback_base_ref: null,
        head_repo_owner: null,
        head_repo_clone_url: null,
        message: 'diverged from upstream',
      });
    });

    // Give any (incorrect) state update a tick to flush, then assert the
    // icon never lit.
    await waitFor(() => {});
    expect(screen.queryByLabelText(/^Sync failed for /)).toBeNull();

    // Positive control in the same test: the label exists in the DOM once
    // an event for THIS mesh's path arrives. Without it, the assertion
    // above would still pass if the listener were dead or the label had
    // been renamed out from under the test — i.e. vacuously.
    await act(async () => {
      await emit('mesh-sync-warning', {
        node_id: 10,
        mesh_path: MESH.path,
        outcome: 'diverged',
        new_commits: null,
        pr_number: null,
        head_ref: null,
        expected_sha: null,
        actual_sha: null,
        fallback_base_ref: null,
        head_repo_owner: null,
        head_repo_clone_url: null,
        message: 'diverged from upstream',
      });
    });
    expect(await screen.findByLabelText(/^Sync failed for /)).toBeTruthy();
  });

  it('retries the sync when the failure badge is clicked', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) =>
      cmd === 'git_sync' ? Promise.resolve({ fetched: true, pulled: true, new_commits: 0, message: 'Already up to date' }) : Promise.resolve({}),
    );
    renderMeshItem();
    await act(async () => {
      await emit('mesh-sync-warning', {
        node_id: 10,
        mesh_path: MESH.path,
        outcome: 'fetch_failed',
        new_commits: null,
        pr_number: null,
        head_ref: null,
        expected_sha: null,
        actual_sha: null,
        fallback_base_ref: null,
        head_repo_owner: null,
        head_repo_clone_url: null,
        message: 'fetch failed: network down',
      });
    });

    const badge = await screen.findByLabelText(/^Sync failed for /);
    await userEvent.click(badge);

    expect(vi.mocked(invoke)).toHaveBeenCalledWith('git_sync', { path: '/tmp/my-mesh' });
    await waitFor(() => expect(screen.queryByLabelText(/^Sync failed for /)).toBeNull());
  });

  it('keeps the failure badge lit when a retry fails', async () => {
    // A failed retry is the only feedback the card gives (no textual
    // status line), so the badge must survive it — clearing it here would
    // leave a possibly-stale mesh looking fresh.
    vi.mocked(invoke).mockImplementation((cmd: string) =>
      cmd === 'git_sync' ? Promise.reject(new Error('fetch failed: network down')) : Promise.resolve({}),
    );
    renderMeshItem();
    await act(async () => {
      await emit('mesh-sync-warning', {
        node_id: 10,
        mesh_path: MESH.path,
        outcome: 'fetch_failed',
        new_commits: null,
        pr_number: null,
        head_ref: null,
        expected_sha: null,
        actual_sha: null,
        fallback_base_ref: null,
        head_repo_owner: null,
        head_repo_clone_url: null,
        message: 'fetch failed: network down',
      });
    });

    const badge = await screen.findByLabelText(/^Sync failed for /);
    await userEvent.click(badge);

    // The rejected sync logs through `console.error`; the badge is the
    // assertion. Re-enable the button (`disabled` while syncing) too, so
    // the retry affordance is usable again.
    await waitFor(() => expect(badge.hasAttribute('disabled')).toBe(false));
    expect(screen.getByLabelText(/^Sync failed for /)).toBe(badge);
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
    await screen.findByText('my-mesh');
    expect(screen.queryByText(/↓/)).toBeNull();
  });

  it('offers "Force sync from upstream" in the context menu and runs gitSync when clicked', async () => {
    let resolveSync!: (v: unknown) => void;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'git_sync') return new Promise((res) => { resolveSync = res; });
      return Promise.resolve({});
    });
    renderMeshItem();

    fireEvent.contextMenu(screen.getByText('my-mesh'));
    await userEvent.click(screen.getByText('Force sync from upstream'));

    // The menu closes on click (the sync runs in the sidebar row, not the
    // menu). The card carries no textual status line any more, so the
    // feedback surface after a force sync is the failure badge clearing —
    // pinned in the test below.
    expect(screen.queryByText('Force sync from upstream')).toBeNull();
    expect(vi.mocked(invoke)).toHaveBeenCalledWith('git_sync', { path: '/tmp/my-mesh' });

    resolveSync({ fetched: true, pulled: true, new_commits: 3, message: 'Pulled 3 commits' });
    await act(async () => {});
  });

  it('clears the failure badge after a successful force sync', async () => {
    // The stale indicator must be reset by a successful manual sync —
    // otherwise the user stays alarmed after the mesh is demonstrably
    // fresh again.
    vi.mocked(invoke).mockImplementation((cmd: string) =>
      cmd === 'git_sync' ? Promise.resolve({ fetched: true, pulled: true, new_commits: 0, message: 'Already up to date' }) : Promise.resolve({}),
    );
    renderMeshItem();
    await act(async () => {
      await emit('mesh-sync-warning', {
        node_id: 10,
        mesh_path: MESH.path,
        outcome: 'fetch_failed',
        new_commits: null,
        pr_number: null,
        head_ref: null,
        expected_sha: null,
        actual_sha: null,
        fallback_base_ref: null,
        head_repo_owner: null,
        head_repo_clone_url: null,
        message: 'fetch failed: network down',
      });
    });
    await screen.findByLabelText(/^Sync failed for /);

    fireEvent.contextMenu(screen.getByText('my-mesh'));
    await userEvent.click(screen.getByText('Force sync from upstream'));

    await waitFor(() => expect(screen.queryByLabelText(/^Sync failed for /)).toBeNull());
  });

  // The card's header: dots + count (no textual status), the colour bar
  // that is both picker and reorder handle, and the expand/collapse
  // contract.
  describe('card header, dots line and colour bar', () => {
    it('renders the mesh name with dots and a count, and no textual status labels', () => {
      renderMeshItem({ nodeClusters: [loneCluster(makeNode())] });
      expect(screen.getByText('my-mesh')).toBeTruthy();
      // Dots carry status without words: none of the status vocabulary
      // appears as text…
      for (const word of ['Running', 'Idle', 'Needs attention', 'Starting…', 'Ready', 'Suspended', 'Error', 'Lost', 'PR opened', 'Archived']) {
        expect(screen.queryByText(word)).toBeNull();
      }
      // …but it is all still exposed to assistive tech on the dots span,
      // so the check above is a real "dots, not words" assertion and not
      // a missing-status one.
      expect(screen.getByRole('img', { name: '1 agent: Running' })).toBeTruthy();
      expect(screen.getByText('1')).toBeTruthy();
    });

    it('starts collapsed for quiet meshes and expands via the dots line', async () => {
      renderMeshItem({ nodeClusters: [loneCluster(makeNode({ status: 'idle' }))] });
      expect(screen.queryByText('node-a')).toBeNull();
      await userEvent.click(screen.getByRole('button', { name: 'Show agents for my-mesh' }));
      expect(screen.getByText('node-a')).toBeTruthy();
      await userEvent.click(screen.getByRole('button', { name: 'Hide agents for my-mesh' }));
      expect(screen.queryByText('node-a')).toBeNull();
    });

    it('starts expanded for meshes with a node needing attention', () => {
      renderMeshItem({ nodeClusters: [loneCluster(makeNode({ id: 11, name: 'node-b', status: 'awaiting_input' }))] });
      expect(screen.getByText('node-b')).toBeTruthy();
    });

    it('makes the colour bar both picker and reorder handle', () => {
      renderMeshItem();
      // One element, two gestures: the accessible name and the tooltip
      // both carry the merge, so neither gesture is undiscoverable.
      const bar = screen.getByRole('button', { name: 'Change mesh colour for my-mesh — Enter to open, Space to pick up for reordering' });
      expect(bar.title).toBe('Drag to reorder my-mesh · click or Enter to change mesh colour');
    });

    it('keeps the header out of the tab order and announces the bar as sortable', () => {
      renderMeshItem({ nodeClusters: [loneCluster(makeNode())] });
      const bar = colourBar();
      expect(bar.getAttribute('aria-roledescription')).toBe('sortable');
      // The header is a plain clickable div, not a widget: no role would
      // make the buttons nested inside it invalid descendants, and
      // tabindex -1 keeps it out of the natural Tab order while still
      // giving the context menu a focus target to return to.
      const header = bar.closest('[data-mesh-card]')?.firstElementChild;
      expect(header?.getAttribute('tabindex')).toBe('-1');
      expect(header?.getAttribute('role')).toBeNull();
    });

    it('opens the picker on a mouse press-and-release in place', () => {
      renderMeshItem();
      const bar = colourBar();
      fireEvent.pointerDown(bar, { clientX: 10, clientY: 10 });
      fireEvent.click(bar, { clientX: 10, clientY: 10, detail: 1 });
      expect(screen.getByText('Colour for my-mesh')).toBeTruthy();
    });

    it('swallows the trailing click after the pointer travelled (a drag ran)', () => {
      renderMeshItem();
      const bar = colourBar();
      fireEvent.pointerDown(bar, { clientX: 10, clientY: 10 });
      // A real mouse click carries detail >= 1 (fireEvent defaults to 0, which
      // is the keyboard/AT path and must open).
      fireEvent.click(bar, { clientX: 60, clientY: 10, detail: 1 });
      expect(screen.queryByText('Colour for my-mesh')).toBeNull();
    });

    it('does not open the picker after a real drag sequence ending on the bar', () => {
      // pointerup always precedes click in a browser: the press record must
      // survive it, or the distance check can never run.
      renderMeshItem();
      const bar = colourBar();
      fireEvent.pointerDown(bar, { clientX: 10, clientY: 10 });
      fireEvent.pointerUp(bar, { clientX: 60, clientY: 10 });
      fireEvent.click(bar, { clientX: 60, clientY: 10, detail: 1 });
      expect(screen.queryByText('Colour for my-mesh')).toBeNull();
    });

    it('does not select the mesh on the trailing click of a travelled press', () => {
      const { props } = renderMeshItem();
      const bar = colourBar();
      fireEvent.pointerDown(bar, { clientX: 10, clientY: 10 });
      fireEvent.click(bar, { clientX: 60, clientY: 10, detail: 1 });
      expect(props.onSelectMesh).not.toHaveBeenCalled();
    });

    it('opens the picker on AT activation even after an off-target travelled press', () => {
      renderMeshItem();
      const bar = colourBar();
      fireEvent.pointerDown(bar, { clientX: 100, clientY: 100 });
      fireEvent.pointerUp(document.body, { clientX: 400, clientY: 100 });
      expect(screen.queryByText('Colour for my-mesh')).toBeNull();
      fireEvent.click(bar, { detail: 0 });
      expect(screen.getByText('Colour for my-mesh')).toBeTruthy();
    });

    it('splits Enter (open the picker) from Space (dnd-kit pickup) on the bar', () => {
      // dnd-kit's KeyboardSensor claims BOTH Space and Enter as pickup
      // keys, so the row intercepts Enter to open the colour picker (what
      // the pre-merge round swatch button did) and chains every other key
      // — Space included — straight to the sensor activator.
      renderMeshItem();
      const bar = colourBar();

      fireEvent.keyDown(bar, { key: 'Enter', code: 'Enter' });
      expect(screen.getByText('Colour for my-mesh')).toBeTruthy();
      // Enter is NOT a drag pickup: the sensor never saw the key.
      expect(bar.getAttribute('aria-pressed')).not.toBe('true');
    });

    it('leaves Space to the reorder sensor and does not open the picker', async () => {
      // The keyboard-drag sequence itself (pickup → ArrowDown → drop) is
      // covered in the `keyboard drag handle a11y` suite below, which
      // mounts the multi-row harness the sensor needs an `over` target
      // for. Here we only pin the negative half: Space must not double as
      // "open the picker", otherwise a keyboard user reordering a mesh
      // gets a modal in the face.
      const { props } = renderMeshItem();
      const bar = colourBar();

      fireEvent.keyDown(bar, { key: ' ', code: 'Space' });
      expect(bar.getAttribute('aria-pressed')).toBe('true');
      await waitFor(() => expect(screen.queryByText('Colour for my-mesh')).toBeNull());
      // A pickup must not select the mesh either.
      expect(props.onSelectMesh).not.toHaveBeenCalled();
    });

    it('expands when heat arrives after mount, but yields to a manual collapse', async () => {
      const hot = makeNode({ id: 11, name: 'node-hot', status: 'awaiting_input' });
      const { rerenderWith } = renderMeshItem({ nodeClusters: [] });
      expect(screen.queryByText('node-hot')).toBeNull();

      rerenderWith({ nodeClusters: [loneCluster(hot)] });
      // `findByText` because the "heat arrived" expansion is an effect on
      // the incoming cluster list, so the node is not guaranteed to be in
      // the DOM the instant `rerender` returns.
      expect(await screen.findByText('node-hot')).toBeTruthy();

      await userEvent.click(screen.getByRole('button', { name: 'Hide agents for my-mesh' }));
      expect(screen.queryByText('node-hot')).toBeNull();
      rerenderWith({ nodeClusters: [loneCluster({ ...hot, id: 12, name: 'node-hotter' })] });
      expect(screen.queryByText('node-hotter')).toBeNull();
    });
  });

  // Issue #1748 — the memo boundary on the row. The counting
  // `NodeCreationForm` wrapper at the top of this file provides the
  // render seam; the per-mesh async hooks are held still by the
  // never-resolving `invoke` below (the same isolation the prototype
  // file got from mocking `useMeshHealth` / `useGitBranchStatus` at module
  // scope) so no background fetch can self-render the row inside the
  // measurement window.
  describe('row memoization (issue #1748)', () => {
    beforeEach(() => {
      vi.mocked(invoke).mockImplementation((cmd: string) =>
        ['get_mesh_health', 'get_git_branch_status', 'get_github_url_for_mesh'].includes(cmd)
          ? new Promise<never>(() => {})
          : Promise.resolve({}),
      );
    });

    it('skips re-render when clusters are fresh arrays with identical members (#1748)', async () => {
      const node = makeNode({ status: 'idle' });
      const { rerenderWith } = renderMeshItem({ nodeClusters: [loneCluster(node)] });
      await act(async () => {});
      rowBodyRenders.count = 0;

      // Fresh cluster wrappers, identical member references — what
      // Sidebar's grouped map produces on every unrelated store update.
      rerenderWith({ nodeClusters: [loneCluster(node)] });
      await act(async () => {});
      expect(rowBodyRenders.count).toBe(0);
      // (The sibling test changes a member and watches the same counter
      // move, so a dead counter can't make this pass vacuously.)
    });

    it('still re-renders when a member actually changes', async () => {
      const node = makeNode({ status: 'idle' });
      const { rerenderWith } = renderMeshItem({ nodeClusters: [loneCluster(node)] });
      await act(async () => {});
      rowBodyRenders.count = 0;

      rerenderWith({ nodeClusters: [loneCluster({ ...node, status: 'running' })] });
      // Polled for the same reason as the sibling test's "stays at 0":
      // the counter is the observation, and a commit that lands a tick
      // late must not read as "the row skipped its re-render".
      await vi.waitFor(() => expect(rowBodyRenders.count).toBeGreaterThan(0));
    });
  });

  // Issue #735 — viewport clamping + WAI-ARIA menu keyboard nav.
  // The context menu must have `role="menu"`, items `role="menuitem"`,
  // an `aria-labelledby` pointing at the mesh-name span, a roving tabindex
  // (only the active item is in the Tab order), arrow / Home / End focus
  // traversal with wrap-around, an autofocus-on-open, Escape closes and
  // returns focus to the trigger, and viewport clamping at edges.
  describe('context menu a11y + keyboard nav (issue #735)', () => {
    /**
     * Right-click on the mesh header with custom clientX/Y. Uses
     * `fireEvent.contextMenu` (not a raw `dispatchEvent`) so React's
     * delegated `onContextMenu` receives the MouseEvent with both
     * `clientX`/`clientY` and the trigger's `e.preventDefault()` path.
     */
    function openContextMenu(clientX = 100, clientY = 200) {
      fireEvent.contextMenu(headerEl(), { clientX, clientY });
    }

    // The MeshItem's keydown handler is attached to `document` (not window).
    // jsdom treats window and document as independent event targets, so the
    // existing convention from `worktree-close-dialog.test.tsx` (fire on
    // `window`) does NOT reach our listener — we must target `document`.
    function pressKey(key: string) {
      fireEvent.keyDown(document, { key });
    }

    /**
     * Stub the menu's `getBoundingClientRect` so it tracks the rendered
     * inline `top`/`left`. This keeps the stub honest across re-renders
     * (a clamped value that the stub still reports as overflowing would
     * send the `useLayoutEffect` into a setState loop).
     */
    function stubRect(menu: HTMLElement, width = 200, height = 220) {
      menu.getBoundingClientRect = function (this: HTMLElement) {
        const left = parseFloat(this.style.left || '0');
        const top = parseFloat(this.style.top || '0');
        return {
          width,
          height,
          top,
          left,
          right: left + width,
          bottom: top + height,
          x: left,
          y: top,
          toJSON() { return {}; },
        } as DOMRect;
      };
    }

    it('marks the menu container with role="menu" and each item with role="menuitem"', () => {
      // The mock returns no `get_github_url_for_mesh` resolution, so
      // the new "View on GitHub" item is conditionally hidden — the
      // menu shape stays at the pre-#758 5-item form. A follow-up
      // test pins the 6-item form when the URL is available.
      renderMeshItem();
      openContextMenu();
      const menu = document.querySelector('[role="menu"]')!;
      expect(menu).toBeTruthy();
      // Five menuitems, in render order: Properties, File Explorer,
      // Force sync from upstream, Archive, GitHub Issues.
      const items = document.querySelectorAll('[role="menuitem"]');
      expect(items).toHaveLength(5);
      expect(items[0].textContent).toMatch(/Properties/);
      expect(items[1].textContent).toMatch(/File Explorer/);
      expect(items[2].textContent).toMatch(/Force sync from upstream/);
      expect(items[3].textContent).toMatch(/Archive/);
      expect(items[4].textContent).toMatch(/GitHub Issues/);
    });

    it('paints menu rows with the contrasting hover surface, not bg-bg-card', () => {
      // The menu sits on `bg-bg-overlay`; `hover:bg-bg-card` is ~1/255
      // brighter and therefore invisible. Align with the app's menu-row
      // pattern (CommandOmnibar / QuickConnectMenu).
      renderMeshItem();
      openContextMenu();
      const items = Array.from(document.querySelectorAll('[role="menuitem"]'));
      expect(items.length).toBeGreaterThan(0);
      for (const item of items) {
        const classes = item.className.split(/\s+/);
        expect(classes).not.toContain('hover:bg-bg-card');
        expect(classes).toContain('hover:bg-bg-card-hover');
      }
    });

    it('adds a "View on GitHub" item as the 6th menuitem when the mesh has a GitHub origin', async () => {
      // Wire `get_github_url_for_mesh` to resolve a URL so the
      // conditional render in MeshItem shows the new item. The test
      // also re-asserts the base 5 items in render order so a future
      // refactor that shuffles the menu doesn't break this contract.
      vi.mocked(invoke).mockImplementation((cmd: string) => {
        if (cmd === 'get_github_url_for_mesh') {
          return Promise.resolve('https://github.com/acme/my-mesh');
        }
        return Promise.resolve({});
      });
      renderMeshItem();
      openContextMenu();

      // Wait for the hook's IPC to resolve and the conditional item
      // to mount — `findByText` is the async query that retries.
      await screen.findByText('View on GitHub');
      const items = document.querySelectorAll('[role="menuitem"]');
      expect(items).toHaveLength(6);
      expect(items[0].textContent).toMatch(/Properties/);
      expect(items[1].textContent).toMatch(/File Explorer/);
      expect(items[2].textContent).toMatch(/Force sync from upstream/);
      expect(items[3].textContent).toMatch(/Archive/);
      expect(items[4].textContent).toMatch(/GitHub Issues/);
      expect(items[5].textContent).toMatch(/View on GitHub/);
    });

    it('hides "View on GitHub" when the mesh has no GitHub origin (returns null)', async () => {
      // The default `Promise.resolve({})` for unhandled commands
      // satisfies the contract "no URL" — the hook collapses to
      // `url === null` and the conditional render drops the item.
      // Pin the explicit-null branch too: a future refactor that
      // changes the IPC to throw on non-GitHub meshes would shift
      // the test to the error branch and would no longer match
      // `Promise.resolve(null)` here.
      vi.mocked(invoke).mockImplementation((cmd: string) => {
        if (cmd === 'get_github_url_for_mesh') return Promise.resolve(null);
        return Promise.resolve({});
      });
      renderMeshItem();
      openContextMenu();

      // Give the IPC a tick to resolve, then assert the item is
      // absent. `queryByText` returns null (not throws) for absent
      // elements, so a small wait + query is enough.
      await waitFor(() => {
        expect(screen.queryByText('View on GitHub')).toBeNull();
      });
      const items = document.querySelectorAll('[role="menuitem"]');
      expect(items).toHaveLength(5);
    });

    it('clicking "View on GitHub" opens the resolved URL via openUrl', async () => {
      // The click must go through `openUrl` (Tauri 2's `target="_blank"`
      // is silently dropped without an explicit capability we don't
      // grant). The file-scoped `openUrlMock` (set up at the top of
      // this file via `vi.hoisted` + `vi.mock('@tauri-apps/plugin-opener')`)
      // captures the call — same pattern as git-issues-tab.test.tsx.
      vi.mocked(invoke).mockImplementation((cmd: string) => {
        if (cmd === 'get_github_url_for_mesh') {
          return Promise.resolve('https://github.com/acme/my-mesh');
        }
        return Promise.resolve({});
      });
      renderMeshItem();
      openContextMenu();

      const item = await screen.findByText('View on GitHub');
      await userEvent.click(item);

      // Pin the full URL — the menu must pass the bare repo URL
      // (NOT `{base}/issues` — that's the Issues probe's destination).
      // The mesh context menu is the repo-home affordance.
      expect(openUrlMock).toHaveBeenCalledWith('https://github.com/acme/my-mesh');
    });

    it('labels the menu with the mesh name via aria-labelledby', () => {
      renderMeshItem();
      openContextMenu();
      const menu = document.querySelector('[role="menu"]')!;
      // The mesh-name span carries `id="mesh-item-name-3"` (mesh.id = 3).
      expect(menu.getAttribute('aria-labelledby')).toBe('mesh-item-name-3');
      // And the labelled element actually exists in the document.
      expect(document.getElementById('mesh-item-name-3')!.textContent).toBe('my-mesh');
    });

    it('autofocuses the first menuitem on open (roving tabindex)', async () => {
      // Issue #837 — the keyboard nav cycle (ArrowDown/Up wrap, Home/End
      // jump), the focus-gate (ignore keystrokes when focus is outside),
      // and the Tab-close behaviour are now covered by the hook tests
      // in `tests/unit/use-aria-menu.test.tsx`. This smoke stays because
      // it's the per-component wiring assertion: a regression that
      // swapped the hook for a no-op would flip this.
      renderMeshItem();
      openContextMenu();
      const items = Array.from(document.querySelectorAll('[role="menuitem"]')) as HTMLButtonElement[];
      // Only the first item is in the Tab order (tabindex=0); the rest are
      // -1 so a single Tab moves focus out of the menu instead of cycling.
      expect(items[0].getAttribute('tabindex')).toBe('0');
      for (let i = 1; i < items.length; i++) {
        expect(items[i].getAttribute('tabindex')).toBe('-1');
      }
      // And the first menuitem is the document.activeElement. The focus
      // is moved via useLayoutEffect inside the hook, so wait one tick.
      await waitFor(() => {
        expect(document.activeElement).toBe(items[0]);
      });
    });

    it('ArrowDown moves focus to the next menuitem (#837 — single smoke)', async () => {
      // Single ArrowDown smoke — proves the hook's `setActiveIndex` is
      // wired into MeshItem's render. The wrap-cycle (and Home/End) are
      // covered by the hook tests.
      renderMeshItem();
      openContextMenu();
      const items = Array.from(document.querySelectorAll('[role="menuitem"]')) as HTMLButtonElement[];

      pressKey('ArrowDown');
      await waitFor(() => expect(document.activeElement).toBe(items[1]));
      expect(items[1].getAttribute('tabindex')).toBe('0');
    });

    it('Escape closes the menu and returns focus to the trigger row (#735, #837)', async () => {
      // Per-component rAF behavior — the hook fires `onClose`, the
      // component's own closure does `requestAnimationFrame(() =>
      // trigger.focus())`. The hook tests cover the Escape-dispatch
      // path; this proves the trigger-focus return is wired.
      renderMeshItem();
      openContextMenu();
      // Sanity: the menu is in the DOM before we press Escape.
      expect(document.querySelector('[role="menu"]')).toBeTruthy();

      pressKey('Escape');

      // The menu unmounts.
      await waitFor(() => {
        expect(document.querySelector('[role="menu"]')).toBeNull();
      });
      // Focus is restored to the row that opened the menu — the header
      // div carries `tabIndex={-1}` precisely for this purpose. The
      // component defers the focus call via requestAnimationFrame, so we
      // poll until the trigger gains focus (within a generous window).
      const trigger = headerEl();
      await waitFor(() => expect(document.activeElement).toBe(trigger));
    });

    it('clicking outside the menu closes it and returns focus to the trigger (#735)', () => {
      // Review catch: the document-level mousedown handler must route
      // through closeContextMenu() — otherwise outside-click closes the
      // menu but leaves focus on document.body instead of returning it
      // to the trigger row.
      renderMeshItem();
      openContextMenu();
      expect(document.querySelector('[role="menu"]')).toBeTruthy();
      const trigger = headerEl();

      // Mouse down somewhere outside both the menu and the trigger row.
      // The menu div uses onMouseDown={e => e.stopPropagation()}, so a
      // mousedown on document.body is what reaches our document listener.
      fireEvent.mouseDown(document.body);

      expect(document.querySelector('[role="menu"]')).toBeNull();
      // Focus returns to the trigger via requestAnimationFrame; flush it.
      // (Promise.resolve + immediate rAF callback both fire in jsdom.)
      return waitFor(() => expect(document.activeElement).toBe(trigger));
    });

    it('Tab closes the menu so the user can move focus to the next tabbable element (#735, #837)', async () => {
      // Issue #837 — the `closeOnTab` default in `useAriaMenu` is `true`,
      // so Tab invokes the hook's `onClose`. This smoke proves the
      // hook's default value is what MeshItem consumes.
      renderMeshItem();
      openContextMenu();
      expect(document.querySelector('[role="menu"]')).toBeTruthy();

      pressKey('Tab');

      await waitFor(() => {
        expect(document.querySelector('[role="menu"]')).toBeNull();
      });
    });

    it('ArrowDown on a keydown target outside the menu does not move menu focus (#735, #837)', () => {
      // Issue #837 — the focus-gate (the hook's `rootRef.current.contains(
      // document.activeElement)` check) is covered by the hook tests in
      // detail. This smoke proves the same focus-gate is wired into
      // MeshItem's render: a sibling trigger with focus must not be
      // hijacked by the document-level listener.
      renderMeshItem();
      openContextMenu();
      const items = Array.from(document.querySelectorAll('[role="menuitem"]')) as HTMLButtonElement[];
      // Force focus elsewhere to simulate the user having left the menu.
      const trigger = headerEl();
      trigger.focus();

      fireEvent.keyDown(document, { key: 'ArrowDown' });

      // The menu stays open and the trigger (not a menuitem) keeps focus.
      expect(document.querySelector('[role="menu"]')).toBeTruthy();
      expect(document.activeElement).toBe(trigger);
      expect(document.activeElement).not.toBe(items[1]);
    });

    it('renders the menu on document.body, not inside the sortable mesh card', () => {
      // Same containing-block trap as NodeItem: MeshItem is a dnd-kit
      // sortable, so `style.transform` (during/after drag) retargets
      // `position:fixed` onto the row. Portaling to `document.body`
      // keeps the click-point `top`/`left` in viewport coordinates.
      renderMeshItem();
      openContextMenu();
      const card = headerEl().closest('div[data-mesh-card]')!;
      const menu = document.querySelector('[role="menu"]') as HTMLElement;
      expect(menu).toBeTruthy();
      expect(card.contains(menu)).toBe(false);
      expect(menu.parentElement).toBe(document.body);
    });

    it('repositions the menu inside the viewport when overflowing the right edge (#735)', async () => {
      renderMeshItem();
      openContextMenu(950, 100);
      const menu = document.querySelector('[role="menu"]') as HTMLElement;
      expect(menu).toBeTruthy();
      stubRect(menu);
      // Re-dispatch contextmenu at the same coords so contextMenu state
      // updates and the useLayoutEffect re-measures (the dep is the
      // `contextMenu` object reference). The rect stub derives its
      // left/top from the inline style, so it tracks each re-render.
      fireEvent.contextMenu(screen.getByText('my-mesh'), { clientX: 950, clientY: 100 });
      // After clamping, with viewport 1024 and MARGIN=4:
      //   overX = 1150 - 1020 = 130, nextX = 950 - 130 = 820.
      await waitFor(() => {
        const x = parseInt(menu.style.left, 10);
        expect(x).toBeGreaterThanOrEqual(4);
        expect(x).toBeLessThanOrEqual(1024 - 200);
      });
    });

    it('repositions the menu inside the viewport when overflowing the bottom edge (#735)', async () => {
      renderMeshItem();
      openContextMenu(100, 700);
      const menu = document.querySelector('[role="menu"]') as HTMLElement;
      expect(menu).toBeTruthy();
      stubRect(menu);
      fireEvent.contextMenu(screen.getByText('my-mesh'), { clientX: 100, clientY: 700 });
      // After clamping, with viewport 768 and MARGIN=4:
      //   overY = 920 - 764 = 156, nextY = 700 - 156 = 544.
      await waitFor(() => {
        const y = parseInt(menu.style.top, 10);
        expect(y).toBeGreaterThanOrEqual(4);
        expect(y).toBeLessThanOrEqual(768 - 220);
      });
    });

    it('does not reposition when the menu already fits within the viewport (#735)', async () => {
      renderMeshItem();
      openContextMenu(50, 50);
      const menu = document.querySelector('[role="menu"]') as HTMLElement;
      expect(menu).toBeTruthy();
      stubRect(menu);
      const before = { top: menu.style.top, left: menu.style.left };
      fireEvent.contextMenu(screen.getByText('my-mesh'), { clientX: 50, clientY: 50 });
      // Let any potential re-render settle before reading the style.
      await new Promise((r) => setTimeout(r, 10));
      expect(menu.style.top).toBe(before.top);
      expect(menu.style.left).toBe(before.left);
    });
  });
});

// Issue #727 — keyboard a11y for the mesh-reorder drag handle. The
// handle is the colour bar, which the row folds together with the
// recolour picker: dnd-kit's `KeyboardSensor` is wired in via
// `useSensors` in Sidebar.tsx, but the row intercepts Enter for the
// picker, so Space is the pickup key. Here we mount a minimal
// sibling-rows harness so the tests can fire Space/ArrowDown/Escape
// against a real sortable list (the single-row `<DndContext>` above
// would otherwise have no `over` target). The harness emits
// `onDragEnd` like the real Sidebar so the reorder contract stays
// honest.
describe('MeshItem — keyboard drag handle a11y (issue #727)', () => {
  afterEach(() => cleanup());

  const MESH_A: Mesh = { ...MESH, id: 1, name: 'mesh-a' };
  const MESH_B: Mesh = { ...MESH, id: 2, name: 'mesh-b' };
  const MESH_C: Mesh = { ...MESH, id: 3, name: 'mesh-c' };

  /** The merged colour-bar handle of the named row — the element the
   *  sensor's `onKeyDown` activator is attached to. */
  function handleFor(name: string) {
    return screen.getByRole('button', { name: new RegExp(`^Change mesh colour for ${name} `) });
  }

  /**
   * Sidebar-equivalent: a DndContext with KeyboardSensor +
   * sortableKeyboardCoordinates + an onDragEnd that calls a passed
   * callback with the new id order. Mirrors Sidebar.tsx's wiring so
   * the tests prove the production contract, not a one-off fixture.
   */
  function SidebarHarness({ onReorder }: { onReorder: (order: number[]) => void }) {
    const sensors = useSensors(
      useSensor(PointerSensor),
      useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
    );
    const handleDragEnd = (event: DragEndEvent) => {
      const { active, over } = event;
      if (!over || active.id === over.id) return;
      const items = [MESH_A.id, MESH_B.id, MESH_C.id];
      const from = items.indexOf(active.id as number);
      const to = items.indexOf(over.id as number);
      if (from === -1 || to === -1) return;
      const next = [...items];
      next.splice(from, 1);
      next.splice(to, 0, active.id as number);
      onReorder(next);
    };
    return (
      <DndContext sensors={sensors} onDragEnd={handleDragEnd}>
        <SortableContext items={[MESH_A.id, MESH_B.id, MESH_C.id]} strategy={verticalListSortingStrategy}>
          <div data-testid="mesh-list">
            {[MESH_A, MESH_B, MESH_C].map(mesh => (
              <MeshItem
                key={mesh.id}
                mesh={mesh}
                isSelected={false}
                isDropdownOpen={false}
                isSpawning={false}
                providerList={PROVIDERS}
                onSelectMesh={vi.fn()}
                onNewNode={vi.fn()}
                onSelectProvider={vi.fn()}
                onOpenFilesProbe={vi.fn()}
                onOpenPropertiesProbe={vi.fn()}
                onOpenWorktreesProbe={vi.fn()}
                onOpenIssuesProbe={vi.fn()}
                onOpenSessionHistoryProbe={vi.fn()}
                nodeClusters={[]}
                onActivateNode={vi.fn()}
                selectMesh={vi.fn()}
                onDeleteNode={vi.fn()}
                getDefaultProvider={vi.fn().mockResolvedValue('anthropic')}
              />
            ))}
          </div>
        </SortableContext>
      </DndContext>
    );
  }

  it('renders the drag handle as a focusable button with aria-roledescription="sortable"', () => {
    render(<SidebarHarness onReorder={() => {}} />);
    const handle = handleFor('mesh-a');
    // tabIndex=0 — required for the KeyboardSensor to find the activator.
    expect(handle.getAttribute('tabindex')).toBe('0');
    // role=button is dnd-kit's default; our explicit override is idempotent.
    expect(handle.getAttribute('role')).toBe('button');
    // aria-roledescription=sortable is the WAI-ARIA description for
    // sortable list items (overrides dnd-kit's "draggable" default).
    expect(handle.getAttribute('aria-roledescription')).toBe('sortable');
  });

  it('focuses the handle when keyboard tab order lands on it', () => {
    render(<SidebarHarness onReorder={() => {}} />);
    const handle = handleFor('mesh-b');
    handle.focus();
    expect(document.activeElement).toBe(handle);
  });

  it('Space starts a drag (aria-pressed flips true) and ArrowDown then drop commits the reorder', async () => {
    // Walk through the full Space → ArrowDown → Space drop sequence.
    // dnd-kit's KeyboardSensor: Space activates the drag (aria-pressed
    // flips true), ArrowDown translates the active item to the next
    // sibling via `sortableKeyboardCoordinates`, and the second Space
    // finalises the move via `onDragEnd`. The reorder is committed
    // through the same `onReorder` callback Sidebar wires to
    // `reorderMeshes` in production.
    //
    // Space, not Enter: the merged bar spends Enter on the colour
    // picker (see the Enter/Space split tests in the suite above), so
    // the sensor's other pickup key is the one left for reordering.
    //
    // Activation fires on the handle's React `onKeyDown` listener.
    // After activation, dnd-kit attaches the document-level listener
    // via `setTimeout(...)` to avoid the same keydown both activating
    // and ending the drag — flush a tick before the ArrowDown/Space.
    //
    // `sortableKeyboardCoordinates` walks siblings by comparing rect
    // tops; jsdom returns zeros, so stub each row's
    // `getBoundingClientRect` with strictly increasing tops so the
    // getter finds the next row.
    const onReorder = vi.fn();
    const { container } = render(<SidebarHarness onReorder={onReorder} />);
    const handles = container.querySelectorAll('[aria-roledescription="sortable"]');
    expect(handles.length).toBe(3);
    handles.forEach((h, i) => {
      // Walk up from the handle to the row that owns `setNodeRef` — the
      // card root carries the `data-mesh-card` seam and the `mb-1.5`
      // class, and the handle is nested two levels down inside it, so
      // `parentElement` isn't enough.
      let row: HTMLElement | null = h as HTMLElement;
      while (row && !row.className.includes('mb-1')) {
        row = row.parentElement;
      }
      expect(row).toBeTruthy();
      const top = i * 50;
      row!.getBoundingClientRect = function (this: HTMLElement) {
        return {
          width: 200,
          height: 50,
          top,
          left: 0,
          right: 200,
          bottom: top + 50,
          x: 0,
          y: top,
          toJSON() { return {}; },
        } as DOMRect;
      };
    });

    const handle = handleFor('mesh-a');
    handle.focus();
    expect(document.activeElement).toBe(handle);

    fireEvent.keyDown(handle, { key: ' ', code: 'Space' });
    expect(handle.getAttribute('aria-pressed')).toBe('true');

    await new Promise(r => setTimeout(r, 0));

    fireEvent.keyDown(document, { key: 'ArrowDown', code: 'ArrowDown' });
    fireEvent.keyDown(document, { key: ' ', code: 'Space' });

    expect(onReorder).toHaveBeenCalledTimes(1);
    expect(onReorder).toHaveBeenCalledWith([2, 1, 3]);
  });

  it('opens the colour picker on Enter instead of picking the row up', () => {
    // The same key the sensor would claim as a pickup is spent on the
    // picker: this is the regression guard for the merge, not a duplicate
    // of the picker test above — here it runs in the full 3-row harness
    // where a pickup WOULD have an `over` target to move to.
    render(<SidebarHarness onReorder={() => {}} />);
    const handle = handleFor('mesh-a');
    handle.focus();

    fireEvent.keyDown(handle, { key: 'Enter', code: 'Enter' });

    expect(screen.getByText('Colour for mesh-a')).toBeTruthy();
    expect(handle.getAttribute('aria-pressed')).not.toBe('true');
  });

  it('Escape cancels the drag and does not commit a reorder', async () => {
    // Escape drops the active item back to its original slot —
    // dnd-kit dispatches `onDragCancel`, which (like Sidebar) does
    // not translate into an `onReorder` call. Cancel is an explicit
    // "no" from the user, not a commit.
    const onReorder = vi.fn();
    render(<SidebarHarness onReorder={onReorder} />);
    const handle = handleFor('mesh-a');
    handle.focus();

    fireEvent.keyDown(handle, { key: ' ', code: 'Space' });
    expect(handle.getAttribute('aria-pressed')).toBe('true');

    await new Promise(r => setTimeout(r, 0));

    fireEvent.keyDown(document, { key: 'Escape', code: 'Escape' });

    expect(onReorder).not.toHaveBeenCalled();
    expect(handle.getAttribute('aria-pressed')).not.toBe('true');
  });
});
