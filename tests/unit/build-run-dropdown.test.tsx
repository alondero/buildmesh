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

  describe('keyboard navigation (issue #814)', () => {
    it('ArrowDown cycles focus between Build → Run → Terminal → Build', () => {
      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      openMenu();
      const items = screen.getAllByRole('menuitem');
      // Item 0 (Build) is auto-focused on open.
      expect(document.activeElement).toBe(items[0]);
      // Advance.
      fireEvent.keyDown(document.activeElement!, { key: 'ArrowDown' });
      expect(document.activeElement).toBe(items[1]);
      fireEvent.keyDown(document.activeElement!, { key: 'ArrowDown' });
      expect(document.activeElement).toBe(items[2]);
      // Wrap around to first.
      fireEvent.keyDown(document.activeElement!, { key: 'ArrowDown' });
      expect(document.activeElement).toBe(items[0]);
    });

    it('ArrowUp cycles with wrap-around (Build → Terminal → Run → Build)', () => {
      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      openMenu();
      const items = screen.getAllByRole('menuitem');
      // Up from first wraps to last.
      fireEvent.keyDown(items[0], { key: 'ArrowUp' });
      expect(document.activeElement).toBe(items[2]);
      fireEvent.keyDown(document.activeElement!, { key: 'ArrowUp' });
      expect(document.activeElement).toBe(items[1]);
      fireEvent.keyDown(document.activeElement!, { key: 'ArrowUp' });
      expect(document.activeElement).toBe(items[0]);
    });

    it('Home jumps to the first item, End jumps to the last', () => {
      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      openMenu();
      const items = screen.getAllByRole('menuitem');
      items[1].focus();
      fireEvent.keyDown(document.activeElement!, { key: 'End' });
      expect(document.activeElement).toBe(items[2]);
      fireEvent.keyDown(document.activeElement!, { key: 'Home' });
      expect(document.activeElement).toBe(items[0]);
    });
  });

  describe('Escape closes the menu and returns focus to the trigger (issue #814)', () => {
    it('Escape closes the menu and returns focus to the trigger button', () => {
      // The WAI-ARIA contract: closing a menu via Escape MUST return
      // focus to the element that opened it (the trigger). Without
      // this, keyboard users land "nowhere" — a screen-reader trap.
      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      const trigger = screen.getByLabelText('Open build menu');
      // `fireEvent.click` (not `trigger.click()`) — React's synthetic
      // event system is wired through Testing Library's dispatcher, so
      // the native HTMLElement.click() in jsdom doesn't reach React's
      // onClick handler reliably.
      fireEvent.click(trigger);
      // Sanity: menuitems are in the DOM (the strongest signal that
      // the menu opened — `getByRole('menu')` would clash with the
      // trigger's `aria-haspopup="menu"` which some accessibility
      // libraries also resolve to role="menu").
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
      // The keydown listener is gated on `isOpen`. Pressing Escape
      // while the menu is closed must not throw or interfere.
      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      expect(() => fireEvent.keyDown(document, { key: 'Escape' })).not.toThrow();
    });
  });

  describe('viewport clamping (issue #814)', () => {
    let rectSpy: ReturnType<typeof vi.spyOn> | undefined;

    afterEach(() => {
      rectSpy?.mockRestore();
      rectSpy = undefined;
    });

    it('applies a negative translateY when the menu would overflow the bottom of the viewport', () => {
      // Stub `HTMLElement.prototype.getBoundingClientRect` so the
      // layout effect sees an overflow rect on its initial mount.
      // (A per-element spy would be on a detached node because the
      // menu unmounts when isOpen flips false — the prototype mock
      // covers the freshly mounted element on the next open.)
      //
      // The mock positions the menu near the bottom of the viewport
      // (top=600) with a height that extends past the bottom (700 +
      // 200 = 900 > 768). `rect.top=600` gives the cap `maxShift =
      // rect.top - MARGIN = 596`, plenty of room to shift up by the
      // full overflow (900 - 764 = 136). A `top=0` mock would
      // cap the shift at 0 and the test would falsely see no
      // transform applied.
      rectSpy = vi
        .spyOn(HTMLElement.prototype, 'getBoundingClientRect')
        .mockReturnValue({
          top: 600,
          bottom: 900,
          left: 0,
          right: 200,
          width: 200,
          height: 300,
          x: 0,
          y: 600,
          toJSON: () => ({}),
        } as DOMRect);

      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      openMenu();
      const menu = document.querySelector('[role="menu"]') as HTMLElement;
      expect(menu.style.transform).toMatch(/translateY\(-/);
    });

    it('does not apply translateY when the menu fits in the viewport', () => {
      // Mock returns a rect that fits comfortably inside the viewport.
      rectSpy = vi
        .spyOn(HTMLElement.prototype, 'getBoundingClientRect')
        .mockReturnValue({
          top: 100,
          bottom: 250,
          left: 0,
          right: 200,
          width: 200,
          height: 150,
          x: 0,
          y: 100,
          toJSON: () => ({}),
        } as DOMRect);

      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      openMenu();
      const menu = document.querySelector('[role="menu"]') as HTMLElement;
      // No overflow → no transform applied.
      expect(menu.style.transform).toBe('');
    });
  });

  describe('Tab closes the menu (WAI-ARIA non-modal popover, issue #814)', () => {
    it('Tab leaves the menu and closes it (no focus trap)', () => {
      // A `role="menu"` is a non-modal popover — Tab moves focus to the
      // next tabbable element on the page and the menu closes. (A
      // modal menu would trap focus; the WAI-ARIA `menu` role is
      // explicitly the non-modal variant.)
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

    it('scopes the dropdown with data-dropdown-for=<node.id> on the outer wrapper', () => {
      // The hook's selector is built from `String(open)`, so a
      // mismatch (e.g. a boolean coercion) would silently break the
      // scoping across sibling dropdowns. Pin the attribute value
      // AND the placement: the attribute lives on the OUTER wrapper
      // (not the menu popup) so a click on the trigger is "inside"
      // and doesn't race with the toggle.
      const onBuildRun = vi.fn();
      render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);
      openMenu();
      const wrapper = document.querySelector('[data-dropdown-for]') as HTMLElement;
      expect(wrapper).toBeTruthy();
      expect(wrapper.getAttribute('data-dropdown-for')).toBe(String(NODE.id));
      // The menu itself does NOT carry the attribute (only the wrapper).
      const menu = document.querySelector('[role="menu"]') as HTMLElement;
      expect(menu.hasAttribute('data-dropdown-for')).toBe(false);
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
