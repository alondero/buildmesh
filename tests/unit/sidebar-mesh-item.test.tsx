import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { DndContext } from '@dnd-kit/core';
import { SortableContext } from '@dnd-kit/sortable';
import { MeshItem } from '../../src/components/Sidebar/MeshItem';
import type { Mesh } from '../../src/stores/meshStore';
import type { AgentNode } from '../../src/stores/agentNodeStore';
import type { SpawnOption } from '../../src/lib/groups';

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
    // Issue #378 — the right-click "GitHub Issues" / "Archive" entries route
    // through the Probe Panel via the new probe-tab handlers. The legacy
    // `onOpenGitHubIssues` / `onOpenSessionBrowser` props are gone; the
    // modal components stay on disk but no consumer wires them up.
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
      const header = screen.getByText('my-mesh').closest('div[class*="border-l-3"]')!;
      fireEvent.contextMenu(header, { clientX, clientY });
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
      renderMeshItem();
      openContextMenu();
      const menu = document.querySelector('[role="menu"]')!;
      expect(menu).toBeTruthy();
      // Five menuitems, in render order: Properties, File Explorer,
      // Sync Latest, Archive, GitHub Issues.
      const items = document.querySelectorAll('[role="menuitem"]');
      expect(items).toHaveLength(5);
      expect(items[0].textContent).toMatch(/Properties/);
      expect(items[1].textContent).toMatch(/File Explorer/);
      expect(items[2].textContent).toMatch(/Sync Latest/);
      expect(items[3].textContent).toMatch(/Archive/);
      expect(items[4].textContent).toMatch(/GitHub Issues/);
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
      // is moved via setTimeout(0) inside the component, so wait one tick.
      await waitFor(() => {
        expect(document.activeElement).toBe(items[0]);
      });
    });

    it('ArrowDown moves focus to the next menuitem with wrap-around (#735)', async () => {
      renderMeshItem();
      openContextMenu();
      const items = Array.from(document.querySelectorAll('[role="menuitem"]')) as HTMLButtonElement[];

      pressKey('ArrowDown');
      await waitFor(() => expect(document.activeElement).toBe(items[1]));
      expect(items[1].getAttribute('tabindex')).toBe('0');

      pressKey('ArrowDown');
      await waitFor(() => expect(document.activeElement).toBe(items[2]));
      expect(items[2].getAttribute('tabindex')).toBe('0');

      // Last → wrap to first.
      for (let i = 0; i < 3; i++) pressKey('ArrowDown');
      await waitFor(() => expect(document.activeElement).toBe(items[0]));
      expect(items[0].getAttribute('tabindex')).toBe('0');
    });

    it('ArrowUp moves focus to the previous menuitem with wrap-around (#735)', async () => {
      renderMeshItem();
      openContextMenu();
      const items = Array.from(document.querySelectorAll('[role="menuitem"]')) as HTMLButtonElement[];
      // First → wrap to last.
      pressKey('ArrowUp');
      await waitFor(() => expect(document.activeElement).toBe(items[items.length - 1]));
      expect(items[items.length - 1].getAttribute('tabindex')).toBe('0');
    });

    it('Home jumps to the first menuitem and End jumps to the last (#735)', async () => {
      renderMeshItem();
      openContextMenu();
      const items = Array.from(document.querySelectorAll('[role="menuitem"]')) as HTMLButtonElement[];
      // Move away from index 0 first (two ArrowDowns → index 2 / "Sync Latest").
      pressKey('ArrowDown');
      pressKey('ArrowDown');
      await waitFor(() => expect(document.activeElement).toBe(items[2]));

      pressKey('Home');
      await waitFor(() => expect(document.activeElement).toBe(items[0]));
      expect(items[0].getAttribute('tabindex')).toBe('0');

      pressKey('End');
      await waitFor(() => expect(document.activeElement).toBe(items[items.length - 1]));
      expect(items[items.length - 1].getAttribute('tabindex')).toBe('0');
    });

    it('Escape closes the menu and returns focus to the trigger row (#735)', async () => {
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
      const trigger = screen.getByText('my-mesh').closest('div[class*="border-l-3"]')!;
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
      const trigger = screen.getByText('my-mesh').closest('div[class*="border-l-3"]')!;

      // Mouse down somewhere outside both the menu and the trigger row.
      // The menu div uses onMouseDown={e => e.stopPropagation()}, so a
      // mousedown on document.body is what reaches our document listener.
      fireEvent.mouseDown(document.body);

      expect(document.querySelector('[role="menu"]')).toBeNull();
      // Focus returns to the trigger via requestAnimationFrame; flush it.
      // (Promise.resolve + immediate rAF callback both fire in jsdom.)
      return waitFor(() => expect(document.activeElement).toBe(trigger));
    });

    it('Tab closes the menu so the user can move focus to the next tabbable element (#735)', async () => {
      // WAI-ARIA menu contract: Tab (and Shift+Tab) move focus out of
      // the menu and close it. The menu is non-modal, so we let the
      // browser perform the focus move and just close behind it.
      renderMeshItem();
      openContextMenu();
      const items = Array.from(document.querySelectorAll('[role="menuitem"]')) as HTMLButtonElement[];
      // Move to a non-zero item so we can confirm close restores focus to trigger.
      pressKey('ArrowDown');
      await waitFor(() => expect(document.activeElement).toBe(items[1]));

      pressKey('Tab');

      await waitFor(() => {
        expect(document.querySelector('[role="menu"]')).toBeNull();
      });
      const trigger = screen.getByText('my-mesh').closest('div[class*="border-l-3"]')!;
      await waitFor(() => expect(document.activeElement).toBe(trigger));
    });

    it('ArrowDown on a keydown target outside the menu does not move menu focus (#735)', () => {
      // The document-level keydown handler must not hijack arrow keys
      // typed when focus is on something other than the menu (e.g.,
      // a sibling mesh row, the activity bar, or document.body). The
      // menu container ref check short-circuits the handler.
      renderMeshItem();
      openContextMenu();
      const items = Array.from(document.querySelectorAll('[role="menuitem"]')) as HTMLButtonElement[];
      // Initially, the first menuitem should be focused (autofocus on open).
      // Force focus elsewhere to simulate the user having left the menu.
      const trigger = screen.getByText('my-mesh').closest('div[class*="border-l-3"]')!;
      (trigger as HTMLElement).focus();

      fireEvent.keyDown(document, { key: 'ArrowDown' });

      // The menu stays open and the trigger (not a menuitem) keeps focus.
      expect(document.querySelector('[role="menu"]')).toBeTruthy();
      expect(document.activeElement).toBe(trigger);
      // And the menuitems themselves didn't shift focus to items[1].
      expect(document.activeElement).not.toBe(items[1]);
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
