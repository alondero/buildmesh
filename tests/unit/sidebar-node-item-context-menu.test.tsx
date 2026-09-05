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
import type { SpawnOption } from '../../src/lib/groups';
import { colorClassForProvider } from '../../src/lib/groups';

// Regenerate on a Completed autopilot node now reaches the backend
// (`validate_status_eligible` accepts Completed) — but the previous
// silent `console.error` left the user staring at a menu that closed
// without a word on rejection. Pin the new shared-toast plumbing so a
// future regression to `console.error` (or a missing catch) is caught
// here. The same pattern is used in `tests/unit/git-issues-tab.test.tsx`
// for the trigger-label toggle's IPC failure path.
const { addToastMock } = vi.hoisted(() => ({
  addToastMock: vi.fn(),
}));
vi.mock('../../src/stores/toastStore', () => ({
  addToast: addToastMock,
  dismissToast: vi.fn(),
}));

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
    is_pinned: false,
    ...overrides,
  };
}

/**
 * Build a Spawn Option for the picker submenu. Defaults carry just
 * enough for `groupByHarness` to bucket it under its `group_key` and
 * for the row's `data-spawn-*` attributes to be useful to tests.
 */
function makeProvider(
  id: string,
  overrides: Partial<SpawnOption> = {},
): SpawnOption {
  return {
    id,
    label: id,
    icon: null,
    harness_id: id,
    provider_id: id,
    is_proxied: false,
    group_key: id,
    color: colorClassForProvider(id),
    ...overrides,
  };
}

const meshColor = getMeshColor(1);

function renderNode(node: AgentNode = makeNode(), providerList?: SpawnOption[]) {
  return render(
    <NodeItem
      node={node}
      meshColor={meshColor}
      isActive={false}
      providerList={providerList}
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
    useAgentNodeStore.setState({ nodesById: {}, nodeIds: [],activeNodeId: null,
      loading: false,
      error: null,
      closingNodeIds: new Set(),
    });
    // Reset the shared toast spy between tests so a Regenerate IPC
    // failure in one case doesn't bleed into the next.
    addToastMock.mockReset();
    // Spy on the store's `regenerateAgentNode` so the picker-click
    // tests can assert the call without going through the Tauri
    // `invoke` mock — the store method is the public surface
    // NodeItem uses. The mock returns a minimal `AgentNode` so the
    // caller's read after `fetchAgentNodes` doesn't trip on `undefined`.
    // Issue #1384 — read by id from the normalized map.
    vi.spyOn(useAgentNodeStore.getState(), 'regenerateAgentNode').mockImplementation(
      async (nodeId, newProviderId) => {
        const existing = useAgentNodeStore.getState().nodesById[nodeId];
        return { ...(existing ?? makeNode()), provider: newProviderId } as AgentNode;
      },
    );
    // Same pattern for the Pin/Unpin item (#985): the menu calls the
    // shared `toggleNodePinned` store action; the spy lets the test
    // assert the call without a real Tauri round-trip and without
    // mutating the in-memory node list (optimistic patch included).
    vi.spyOn(useAgentNodeStore.getState(), 'toggleNodePinned').mockImplementation(
      async (nodeId) => {
        const existing = useAgentNodeStore.getState().nodesById[nodeId];
        return { ...(existing ?? makeNode()), is_pinned: !(existing?.is_pinned ?? false) } as AgentNode;
      },
    );
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

  it('renders the menu on document.body, not inside the session row', () => {
    // `position:fixed` is retargeted by any ancestor `filter` (the
    // inactive-row `hover:brightness-125`) or dnd-kit `transform` on
    // the parent MeshItem. Auto-focus then scrolls the sidebar list
    // (`overflow-y-auto`) and the menu jumps. Portaling to
    // `document.body` keeps `top`/`left` in viewport coordinates.
    renderNode();
    openContextMenu();
    const row = screen.getByText('calm-sweet-wolf').closest('[data-session-item]')!;
    const menu = document.querySelector('[role="menu"]') as HTMLElement;
    expect(menu).toBeTruthy();
    expect(row.contains(menu)).toBe(false);
    expect(menu.parentElement).toBe(document.body);
  });

  it('auto-focuses the first menuitem with preventScroll so the sidebar does not jump', () => {
    const focusSpy = vi.spyOn(HTMLElement.prototype, 'focus');
    renderNode();
    openContextMenu();
    expect(document.activeElement).toBe(screen.getByText(/Regenerate/).closest('button'));
    expect(focusSpy.mock.calls.some((call) => call[0]?.preventScroll === true)).toBe(true);
    focusSpy.mockRestore();
  });

  it('marks the menu container with role="menu" and two items (Regenerate + Pin) with role="menuitem"', () => {
    // Wayfinder #982 / #985: the menu grew from 1 → 2 items so a user
    // can pin/unpin without entering the canvas, the same store action
    // the header uses, kept keyboard-accessible via the existing
    // roving-tabindex / arrow-wrap contract.
    renderNode();
    openContextMenu();
    expect(document.querySelector('[role="menu"]')).toBeTruthy();
    const items = document.querySelectorAll('[role="menuitem"]');
    expect(items).toHaveLength(2);
    expect(items[0].textContent).toMatch(/Regenerate/);
    expect(items[1].textContent).toMatch(/Pin node|Unpin node/);
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

  it('clicking the Regenerate item opens the provider-picker submenu (#774)', async () => {
    // Issue #774 / ticket 03 — the Regenerate row is no longer a
    // click-to-fire button; it's a submenu trigger that opens the
    // provider picker. The actual `regenerate_agent_node` call moves
    // to "clicking a provider in the picker".
    const node = makeNode({ id: 77, status: 'idle', provider: 'anthropic' });
    renderNode(node, [makeProvider('claude', { is_proxied: false })]);
    openContextMenu();

    // `getByText(/Regenerate/)` matches the text node inside the
    // button; `.closest('button')` walks up to the actual button so
    // userEvent.click fires the React onClick (the SVG inside the
    // button is `pointer-events` neutral in jsdom and would otherwise
    // absorb the synthetic click).
    const trigger = screen.getByText(/Regenerate/).closest('button')!;
    await userEvent.click(trigger);

    // Menu stays open (submenu trigger shouldn't dismiss the menu)…
    await waitFor(() => {
      expect(document.querySelector('[role="menu"]')).toBeTruthy();
    });
    // …and the picker submenu renders with the candidate providers.
    const submenu = screen.getByTestId('regenerate-submenu');
    expect(submenu).toBeTruthy();
    expect(submenu.textContent).toMatch(/claude/i);
    // `regenerateAgentNode` must NOT fire on a trigger click — only
    // a picker-row click invokes the IPC.
    expect(useAgentNodeStore.getState().regenerateAgentNode).not.toHaveBeenCalled();
  });

  // Status-gating — Regenerate must be DISABLED for the four "race or
  // reject" statuses so a click can't fire a doomed `regenerate_agent_node`
  // IPC. Greyed-out is preferred over hidden for discoverability: the user
  // sees the action exists, with a tooltip explaining why it's blocked.
  // Each test passes a populated `providerList` so the "no alternate
  // providers" gate (#774) doesn't mask the status gate under test.
  const alternateProvider = [makeProvider('claude', { group_key: 'claude', harness_id: 'claude' })];
  describe('Regenerate disabled-state by status', () => {
    it.each([
      ['spawning'],
      ['pending'],
      ['archived'],
    ] as const)('renders Regenerate as disabled when status is "%s"', (status) => {
      renderNode(makeNode({ status }), alternateProvider);
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

    it.each(['running', 'idle', 'awaiting_input', 'error', 'suspended'] as const)(
      'renders Regenerate as enabled when status is "%s"',
      (status) => {
        renderNode(makeNode({ status }), alternateProvider);
        openContextMenu();
        const item = screen.getByText(/Regenerate/).closest('button')!;
        expect(item.hasAttribute('disabled')).toBe(false);
      },
    );

    it('clicking a disabled Regenerate does NOT invoke regenerateAgentNode', async () => {
      // Defense in depth: the HTML `disabled` attribute already blocks
      // clicks in real browsers, but a programmatic `.click()` (or a
      // future refactor that drops the attribute) must still be
      // stopped by the handler's `if (isRegenerateDisabled) return`
      // guard. Pin the guard so the IPC can't fire from a forbidden
      // status.
      const node = makeNode({ id: 88, status: 'spawning' });
      renderNode(node, alternateProvider);
      openContextMenu();

      const item = screen.getByText(/Regenerate/).closest('button')!;
      // `userEvent.click` honours `disabled` and short-circuits, so
      // fire a raw click to exercise the handler-side guard.
      fireEvent.click(item);

      expect(useAgentNodeStore.getState().regenerateAgentNode).not.toHaveBeenCalled();
    });
  });

  describe('Pin/Unpin item (wayfinder #982 / #985)', () => {
    it('renders "Pin node" with aria-pressed=false for an unpinned node', () => {
      renderNode();
      openContextMenu();
      const item = screen.getByText('Pin node').closest('button')!;
      expect(item.getAttribute('aria-pressed')).toBe('false');
      expect(item.getAttribute('title')).toBe('Keep this node in the Pinned grid');
    });

    it('renders "Unpin node" with aria-pressed=true for a pinned node', () => {
      renderNode(makeNode({ is_pinned: true }));
      openContextMenu();
      const item = screen.getByText('Unpin node').closest('button')!;
      expect(item.getAttribute('aria-pressed')).toBe('true');
      expect(item.getAttribute('title')).toBe('Remove this node from the Pinned grid');
    });

    it('clicking the Pin item invokes the shared store action with the node id', async () => {
      renderNode();
      openContextMenu();
      const item = screen.getByText('Pin node').closest('button')!;
      // The action is async and the optimistic patch is in the store
      // implementation; the spy on `toggleNodePinned` is what proves
      // the click reached the right API — we don't have to wait for
      // the resolved promise (the spy handles it synchronously after
      // its single microtask tick).
      await userEvent.click(item);
      expect(useAgentNodeStore.getState().toggleNodePinned).toHaveBeenCalledWith(42);
    });

    it('ArrowDown wraps from Regenerate (0) to Pin (1) and back via ArrowUp', () => {
      // Issue #985 — the menu grew from 1 → 2 items; ArrowDown/Up
      // must wrap across the full count, not degenerate to a no-op.
      renderNode(makeNode(), [makeProvider('claude')]);
      openContextMenu();
      // The two top-level menuitems are the only role=menuitems
      // inside the parent menu (not the Regenerate submenu). Pick
      // them directly so we don't read submenu rows.
      const parentMenu = document.querySelector('[role="menu"]')!;
      const topLevel = Array.from(parentMenu.querySelectorAll('[role="menuitem"]')) as HTMLButtonElement[];
      expect(topLevel).toHaveLength(2);
      // First (Regenerate) is auto-focused on open.
      expect(document.activeElement).toBe(topLevel[0]);
      // ArrowDown → Pin.
      pressKey('ArrowDown');
      expect(document.activeElement).toBe(topLevel[1]);
      // ArrowDown → wraps back to Regenerate.
      pressKey('ArrowDown');
      expect(document.activeElement).toBe(topLevel[0]);
      // ArrowUp → wraps forward to Pin.
      pressKey('ArrowUp');
      expect(document.activeElement).toBe(topLevel[1]);
    });

    it('ArrowRight on the Pin item does NOT open the Regenerate submenu', () => {
      // The keyboard-nav guard (wayfinder #982 — ticket migration
      // touch-up) ensures ArrowRight is a submenu command only when
      // focus is on the Regenerate trigger. With two top-level items
      // sharing the menu, ArrowRight on the Pin item used to open
      // the provider picker — wrong.
      renderNode(makeNode(), [makeProvider('claude')]);
      openContextMenu();
      const parentMenu = document.querySelector('[role="menu"]')!;
      const topLevel = Array.from(parentMenu.querySelectorAll('[role="menuitem"]')) as HTMLButtonElement[];
      expect(topLevel).toHaveLength(2);
      // Focus the Pin item.
      topLevel[1].focus();
      pressKey('ArrowRight');
      // Submenu must NOT be open.
      expect(document.querySelector('[data-testid="regenerate-submenu"]')).toBeNull();
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

  // Issue #774 / ticket 03 — Regenerate's provider-picker submenu.
  // Mirrors `GroupedProviderMenu`'s shape (harness headers + proxied
  // children) so the picker is visually consistent with the rest of
  // the spawn surface.
  describe('Regenerate submenu (issue #774 / ticket 03)', () => {
    /** Open the parent context menu and click Regenerate so the
     *  submenu's hover/click toggle fires. Returns the submenu node
     *  so individual tests can query rows inside it. */
    async function openSubmenu(node: AgentNode = makeNode(), providers?: SpawnOption[]) {
      renderNode(node, providers);
      openContextMenu();
      // Same `.closest('button')` walk as the v1 "Regenerate" test —
      // userEvent.click on the bare text node would otherwise hit the
      // SVG (which jsdom marks non-interactive) and miss the React
      // onClick handler on the button.
      const trigger = screen.getByText(/Regenerate/).closest('button')!;
      await userEvent.click(trigger);
      await waitFor(() => {
        expect(screen.getByTestId('regenerate-submenu')).toBeTruthy();
      });
      return screen.getByTestId('regenerate-submenu');
    }

    it('renders the current provider pinned on top plus alternates grouped by harness (issue #1502)', async () => {
      const node = makeNode({ provider: 'anthropic' });
      await openSubmenu(node, [
        // current — pinned on top as `Current (<label>)` for in-place kick-start
        makeProvider('anthropic', { label: 'Anthropic', group_key: 'anthropic', harness_id: 'anthropic' }),
        // a sibling harness with native + proxied children
        makeProvider('claude', { label: 'Claude Code', group_key: 'claude', harness_id: 'claude' }),
        makeProvider('claude-minimax', {
          id: 'claude-minimax', label: 'Claude Code · Minimax',
          group_key: 'claude', harness_id: 'claude',
          is_proxied: true, provider_id: 'minimax',
        }),
        // another harness
        makeProvider('kimi', { label: 'Kimi', group_key: 'kimi', harness_id: 'kimi' }),
      ]);

      const submenu = screen.getByTestId('regenerate-submenu');
      // Current section pinned on top (in-place kick-start).
      const currentSection = submenu.querySelector('[data-regenerate-section="current"]')!;
      expect(currentSection).toBeTruthy();
      expect(currentSection.querySelector('[data-spawn-id="anthropic"]')).toBeTruthy();
      expect(currentSection.textContent).toMatch(/Current \(Anthropic\)/);
      expect(currentSection.querySelector('[data-is-current="true"]')).toBeTruthy();
      // Two harness groups for alternates (anthropic lives in the current section, not grouped).
      expect(submenu.querySelectorAll('[data-spawn-group]')).toHaveLength(2);
      // claude group carries its harness header + proxied child.
      const claudeGroup = submenu.querySelector('[data-spawn-group="claude"]')!;
      expect(claudeGroup.querySelectorAll('[data-spawn-id]')).toHaveLength(2);
      // Total picker rows = 4 (current + claude + claude-minimax + kimi).
      expect(submenu.querySelectorAll('[role="menuitem"]')).toHaveLength(4);
    });

    it('clicking a picker row invokes regenerateAgentNode(nodeId, providerId) and closes the menu', async () => {
      const node = makeNode({ id: 91, provider: 'anthropic', status: 'idle' });
      await openSubmenu(node, [
        makeProvider('anthropic', { group_key: 'anthropic', harness_id: 'anthropic' }),
        makeProvider('claude', { label: 'Claude Code', group_key: 'claude', harness_id: 'claude' }),
      ]);

      await userEvent.click(screen.getByText(/Claude Code/));

      // IPC fires with the chosen provider id (NOT the current one).
      expect(useAgentNodeStore.getState().regenerateAgentNode).toHaveBeenCalledTimes(1);
      expect(useAgentNodeStore.getState().regenerateAgentNode).toHaveBeenCalledWith(91, 'claude');
      // The whole menu (parent + submenu) closes.
      await waitFor(() => {
        expect(document.querySelector('[role="menu"]')).toBeNull();
      });
    });

    it('surfaces a Regenerate toast when the IPC rejects', async () => {
      // Pinned for the "Completed autopilot node" case (and any future
      // backend rejection): the previous `console.error` catch left the
      // user staring at a closed menu with no feedback. The store spy
      // is reset by `beforeEach`'s `mockImplementation`; we override it
      // for this single test with `mockImplementationOnce` so the rest
      // of the suite keeps the happy-path mock.
      const rejection = new Error('regenerate unavailable: node is in archived state (must be idle, awaiting_input, error, running, suspended, or completed)');
      vi.mocked(useAgentNodeStore.getState().regenerateAgentNode)
        .mockImplementationOnce(async () => {
          throw rejection;
        });
      const node = makeNode({ id: 411, provider: 'anthropic', status: 'idle' });
      await openSubmenu(node, [
        makeProvider('anthropic', { group_key: 'anthropic', harness_id: 'anthropic' }),
        makeProvider('claude', { label: 'Claude Code', group_key: 'claude', harness_id: 'claude' }),
      ]);
      // The picker is now open; click the Claude row (the only
      // alternate provider). `pickProvider`'s catch branch fires the
      // `addToast('Regenerate', ...)` call — the assertion below is
      // the regression pin.
      const claudeItem = screen.getByRole('menuitem', { name: /claude/i });
      await userEvent.click(claudeItem);
      await waitFor(() => {
        expect(addToastMock).toHaveBeenCalledTimes(1);
      });
      expect(addToastMock).toHaveBeenCalledWith(
        'Regenerate failed',
        expect.stringContaining('regenerate unavailable'),
        'error',
      );
      // IPC was called exactly once before the toast — pins the
      // catch-side firing, not a re-throw-and-catch dance.
      expect(useAgentNodeStore.getState().regenerateAgentNode).toHaveBeenCalledTimes(1);
      expect(useAgentNodeStore.getState().regenerateAgentNode).toHaveBeenCalledWith(411, 'claude');
    });

    it('renders an empty-state message when no providers are available at all (issue #1502)', async () => {
      const node = makeNode({ provider: 'anthropic' });
      await openSubmenu(node, []);
      const empty = screen.getByTestId('regenerate-submenu-empty');
      expect(empty).toBeTruthy();
      expect(empty.textContent).toMatch(/no providers available/i);
    });

    it('renders the current provider alone when it is the only option (in-place kick-start, issue #1502)', async () => {
      const node = makeNode({ provider: 'anthropic' });
      await openSubmenu(node, [
        makeProvider('anthropic', { label: 'Anthropic', group_key: 'anthropic', harness_id: 'anthropic' }),
      ]);
      // No empty state — the current row IS the picker.
      expect(screen.queryByTestId('regenerate-submenu-empty')).toBeNull();
      const current = screen.getByTestId('regenerate-submenu-current');
      expect(current).toBeTruthy();
      expect(current.textContent).toMatch(/Current \(Anthropic\)/);
      expect(screen.getByTestId('regenerate-submenu').querySelectorAll('[role="menuitem"]')).toHaveLength(1);
    });

    it('disables the trigger when providerList is empty', () => {
      // The picker would have nothing to offer, so the trigger is
      // disabled and a click does nothing.
      renderNode(makeNode(), []);
      openContextMenu();
      const trigger = screen.getByText(/Regenerate/).closest('button')!;
      expect(trigger.hasAttribute('disabled')).toBe(true);
      expect(trigger.getAttribute('title')).toMatch(/no providers/i);
    });

    it('keeps the picker closed until the trigger is hovered or clicked', () => {
      // The submenu's `aria-expanded` mirrors the open state, so
      // screen readers announce the parent as collapsed until the
      // user interacts. Click here so the picker flips open.
      renderNode(makeNode(), [
        makeProvider('claude', { group_key: 'claude', harness_id: 'claude' }),
      ]);
      openContextMenu();
      const trigger = screen.getByText(/Regenerate/).closest('button')!;
      expect(trigger.getAttribute('aria-expanded')).toBe('false');
      expect(trigger.getAttribute('aria-haspopup')).toBe('menu');
      // Picker not yet in the DOM.
      expect(document.querySelector('[data-testid="regenerate-submenu"]')).toBeNull();
    });

    it('does not invoke regenerateAgentNode when the trigger is disabled', async () => {
      // Issue #774 — disabled-for-status gating propagates to the
      // submenu. The trigger is greyed-out, the picker never opens,
      // and a programmatic .click() can't bypass the guard.
      const node = makeNode({ status: 'archived' });
      renderNode(node, [
        makeProvider('claude', { group_key: 'claude', harness_id: 'claude' }),
      ]);
      openContextMenu();

      const trigger = screen.getByText(/Regenerate/).closest('button')!;
      fireEvent.click(trigger);

      expect(screen.queryByTestId('regenerate-submenu')).toBeNull();
      expect(useAgentNodeStore.getState().regenerateAgentNode).not.toHaveBeenCalled();
    });

    it('keeps the trigger enabled when the mesh offers only the current provider (in-place kick-start, issue #1502)', () => {
      // Pre-#1502 the picker excluded the current provider, so a mesh
      // whose only option was the current one disabled the trigger.
      // Post-#1502 the current provider alone enables in-place
      // regeneration (kick-start a wonky harness).
      const node = makeNode({ provider: 'anthropic' });
      renderNode(node, [
        makeProvider('anthropic', { group_key: 'anthropic', harness_id: 'anthropic' }),
      ]);
      openContextMenu();
      const trigger = screen.getByText(/Regenerate/).closest('button')!;
      expect(trigger.hasAttribute('disabled')).toBe(false);
    });

    describe('keyboard navigation (#774, #1502)', () => {
      it('ArrowRight on the trigger opens the picker and focuses the current provider first', async () => {
        renderNode(
          makeNode({ provider: 'anthropic' }),
          [
            makeProvider('anthropic', { label: 'Anthropic', group_key: 'anthropic', harness_id: 'anthropic' }),
            makeProvider('claude', { label: 'Claude Code', group_key: 'claude', harness_id: 'claude' }),
            makeProvider('kimi', { label: 'Kimi', group_key: 'kimi', harness_id: 'kimi' }),
          ],
        );
        openContextMenu();
        // Trigger has focus on open (the existing v1 roving-tabindex
        // behavior). Fire ArrowRight on the document (the keydown
        // handler is attached there).
        fireEvent.keyDown(document, { key: 'ArrowRight' });

        await waitFor(() => {
          expect(screen.getByTestId('regenerate-submenu')).toBeTruthy();
        });
        // First row is the in-place current provider (issue #1502 pins
        // it on top); alternates follow in input order.
        await waitFor(() => {
          const firstItem = screen.getByTestId('regenerate-submenu-current');
          expect(document.activeElement).toBe(firstItem);
        });
      });

      it('ArrowLeft inside the picker closes it and returns focus to the trigger', async () => {
        renderNode(
          makeNode({ provider: 'anthropic' }),
          [
            makeProvider('anthropic', { group_key: 'anthropic', harness_id: 'anthropic' }),
            makeProvider('claude', { group_key: 'claude', harness_id: 'claude' }),
          ],
        );
        openContextMenu();
        fireEvent.keyDown(document, { key: 'ArrowRight' });
        await waitFor(() => {
          expect(screen.getByTestId('regenerate-submenu')).toBeTruthy();
        });

        fireEvent.keyDown(document, { key: 'ArrowLeft' });

        await waitFor(() => {
          expect(screen.queryByTestId('regenerate-submenu')).toBeNull();
        });
        const trigger = screen.getByText(/Regenerate/).closest('button')!;
        await waitFor(() => {
          expect(document.activeElement).toBe(trigger);
        });
      });

      it('ArrowDown inside the picker advances through current + alternates with wrap (issue #1502)', async () => {
        renderNode(
          makeNode({ provider: 'anthropic' }),
          [
            makeProvider('anthropic', { label: 'Anthropic', group_key: 'anthropic', harness_id: 'anthropic' }),
            makeProvider('claude', { label: 'Claude Code', group_key: 'claude', harness_id: 'claude' }),
            makeProvider('kimi', { label: 'Kimi', group_key: 'kimi', harness_id: 'kimi' }),
          ],
        );
        openContextMenu();
        fireEvent.keyDown(document, { key: 'ArrowRight' });
        await waitFor(() => {
          expect(screen.getByTestId('regenerate-submenu')).toBeTruthy();
        });

        // current → claude → kimi → wrap back to current
        const items = () => Array.from(
          screen.getByTestId('regenerate-submenu').querySelectorAll<HTMLButtonElement>('[role="menuitem"]'),
        );
        expect(items()).toHaveLength(3);
        expect(document.activeElement).toBe(items()[0]);
        expect(items()[0].getAttribute('data-testid')).toBe('regenerate-submenu-current');
        fireEvent.keyDown(document, { key: 'ArrowDown' });
        expect(document.activeElement).toBe(items()[1]);
        fireEvent.keyDown(document, { key: 'ArrowDown' });
        expect(document.activeElement).toBe(items()[2]);
        fireEvent.keyDown(document, { key: 'ArrowDown' });
        // Wrap-around: kimi → current.
        expect(document.activeElement).toBe(items()[0]);
      });
    });

    // Issue #1293 — Chromium fires `mouseenter` synchronously on the
    // Regenerate wrapper when the menu mounts under an existing cursor
    // (right-click places the cursor on the first item, the menu pops
    // up at the click point, and the wrapper is already under the
    // cursor). The picker used to open as a side effect, looking like
    // the menu "jumped". The wrapper's `onPointerOver` arms a ref;
    // only an armed `mouseenter` opens. Click + `ArrowRight` stay
    // immediate because they don't go through `mouseenter`.
    describe('hover arm — pointerover required before mouseenter opens (#1293)', () => {
      /** Find the wrapper div that holds the Regenerate trigger and
       *  its picker submenu. It's the parent of the Regenerate button
       *  that also has `role="presentation"` and `className="relative"`. */
      function getRegenWrapper(): HTMLElement {
        const trigger = screen.getByText(/Regenerate/).closest('button')!;
        const wrapper = trigger.parentElement!;
        // Sanity pin: the wrapper is a `<div role="presentation">`
        // with a class containing `relative`. If a future refactor
        // moves the handlers to a different element, this test will
        // surface it (and we'll re-target the queries below).
        expect(wrapper.getAttribute('role')).toBe('presentation');
        return wrapper as HTMLElement;
      }

      function mountRegenMenu() {
        renderNode(makeNode({ status: 'idle' }), [
          makeProvider('anthropic', { group_key: 'anthropic', harness_id: 'anthropic' }),
          makeProvider('claude', { group_key: 'claude', harness_id: 'claude' }),
        ]);
        openContextMenu();
      }

      it('does NOT open the picker on a mount-time mouseenter (Chromium quirk)', () => {
        // Simulate Chromium firing `mouseenter` on the wrapper as the
        // menu mounts. No prior `pointermove` → the wrapper is
        // unarmed → the picker stays closed. This is the exact path
        // PR #1290's "occasionally" reports described.
        mountRegenMenu();
        const wrapper = getRegenWrapper();
        fireEvent.mouseEnter(wrapper);

        expect(document.querySelector('[data-testid="regenerate-submenu"]')).toBeNull();
      });

      it('opens the picker after pointerover then mouseenter (real hover)', () => {
        // The same wrapper, but with a real pointerover before the
        // mouseenter. The arm flips → mouseenter opens. Mirrors the
        // user dragging the cursor from elsewhere onto the row after
        // the menu has already mounted. `pointerover` is the FIRST
        // event in the per-spec enter-the-element sequence (before
        // `mouseenter`), and matches what user-event v14 dispatches
        // in tests — keeping this test faithful to production order.
        mountRegenMenu();
        const wrapper = getRegenWrapper();
        fireEvent.pointerOver(wrapper);
        fireEvent.mouseEnter(wrapper);

        expect(screen.getByTestId('regenerate-submenu')).toBeTruthy();
      });

      it('clicking the trigger opens the picker immediately (no arm required)', () => {
        // Click bypasses `mouseenter` entirely, so the arm gate must
        // NOT block it. Pin the immediate path so a future "always
        // arm" regression doesn't break click/keyboard users.
        mountRegenMenu();
        const trigger = screen.getByText(/Regenerate/).closest('button')!;
        fireEvent.click(trigger);

        expect(screen.getByTestId('regenerate-submenu')).toBeTruthy();
      });

      it('ArrowRight opens the picker immediately (no arm required)', () => {
        // Same as click: `openSubmenuViaKeyboard` doesn't go through
        // `mouseenter`. The focus-on-open effect fires before any
        // pointer event would.
        mountRegenMenu();
        fireEvent.keyDown(document, { key: 'ArrowRight' });

        expect(screen.getByTestId('regenerate-submenu')).toBeTruthy();
      });

      it('disarms on mouseleave so a re-entry still requires pointerover', () => {
        // After a real hover opens the picker, mouseleave closes it
        // AND resets the arm. Re-entering without a fresh pointerover
        // should NOT reopen it (the next user gesture still has to
        // be a real movement).
        mountRegenMenu();
        const wrapper = getRegenWrapper();
        // Real hover → open.
        fireEvent.pointerOver(wrapper);
        fireEvent.mouseEnter(wrapper);
        expect(screen.getByTestId('regenerate-submenu')).toBeTruthy();
        // Leave → close + disarm.
        fireEvent.mouseLeave(wrapper);
        expect(document.querySelector('[data-testid="regenerate-submenu"]')).toBeNull();
        // Re-enter without a fresh pointerover → still closed.
        fireEvent.mouseEnter(wrapper);
        expect(document.querySelector('[data-testid="regenerate-submenu"]')).toBeNull();
        // Now a real move → opens.
        fireEvent.pointerOver(wrapper);
        fireEvent.mouseEnter(wrapper);
        expect(screen.getByTestId('regenerate-submenu')).toBeTruthy();
      });
    });
  });

  describe('Start Fresh context menu item (issue #1306)', () => {
    it('renders Start Fresh in context menu for an error node with a cli_session_id (3 items total)', () => {
      const node = makeNode({
        status: 'error',
        cli_session_id: 'stale-uuid-1234',
      });
      renderNode(node);
      openContextMenu();

      const items = document.querySelectorAll('[role="menuitem"]');
      expect(items).toHaveLength(3);
      expect(items[0].textContent).toMatch(/Regenerate/);
      expect(items[1].textContent).toMatch(/Start Fresh/);
      expect(items[2].textContent).toMatch(/Pin node|Unpin node/);
    });

    it('does NOT render Start Fresh in context menu when error node has no cli_session_id (2 items)', () => {
      const node = makeNode({
        status: 'error',
        cli_session_id: null,
      });
      renderNode(node);
      openContextMenu();

      const items = document.querySelectorAll('[role="menuitem"]');
      expect(items).toHaveLength(2);
      expect(items[0].textContent).toMatch(/Regenerate/);
      expect(items[1].textContent).toMatch(/Pin node|Unpin node/);
    });

    it('clicking Start Fresh in context menu calls restartFreshAgent and closes menu', async () => {
      const restartFreshSpy = vi.fn().mockResolvedValue(makeNode());
      useAgentNodeStore.setState({
        restartFreshAgent: restartFreshSpy,
      });

      const node = makeNode({
        id: 88,
        status: 'error',
        cli_session_id: 'stale-uuid-1234',
      });
      renderNode(node);
      openContextMenu();

      const startFreshBtn = screen.getByTestId('context-start-fresh');
      await userEvent.click(startFreshBtn);

      expect(restartFreshSpy).toHaveBeenCalledWith(88);
      await waitFor(() => {
        expect(document.querySelector('[role="menu"]')).toBeNull();
      });
    });

    it('ArrowDown wraps through all 3 items (Regenerate -> Start Fresh -> Pin -> Regenerate)', () => {
      const node = makeNode({
        status: 'error',
        cli_session_id: 'stale-uuid-1234',
      });
      renderNode(node);
      openContextMenu();

      const items = () => Array.from(document.querySelectorAll('[role="menuitem"]')) as HTMLButtonElement[];
      expect(document.activeElement).toBe(items()[0]);

      fireEvent.keyDown(document, { key: 'ArrowDown' });
      expect(document.activeElement).toBe(items()[1]);

      fireEvent.keyDown(document, { key: 'ArrowDown' });
      expect(document.activeElement).toBe(items()[2]);

      fireEvent.keyDown(document, { key: 'ArrowDown' });
      expect(document.activeElement).toBe(items()[0]);
    });

    it('ArrowUp wraps backwards through all 3 items (Regenerate -> Pin -> Start Fresh -> Regenerate)', () => {
      const node = makeNode({
        status: 'error',
        cli_session_id: 'stale-uuid-1234',
      });
      renderNode(node);
      openContextMenu();

      const items = () => Array.from(document.querySelectorAll('[role="menuitem"]')) as HTMLButtonElement[];
      expect(document.activeElement).toBe(items()[0]);

      fireEvent.keyDown(document, { key: 'ArrowUp' });
      expect(document.activeElement).toBe(items()[2]);

      fireEvent.keyDown(document, { key: 'ArrowUp' });
      expect(document.activeElement).toBe(items()[1]);

      fireEvent.keyDown(document, { key: 'ArrowUp' });
      expect(document.activeElement).toBe(items()[0]);
    });

    it('Home and End jump to first and last items', () => {
      const node = makeNode({
        status: 'error',
        cli_session_id: 'stale-uuid-1234',
      });
      renderNode(node);
      openContextMenu();

      const items = () => Array.from(document.querySelectorAll('[role="menuitem"]')) as HTMLButtonElement[];

      fireEvent.keyDown(document, { key: 'End' });
      expect(document.activeElement).toBe(items()[2]);

      fireEvent.keyDown(document, { key: 'Home' });
      expect(document.activeElement).toBe(items()[0]);
    });

    it('surfaces toast on Start Fresh failure', async () => {
      const restartFreshSpy = vi.fn().mockRejectedValue(new Error('Spawn crashed'));
      useAgentNodeStore.setState({
        restartFreshAgent: restartFreshSpy,
      });

      const node = makeNode({
        id: 99,
        status: 'error',
        cli_session_id: 'stale-uuid-1234',
      });
      renderNode(node);
      openContextMenu();

      const startFreshBtn = screen.getByTestId('context-start-fresh');
      await userEvent.click(startFreshBtn);

      expect(restartFreshSpy).toHaveBeenCalledWith(99);
      expect(addToastMock).toHaveBeenCalledWith(
        'Start Fresh failed',
        expect.stringContaining('Spawn crashed'),
        'error',
      );
    });
  });
});
