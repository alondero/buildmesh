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
    useAgentNodeStore.setState({
      agentNodes: [],
      activeNodeId: null,
      loading: false,
      error: null,
      closingNodeIds: new Set(),
    });
    // Spy on the store's `regenerateAgentNode` so the picker-click
    // tests can assert the call without going through the Tauri
    // `invoke` mock — the store method is the public surface
    // NodeItem uses. The mock returns a minimal `AgentNode` so the
    // caller's `agentNodes.find` after `fetchAgentNodes` doesn't
    // trip on `undefined`.
    vi.spyOn(useAgentNodeStore.getState(), 'regenerateAgentNode').mockImplementation(
      async (nodeId, newProviderId) => {
        const existing = useAgentNodeStore.getState().agentNodes.find(n => n.id === nodeId);
        return { ...(existing ?? makeNode()), provider: newProviderId } as AgentNode;
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
      ['suspended'],
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

    it.each(['running', 'idle', 'awaiting_input', 'error'] as const)(
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

    it('renders one row per non-current provider, grouped by harness', async () => {
      const node = makeNode({ provider: 'anthropic' });
      await openSubmenu(node, [
        // current — must be excluded from the picker
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
      // Three harness groups (anthropic excluded).
      expect(submenu.querySelectorAll('[data-spawn-group]')).toHaveLength(2);
      // claude group carries its harness header + proxied child.
      const claudeGroup = submenu.querySelector('[data-spawn-group="claude"]')!;
      expect(claudeGroup.querySelectorAll('[data-spawn-id]')).toHaveLength(2);
      // Total picker rows = 3 (claude + claude-minimax + kimi).
      expect(submenu.querySelectorAll('[role="menuitem"]')).toHaveLength(3);
      // Current provider MUST NOT appear in the picker.
      expect(submenu.querySelector('[data-spawn-id="anthropic"]')).toBeNull();
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

    it('renders an empty-state message when no other providers are available', async () => {
      const node = makeNode({ provider: 'anthropic' });
      await openSubmenu(node, [
        // Only the current provider — nothing to offer.
        makeProvider('anthropic', { group_key: 'anthropic', harness_id: 'anthropic' }),
      ]);
      const empty = screen.getByTestId('regenerate-submenu-empty');
      expect(empty).toBeTruthy();
      expect(empty.textContent).toMatch(/no other providers/i);
      expect(screen.queryByRole('menuitem', { name: /anthropic/i })).toBeNull();
    });

    it('disables the trigger when providerList is empty', () => {
      // The picker would have nothing to offer (every provider IS the
      // current one), so the trigger is disabled and a click does
      // nothing — covered by the "no alternate providers" test above,
      // but pinned here too for the empty-list edge case.
      renderNode(makeNode(), []);
      openContextMenu();
      const trigger = screen.getByText(/Regenerate/).closest('button')!;
      expect(trigger.hasAttribute('disabled')).toBe(true);
      expect(trigger.getAttribute('title')).toMatch(/no other providers/i);
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

    it('greys out the trigger when the mesh has no alternate providers (#774)', () => {
      // The picker excludes the node's current provider, so a mesh
      // whose only option is the current one would render an empty
      // submenu — the trigger must be disabled instead so the user
      // sees the affordance exists with a tooltip explaining why.
      const node = makeNode({ provider: 'anthropic' });
      renderNode(node, [
        makeProvider('anthropic', { group_key: 'anthropic', harness_id: 'anthropic' }),
      ]);
      openContextMenu();
      const trigger = screen.getByText(/Regenerate/).closest('button')!;
      expect(trigger.hasAttribute('disabled')).toBe(true);
      expect(trigger.getAttribute('title')).toMatch(/no other providers/i);
    });

    describe('keyboard navigation (#774)', () => {
      it('ArrowRight on the trigger opens the picker and focuses the first provider', async () => {
        renderNode(
          makeNode({ provider: 'anthropic' }),
          [
            makeProvider('anthropic', { group_key: 'anthropic', harness_id: 'anthropic' }),
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
        // First provider row (the claude group is rendered first
        // since `groupByHarness` walks the input order — anthropic is
        // excluded, claude comes next).
        await waitFor(() => {
          const firstItem = screen.getByTestId('regenerate-submenu')
            .querySelector<HTMLButtonElement>('[role="menuitem"]');
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

      it('ArrowDown inside the picker advances through the flat provider list with wrap', async () => {
        renderNode(
          makeNode({ provider: 'anthropic' }),
          [
            makeProvider('anthropic', { group_key: 'anthropic', harness_id: 'anthropic' }),
            makeProvider('claude', { label: 'Claude Code', group_key: 'claude', harness_id: 'claude' }),
            makeProvider('kimi', { label: 'Kimi', group_key: 'kimi', harness_id: 'kimi' }),
          ],
        );
        openContextMenu();
        fireEvent.keyDown(document, { key: 'ArrowRight' });
        await waitFor(() => {
          expect(screen.getByTestId('regenerate-submenu')).toBeTruthy();
        });

        // claude → kimi → wrap back to claude
        const items = () => Array.from(
          screen.getByTestId('regenerate-submenu').querySelectorAll<HTMLButtonElement>('[role="menuitem"]'),
        );
        expect(document.activeElement).toBe(items()[0]);
        fireEvent.keyDown(document, { key: 'ArrowDown' });
        expect(document.activeElement).toBe(items()[1]);
        fireEvent.keyDown(document, { key: 'ArrowDown' });
        // Wrap-around: kimi → claude.
        expect(document.activeElement).toBe(items()[0]);
      });
    });
  });
});
