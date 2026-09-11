/**
 * The Build menu on an agent node grew a third item — a raw interactive
 * terminal session started in the worktree directory — to give users a
 * scratch space without leaving the mesh. The terminal item sits below a
 * divider so it doesn't get conflated with the one-shot build/run commands,
 * and the label adapts to the worktree context (the same way Build/Run do)
 * so the user can tell at a glance which directory the shell will land in.
 *
 * The trigger button is now an icon-only wrench + chevron (matching the
 * close + expand buttons in GridNodeHeader for a balanced trio). Title-bar
 * space shrinks ~34 px and the menu items keep their original labels.
 */
import { describe, it, expect, vi } from 'vitest';
import { render, fireEvent, screen, act } from '@testing-library/react';
import { type AgentNode } from '../../src/stores/agentNodeStore';
import { BuildRunDropdown } from '../../src/components/BuildRun/BuildRunDropdown';

const NODE: AgentNode = {
  id: 7,
  mesh_id: 1,
  name: 'agent-7',
  path: '/repo',
  branch: 'main',
  env: 'wsl',
  provider: 'anthropic',
  status: 'running',
  use_worktree: false,
  created_at: new Date(0).toISOString(),
};

/**
 * The trigger is icon-only and identified by `aria-label` rather than its
 * old "Build" text label. Using the accessible name (instead of any visible
 * label) keeps this helper robust against future label/icon tweaks as long
 * as the aria-label contract holds.
 */
function openMenu() {
  fireEvent.click(screen.getByLabelText('Open build menu'));
}

describe('BuildRunDropdown', () => {
  it('hides the "Build" word from the trigger and uses an accessible name', () => {
    const onBuildRun = vi.fn();
    render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);

    // No "Build" text on the trigger — it should not exist before the menu
    // opens. After openMenu() below, the only "Build" matches are the menu
    // item, never the trigger.
    expect(screen.queryByText('Build')).toBeNull();
    expect(screen.getByLabelText('Open build menu')).toBeTruthy();

    openMenu();
    // Now there IS one "Build" — the menu item, not the trigger.
    expect(screen.getAllByText('Build')).toHaveLength(1);
  });

  it('trigger matches the close + maximise trio surface (h-7 + bg + border)', () => {
    // The trio lives on a single row in GridNodeHeader. The close + expand
    // asserts live in grid-node-header.test.tsx (where BuildRunDropdown is
    // mocked to null for unrelated git-summary-chip tests); the Build-side
    // counterpart has to live here where BuildRunDropdown is the real DOM.
    // Note: the trio shares HEIGHT (h-7), not width — Build is content-width
    // (wrench + chevron), while close + maximise are fixed square w-7 h-7.
    const onBuildRun = vi.fn();
    render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);

    const cls = screen.getByLabelText('Open build menu').className;
    expect(cls).toMatch(/\bh-7\b/);
    expect(cls).toContain('bg-bg-base/60');
    expect(cls).toContain('border-border-default');
  });

  it('renders Build, Run, and Terminal items when the menu is open (worktrees off)', () => {
    const onBuildRun = vi.fn();
    render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);

    openMenu();

    expect(screen.getByText('Build')).toBeTruthy();
    expect(screen.getByText('Run')).toBeTruthy();
    expect(screen.getByText('Terminal')).toBeTruthy();
  });

  it('renders the worktree-suffixed labels when use_worktree is true', () => {
    const onBuildRun = vi.fn();
    render(
      <BuildRunDropdown
        node={{ ...NODE, use_worktree: true }}
        onBuildRun={onBuildRun}
      />,
    );

    openMenu();

    expect(screen.getByText('Build from worktree')).toBeTruthy();
    expect(screen.getByText('Run from worktree')).toBeTruthy();
    expect(screen.getByText('Terminal in worktree')).toBeTruthy();
    // The menu items must use the suffixed form — there should be NO
    // bare "Run"/"Terminal" menu items.
    expect(screen.queryByText('Run')).toBeNull();
    expect(screen.queryByText('Terminal')).toBeNull();
  });

  it('invokes onBuildRun with terminal mode when the terminal item is clicked', () => {
    const onBuildRun = vi.fn();
    render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);

    openMenu();
    fireEvent.click(screen.getByText('Terminal'));

    expect(onBuildRun).toHaveBeenCalledWith(NODE.id, 'terminal');
  });

  it('still invokes onBuildRun with build and run modes (regression)', () => {
    const onBuildRun = vi.fn();
    render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);

    openMenu();
    fireEvent.click(screen.getByText('Build'));
    expect(onBuildRun).toHaveBeenLastCalledWith(NODE.id, 'build');

    openMenu();
    fireEvent.click(screen.getByText('Run'));
    expect(onBuildRun).toHaveBeenLastCalledWith(NODE.id, 'run');
  });

  describe('WAI-ARIA menu semantics (issue #814)', () => {
    it('declares role="menu" on the menu container with an accessible label', () => {
      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      openMenu();
      const menu = screen.getByRole('menu', { name: /Build, run/ });
      expect(menu).toBeTruthy();
    });

    it('marks every action as a menuitem (Build, Run, Terminal)', () => {
      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      openMenu();
      expect(screen.getAllByRole('menuitem')).toHaveLength(3);
      expect(screen.getByRole('menuitem', { name: 'Build' })).toBeTruthy();
      expect(screen.getByRole('menuitem', { name: 'Run' })).toBeTruthy();
      expect(screen.getByRole('menuitem', { name: 'Terminal' })).toBeTruthy();
    });

    it('puts only the first item in the natural tab order on open (roving tabindex)', () => {
      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      openMenu();
      const items = screen.getAllByRole('menuitem');
      expect(items[0].getAttribute('tabindex')).toBe('0');
      expect(items[1].getAttribute('tabindex')).toBe('-1');
      expect(items[2].getAttribute('tabindex')).toBe('-1');
    });
  });

  describe('Escape closes the menu and returns focus to the trigger (issue #814)', () => {
    it('Escape closes the menu and returns focus to the trigger button', () => {
      // The WAI-ARIA contract: closing a menu via Escape MUST return
      // focus to the element that opened it (the trigger). Without
      // this, keyboard users land "nowhere" — a screen-reader trap.
      //
      // Issue #837 — the keyboard nav cycle, Home/End jumps, focus-gate,
      // and onClose dispatch are now covered by `tests/unit/use-aria-menu.test.tsx`.
      // What stays here is the per-component behaviour: the rAF-based
      // trigger-focus return (the hook fires `onClose`, the component's
      // own closure does `requestAnimationFrame(() => trigger?.focus())`).
      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      const trigger = screen.getByLabelText('Open build menu');
      fireEvent.click(trigger);
      const items = screen.getAllByRole('menuitem');
      expect(items).toHaveLength(3);
      expect(document.activeElement).toBe(items[0]);
      // Press Escape — menu closes, focus returns to trigger.
      fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
      expect(screen.queryAllByRole('menuitem')).toHaveLength(0);
      // requestAnimationFrame is used to wait for the unmount before
      // focusing the trigger — flush microtasks + rAF so the assertion
      // sees the post-rAF state.
      return new Promise<void>((resolve) =>
        requestAnimationFrame(() => {
          expect(document.activeElement).toBe(trigger);
          resolve();
        }),
      );
    });

    it('Escape does nothing when the menu is closed', () => {
      // Issue #837 — the hook's `enabled: isOpen` gate detaches the
      // listener while the menu is closed. Pressing Escape while
      // closed must not throw or interfere.
      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      expect(() => fireEvent.keyDown(document, { key: 'Escape' })).not.toThrow();
    });
  });

  describe('portal + fixed positioning (PR #1699)', () => {
    // The header row is `overflow-hidden` (#1650 compact title bar), which
    // clipped the old inline `absolute right-0 top-full` menu to the ~36px
    // header strip — only one menu item was ever visible. Same root cause
    // the PR pill (#1585) and the kebab (#1589) were portaled for. The
    // menu now renders into `document.body` and is positioned by the
    // shared `useAnchoredPosition` hook (`fixed` viewport coordinates).
    it('renders the menu into document.body, not inside the trigger wrapper', () => {
      const onBuildRun = vi.fn();
      const { container } = render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      openMenu();
      const menu = screen.getByRole('menu', { name: /Build, run/ });
      expect(menu.parentElement).toBe(document.body);
      // Nothing menu-shaped inside the component's own DOM anymore.
      expect(container.querySelector('[role="menu"]')).toBeNull();
    });

    // The old useViewportClamp smoke only proved "some transform landed";
    // that was a paper tiger. This pins the actual `align: 'end'` math
    // from useAnchoredPosition with real geometry: the menu's left edge
    // must sit at triggerRect.right - menuRect.width (right-aligned to
    // the trigger) and its top edge at triggerRect.bottom + gap (4),
    // i.e. dropped below the trigger — not clamped-margin defaults.
    it('anchors the menu to the trigger (align end: right edge + below) via useAnchoredPosition', () => {
      // Geometry (viewport 1024x768 in jsdom):
      //   trigger: x=900..940, y=20..48 (h=28)
      //   menu:    w=176, h=90 — fits below (48+4+90=142 < 764), fits right.
      // Expected placement: left = 940-176 = 764, top = 48+4 = 52.
      const TRIGGER_RECT = { top: 20, bottom: 48, left: 900, right: 940, width: 40, height: 28, x: 900, y: 20 };
      const MENU_RECT = { top: 0, bottom: 90, left: 0, right: 176, width: 176, height: 90, x: 0, y: 0 };
      const rectSpy = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect')
        .mockImplementation(function (this: HTMLElement) {
          return this.getAttribute('role') === 'menu'
            ? { ...MENU_RECT, toJSON: () => ({}) } as DOMRect
            : { ...TRIGGER_RECT, toJSON: () => ({}) } as DOMRect;
        });

      try {
        const onBuildRun = vi.fn();
        render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
        openMenu();
        const menu = screen.getByRole('menu', { name: /Build, run/ });
        expect(menu.className).toMatch(/\bfixed\b/);
        expect(menu.style.left).toBe('764px');
        expect(menu.style.top).toBe('52px');
        expect(menu.style.visibility).toBe('');
      } finally {
        rectSpy.mockRestore();
      }
    });
  });

  describe('Tab closes the menu (WAI-ARIA non-modal popover, issue #814)', () => {
    it('Tab leaves the menu and closes it (no focus trap)', () => {
      // Issue #837 — the `closeOnTab` default in `useAriaMenu` is
      // `true`, so Tab invokes the hook's `onClose`. The deeper
      // contract (focus-gate, key dispatch) is covered in the hook
      // tests; this smoke proves the hook's default value is what
      // BuildRunDropdown consumes.
      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      openMenu();
      expect(screen.getByRole('menu')).toBeTruthy();
      fireEvent.keyDown(document.activeElement ?? document.body, { key: 'Tab' });
      expect(screen.queryByRole('menu')).toBeNull();
    });
  });

  describe('close-on-outside-click via the shared useClickOutside hook (issue #814)', () => {
    // Pre-#814 the dropdown hand-rolled a `ref.contains` mousedown
    // listener; the consolidation in #492's hook form is the canonical
    // primitive. The hook's selector is `[data-dropdown-for="<id>"]`
    // and must scope correctly per node id (multiple dropdowns can be
    // mounted simultaneously, one per agent node in the grid).

    it('scopes the dropdown with data-dropdown-for=<node.id> on the trigger and the portaled menu', () => {
      // The hook's selector is built from `String(open)`, so a
      // mismatch (e.g. a boolean coercion) would silently break the
      // scoping across sibling dropdowns. Pin the attribute value
      // AND the placement: the attribute lives on the trigger AND
      // the portaled menu popup (the portal removes the menu from
      // the wrapper's subtree, so both ends must be tagged for
      // `closest()` to classify a click on either as "inside").
      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      openMenu();
      const trigger = screen.getByLabelText('Open build menu');
      expect(trigger.getAttribute('data-dropdown-for')).toBe(`buildrun-${NODE.id}`);
      const menu = screen.getByRole('menu', { name: /Build, run/ });
      expect(menu.getAttribute('data-dropdown-for')).toBe(`buildrun-${NODE.id}`);
    });

    // PR #1699 — behavioral twin of the attribute pins above. The trigger
    // carries `data-dropdown-for` precisely so the close-on-mousedown and
    // the toggle-on-click don't fight: without the attribute, mousedown
    // closes the menu and the trailing click re-opens it (flicker race).
    // Assert the full user gesture — mousedown, then click — leaves the
    // menu CLOSED, not re-opened.
    it('toggle-click on the open trigger closes the menu without a re-open race', () => {
      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      const trigger = screen.getByLabelText('Open build menu');

      openMenu();
      expect(screen.queryByRole('menu')).toBeTruthy();

      // A real trigger press fires mousedown first (useClickOutside
      // listens for it), then mouseup, then the click that toggles.
      fireEvent.mouseDown(trigger);
      // Mid-gesture: mousedown classified as "inside" — menu still open.
      expect(screen.queryByRole('menu')).toBeTruthy();
      fireEvent.click(trigger);
      // Completed gesture: toggled closed, and no re-open.
      expect(screen.queryByRole('menu')).toBeNull();
      expect(onBuildRun).not.toHaveBeenCalled();
    });

    it('closes the menu on mousedown outside the scoped element', () => {
      const onBuildRun = vi.fn();
      render(
        <div>
          <button data-testid="outside">outside</button>
          <BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />
        </div>,
      );
      openMenu();
      expect(screen.queryByRole('menu')).toBeTruthy();
      fireEvent.mouseDown(screen.getByTestId('outside'));
      expect(screen.queryByRole('menu')).toBeNull();
    });

    it('does NOT close the menu on mousedown inside the scoped element', () => {
      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      openMenu();
      const buildItem = screen.getByRole('menuitem', { name: 'Build' });
      // mousedown on the menuitem must not flip isOpen — the click
      // handler below is the only path that closes the menu after a
      // pick. (Without this, a real user would have their Build action
      // cancelled because the menu closed mid-mousedown.)
      fireEvent.mouseDown(buildItem);
      expect(screen.queryByRole('menu')).toBeTruthy();
    });

    it('does not attach a mousedown listener while the menu is closed', () => {
      // Outside mousedown before the menu ever opened must be a no-op
      // (the hook only attaches while `open !== null`).
      const onBuildRun = vi.fn();
      render(
        <div>
          <button data-testid="outside">outside</button>
          <BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />
        </div>,
      );
      // Menu is closed → listener is detached → outside click is a no-op.
      fireEvent.mouseDown(screen.getByTestId('outside'));
      expect(screen.queryByRole('menu')).toBeNull();
    });
  });
});
