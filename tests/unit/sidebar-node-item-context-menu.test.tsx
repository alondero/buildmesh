/**
 * Issue #776 — right-click context menu on `NodeItem`.
 *
 * Mirrors the MeshItem context-menu contract from issue #735:
 *  - WAI-ARIA `role="menu"` + per-item `role="menuitem"` + `aria-labelledby`
 *    pointing at the row id (so screen readers announce the menu name).
 *  - Roving tabindex so Tab exits the menu cleanly (only the active item
 *    is in the Tab order; the rest are `tabIndex={-1}`).
 *  - ArrowUp / ArrowDown move focus between items with wrap-around.
 *  - Home / End jump to the first / last item.
 *  - Escape closes and returns focus to the row that opened the menu.
 *  - Clicking outside the menu also closes and restores focus.
 *  - Viewport clamping keeps the menu inside the right / bottom edge.
 *
 * The "Regenerate ▸" entry point is the only v1 item (ticket 03 of
 * #774 wires the submenu); it must be DISABLED for statuses where a
 * fresh `spawn_agent` would race the in-flight spawn (`spawning`,
 * `pending`) or be rejected by the backend (`archived`, `suspended`).
 */
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, fireEvent, waitFor, cleanup } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { NodeItem } from '../../src/components/Sidebar/NodeItem';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { getMeshColor } from '../../src/lib/meshColors';

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 42,
    mesh_id: 1,
    name: 'calm-sweet-wolf',
    path: '/repo',
    branch: 'main',
    env: 'wsl',
    provider: 'anthropic',
    status: 'running',
    cli_session_id: null,
    use_worktree: false,
    source_issue: null,
    source_pr: null,
    head_repo_owner: null,
    head_repo_clone_url: null,
    source_pr_pinned_sha: null,
    position: 0,
    created_at: '2026-01-01',
    ...overrides,
  };
}

const meshColor = getMeshColor(1);

function renderNode(node: AgentNode = makeNode()) {
  return render(
    <NodeItem
      node={node}
      meshColor={meshColor}
      isActive={false}
      onSelect={vi.fn()}
      onDelete={vi.fn()}
    />,
  );
}

/**
 * Right-click on the row. Uses `fireEvent.contextMenu` (not a raw
 * `dispatchEvent`) so React's delegated `onContextMenu` receives the
 * MouseEvent with both `clientX`/`clientY` and the trigger's
 * `e.preventDefault()` path — same pattern as sidebar-mesh-item.test.tsx.
 */
function openContextMenu(node = screen.getByText('calm-sweet-wolf'), clientX = 100, clientY = 200) {
  const row = node.closest('[data-session-item]')!;
  fireEvent.contextMenu(row, { clientX, clientY });
}

/**
 * The keydown handler is attached to `document` (not window). jsdom
 * treats them as independent targets, so the existing convention from
 * sidebar-mesh-item.test.tsx is to fire on `document`.
 */
function pressKey(key: string) {
  fireEvent.keyDown(document, { key });
}

describe('NodeItem context menu (issue #776)', () => {
  beforeEach(() => {
    // Reset the store between tests so a previous test's pre-populated
    // `agentNodes` (the spawnAgent mock looks the node up by id) doesn't
    // leak into the next case and silently satisfy the lookup.
    useAgentNodeStore.setState({
      agentNodes: [],
      activeNodeId: null,
      loading: false,
      error: null,
      closingNodeIds: new Set(),
    });
    // Spy on the store's `spawnAgent` so the click test can assert the
    // call without going through the Tauri `invoke` mock — the store
    // method is the public surface NodeItem uses.
    vi.spyOn(useAgentNodeStore.getState(), 'spawnAgent').mockResolvedValue(undefined);
  });

  // RTL doesn't auto-unmount in this vitest setup, so the previous
  // render's DOM (with its own context menu + keydown listener) would
  // still be in the document — `getAllByRole('[role="menuitem"]')`
  // would then return 2× the items and ArrowDown's focus would land
  // on the wrong element. Mirrors sidebar-mesh-item.test.tsx:121-123.
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it('opens a context menu with a Regenerate item on right-click', () => {
    renderNode();
    openContextMenu();
    expect(screen.getByText(/Regenerate/)).toBeTruthy();
  });

  it('does not render the menu until right-clicked', () => {
    renderNode();
    expect(document.querySelector('[role="menu"]')).toBeNull();
  });

  it('marks the menu container with role="menu" and the item with role="menuitem"', () => {
    renderNode();
    openContextMenu();
    expect(document.querySelector('[role="menu"]')).toBeTruthy();
    const items = document.querySelectorAll('[role="menuitem"]');
    expect(items).toHaveLength(1);
    expect(items[0].textContent).toMatch(/Regenerate/);
  });

  it('labels the menu with the node name via aria-labelledby pointing at the row id', () => {
    // The row carries `id={`node-item-name-${node.id}`}` so the
    // menu's `aria-labelledby` points at a real, visible element.
    // This mirrors MeshItem's `mesh-item-name-${mesh.id}` pattern
    // (issue #735) so screen readers can announce the menu name.
    const node = makeNode({ id: 99 });
    renderNode(node);
    openContextMenu(screen.getByText(node.name));
    const menu = document.querySelector('[role="menu"]')!;
    expect(menu.getAttribute('aria-labelledby')).toBe('node-item-name-99');
    expect(document.getElementById('node-item-name-99')!.textContent).toBe(node.name);
  });

  it('autofocuses the first (and only) menuitem on open (roving tabindex)', async () => {
    renderNode();
    openContextMenu();
    const items = Array.from(document.querySelectorAll('[role="menuitem"]')) as HTMLButtonElement[];
    expect(items[0].getAttribute('tabindex')).toBe('0');
    // Focus moves to the menuitem via `setActiveIndex(0)` +
    // `menuItemRefs.current[0]?.focus()` inside the layout effect —
    // wait one tick so the assertion doesn't race the synchronous
    // layout-effect commit.
    await waitFor(() => {
      expect(document.activeElement).toBe(items[0]);
    });
  });

  it('Escape closes the menu and returns focus to the trigger row (#776)', async () => {
    renderNode();
    openContextMenu();
    const row = screen.getByText('calm-sweet-wolf').closest('[data-session-item]')!;
    expect(document.querySelector('[role="menu"]')).toBeTruthy();

    pressKey('Escape');

    await waitFor(() => {
      expect(document.querySelector('[role="menu"]')).toBeNull();
    });
    // The component defers the focus call via requestAnimationFrame, so
    // we poll until the trigger row gains focus (within a generous
    // window — jsdom flushes rAF on a microtask).
    await waitFor(() => {
      expect(document.activeElement).toBe(row);
    });
  });

  it('clicking outside the menu closes it and returns focus to the trigger (#776)', async () => {
    renderNode();
    openContextMenu();
    const row = screen.getByText('calm-sweet-wolf').closest('[data-session-item]')!;
    expect(document.querySelector('[role="menu"]')).toBeTruthy();

    // Mouse down somewhere outside both the menu and the trigger row.
    // The menu div uses onMouseDown={e => e.stopPropagation()}, so a
    // mousedown on document.body is what reaches the document-level
    // close handler. Same pattern as sidebar-mesh-item.test.tsx:573-574.
    fireEvent.mouseDown(document.body);

    await waitFor(() => {
      expect(document.querySelector('[role="menu"]')).toBeNull();
    });
    await waitFor(() => {
      expect(document.activeElement).toBe(row);
    });
  });

  it('clicking the Regenerate item closes the menu and invokes spawnAgent with (nodeId, provider)', async () => {
    // Pre-populate the store so spawnAgent's lookup of `cli_session_id`
    // (production behaviour mirrored from node-item-restart-button.test.tsx:86-87)
    // doesn't find an empty agentNodes array.
    const node = makeNode({ id: 77, status: 'idle' });
    useAgentNodeStore.setState({ agentNodes: [node] });
    renderNode(node);
    openContextMenu();

    await userEvent.click(screen.getByText(/Regenerate/));

    // Menu closes.
    await waitFor(() => {
      expect(document.querySelector('[role="menu"]')).toBeNull();
    });
    // spawnAgent receives the node id + provider so the backend can
    // resume (or restart, for fresh-spawn nodes) the agent.
    expect(useAgentNodeStore.getState().spawnAgent).toHaveBeenCalledTimes(1);
    expect(useAgentNodeStore.getState().spawnAgent).toHaveBeenCalledWith(77, 'anthropic');
  });

  // Status-gating — Regenerate must be DISABLED for the four "race or
  // reject" statuses so a click can't fire a doomed `spawn_agent` IPC.
  // Greyed-out is preferred over hidden for discoverability: the user
  // sees the action exists, with a tooltip explaining why it's blocked.
  describe('Regenerate disabled-state by status', () => {
    it.each([
      ['spawning'],
      ['pending'],
      ['archived'],
      ['suspended'],
    ] as const)('renders Regenerate as disabled when status is "%s"', (status) => {
      renderNode(makeNode({ status }));
      openContextMenu();
      const item = screen.getByText(/Regenerate/).closest('button')!;
      expect(item.hasAttribute('disabled')).toBe(true);
      // Tooltip mentions the raw status name so the user knows *why*
      // the action is unavailable, not just that it's blocked. The
      // raw status is used (rather than `STATUS_CONFIG[status].label`)
      // because `archived` falls back to the `idle` row in
      // STATUS_CONFIG and would misleadingly read "while idle".
      expect(item.getAttribute('title')).toMatch(new RegExp(`while ${status}\\b`, 'i'));
    });

    it.each(['running', 'idle', 'awaiting_input', 'error'] as const)(
      'renders Regenerate as enabled when status is "%s"',
      (status) => {
        renderNode(makeNode({ status }));
        openContextMenu();
        const item = screen.getByText(/Regenerate/).closest('button')!;
        expect(item.hasAttribute('disabled')).toBe(false);
      },
    );

    it('clicking a disabled Regenerate does NOT invoke spawnAgent', async () => {
      // Defense in depth: the HTML `disabled` attribute already blocks
      // clicks in real browsers, but a programmatic `.click()` (or a
      // future refactor that drops the attribute) must still be
      // stopped by the handler's `if (isRegenerateDisabled) return`
      // guard. Pin the guard so the IPC can't fire from a forbidden
      // status.
      const node = makeNode({ id: 88, status: 'spawning' });
      useAgentNodeStore.setState({ agentNodes: [node] });
      renderNode(node);
      openContextMenu();

      const item = screen.getByText(/Regenerate/).closest('button')!;
      // `userEvent.click` honours `disabled` and short-circuits, so
      // fire a raw click to exercise the handler-side guard.
      fireEvent.click(item);

      expect(useAgentNodeStore.getState().spawnAgent).not.toHaveBeenCalled();
    });
  });

  describe('keyboard navigation (#776)', () => {
    it('ArrowDown on a keydown target outside the menu does not move menu focus', () => {
      // The document-level keydown handler must not hijack arrow keys
      // typed when focus is on something other than the menu (e.g.,
      // a sibling node row, the activity bar, or document.body). The
      // menu container ref check short-circuits the handler. Mirrors
      // sidebar-mesh-item.test.tsx:602-622.
      renderNode();
      openContextMenu();
      const items = Array.from(document.querySelectorAll('[role="menuitem"]')) as HTMLButtonElement[];
      const row = screen.getByText('calm-sweet-wolf').closest('[data-session-item]')!;
      // Force focus elsewhere to simulate the user having left the menu.
      (row as HTMLElement).focus();

      fireEvent.keyDown(document, { key: 'ArrowDown' });

      // Menu stays open, focus stays on the trigger, focus didn't move
      // to the menuitem.
      expect(document.querySelector('[role="menu"]')).toBeTruthy();
      expect(document.activeElement).toBe(row);
      expect(document.activeElement).not.toBe(items[0]);
    });

    it('Tab closes the menu so focus can move to the next tabbable element (#776)', async () => {
      // WAI-ARIA menu contract: Tab (and Shift+Tab) move focus out of
      // the menu and close it. The menu is non-modal, so we let the
      // browser perform the focus move and just close behind it.
      // Same pattern as sidebar-mesh-item.test.tsx:582-600.
      renderNode();
      openContextMenu();
      const row = screen.getByText('calm-sweet-wolf').closest('[data-session-item]')!;

      pressKey('Tab');

      await waitFor(() => {
        expect(document.querySelector('[role="menu"]')).toBeNull();
      });
      await waitFor(() => {
        expect(document.activeElement).toBe(row);
      });
    });
  });

  describe('viewport clamping (#776)', () => {
    /**
     * Stub the menu's `getBoundingClientRect` so it tracks the
     * rendered inline `top`/`left`. This keeps the stub honest across
     * re-renders (a clamped value that the stub still reports as
     * overflowing would send the `useLayoutEffect` into a setState
     * loop — see the no-op guard comment in NodeItem.tsx).
     * Mirrors sidebar-mesh-item.test.tsx:353-369.
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

    it('repositions the menu inside the viewport when overflowing the right edge (#776)', async () => {
      renderNode();
      openContextMenu(screen.getByText('calm-sweet-wolf'), 950, 100);
      const menu = document.querySelector('[role="menu"]') as HTMLElement;
      expect(menu).toBeTruthy();
      stubRect(menu);
      // Re-dispatch contextmenu at the same coords so contextMenu
      // state updates and the useLayoutEffect re-measures (the dep is
      // the `contextMenu` object reference).
      fireEvent.contextMenu(screen.getByText('calm-sweet-wolf'), { clientX: 950, clientY: 100 });
      // After clamping, with viewport 1024 and MARGIN=4:
      //   overX = 1150 - 1020 = 130, nextX = 950 - 130 = 820.
      await waitFor(() => {
        const x = parseInt(menu.style.left, 10);
        expect(x).toBeGreaterThanOrEqual(4);
        expect(x).toBeLessThanOrEqual(1024 - 200);
      });
    });

    it('does not reposition when the menu already fits within the viewport (#776)', async () => {
      renderNode();
      openContextMenu(screen.getByText('calm-sweet-wolf'), 50, 50);
      const menu = document.querySelector('[role="menu"]') as HTMLElement;
      expect(menu).toBeTruthy();
      stubRect(menu);
      const before = { top: menu.style.top, left: menu.style.left };
      fireEvent.contextMenu(screen.getByText('calm-sweet-wolf'), { clientX: 50, clientY: 50 });
      // Let any potential re-render settle before reading the style.
      await new Promise((r) => setTimeout(r, 10));
      expect(menu.style.top).toBe(before.top);
      expect(menu.style.left).toBe(before.left);
    });
  });
});