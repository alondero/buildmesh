/**
 * Tests for the keyboard shortcut catalog (issue #731).
 *
 * The catalog is the single source of truth for the cheatsheet: every
 * shortcut the user can press in Buildmesh, with a platform-aware label
 * and a grouping for the cheatsheet's section headings. App.tsx owns the
 * Tauri-binding side; the cheatsheet modal reads from this catalog.
 *
 * Drift here would be visible — the cheatsheet would either lie (listing
 * shortcuts that don't fire) or omit a shortcut a user just learned about
 * elsewhere in the UI. These tests pin the contract.
 */
import { describe, it, expect } from 'vitest';
import {
  SHORTCUT_CATALOG,
  shortcutLabel,
  groupedShortcutEntries,
  type ShortcutEntry,
  type ShortcutGroup,
} from '../../src/lib/shortcutCatalog';

describe('shortcutCatalog', () => {
  it('exports a non-empty catalog with one entry per shipped shortcut', () => {
    expect(SHORTCUT_CATALOG.length).toBeGreaterThanOrEqual(10);
  });

  it('gives every entry a unique action id so App.tsx can key on it', () => {
    const actions = SHORTCUT_CATALOG.map(s => s.action);
    expect(new Set(actions).size).toBe(actions.length);
  });

  it('gives every entry a non-empty description, group, and both platform labels', () => {
    for (const entry of SHORTCUT_CATALOG) {
      expect(entry.description.trim().length, `${entry.action} description`).toBeGreaterThan(0);
      expect(entry.winKey.trim().length, `${entry.action} winKey`).toBeGreaterThan(0);
      expect(entry.macKey.trim().length, `${entry.action} macKey`).toBeGreaterThan(0);
      expect(['app', 'grid', 'terminal', 'modal']).toContain(entry.group);
    }
  });

  it('surfaces the canonical window-global shortcuts (issue #731 acceptance criteria)', () => {
    const actions = new Set(SHORTCUT_CATALOG.map(s => s.action));
    // From src/App.tsx Tauri global-shortcut registrations:
    expect(actions.has('new-agent')).toBe(true);
    expect(actions.has('toggle-maximize-grid')).toBe(true);
    expect(actions.has('arrow-left')).toBe(true);
    expect(actions.has('arrow-right')).toBe(true);
    expect(actions.has('arrow-up')).toBe(true);
    expect(actions.has('arrow-down')).toBe(true);
    expect(actions.has('jump-to-next-awaiting')).toBe(true);
    expect(actions.has('open-omnibar')).toBe(true);
    expect(actions.has('open-omnibar-commands')).toBe(true);
  });

  it('pins the Omnibar bindings (issue #1409): ⌘/Ctrl+K and ⌘/Ctrl+P, K is splash, P is not', () => {
    const omnibar = SHORTCUT_CATALOG.find(s => s.action === 'open-omnibar');
    const omnibarCommands = SHORTCUT_CATALOG.find(s => s.action === 'open-omnibar-commands');
    expect(omnibar).toBeDefined();
    expect(omnibar?.winKey).toBe('Ctrl+Shift+K');
    expect(omnibar?.macKey).toBe('⌘+K');
    expect(omnibar?.group).toBe('app');
    expect(omnibar?.splash).toBe(true);
    expect(omnibarCommands).toBeDefined();
    expect(omnibarCommands?.winKey).toBe('Ctrl+Shift+P');
    expect(omnibarCommands?.macKey).toBe('⌘+P');
    expect(omnibarCommands?.splash).toBeUndefined();
  });

  it('does not advertise bare Ctrl+K / Ctrl+P on Windows/Linux (readline collision, issue #1409 review)', () => {
    // Bare Ctrl+K (kill-line) and Ctrl+P (previous-history) must keep
    // reaching Win/Linux shells; the palette chords carry Shift there.
    const omnibar = SHORTCUT_CATALOG.find(s => s.action === 'open-omnibar');
    const omnibarCommands = SHORTCUT_CATALOG.find(s => s.action === 'open-omnibar-commands');
    expect(omnibar?.winKey).not.toBe('Ctrl+K');
    expect(omnibarCommands?.winKey).not.toBe('Ctrl+P');
  });

  it('pins terminal clear as Ctrl+Shift+L on Win/Linux (issue #1568 — remapped off the Omnibar chord)', () => {
    const clear = SHORTCUT_CATALOG.find(s => s.action === 'term-clear');
    expect(clear).toBeDefined();
    expect(clear?.winKey).toBe('Ctrl+Shift+L');
    expect(clear?.macKey).toBe('⌘+Shift+K');
  });

  it('surfaces terminal-zoom shortcuts (issue #667) wired in Terminal.tsx', () => {
    const actions = new Set(SHORTCUT_CATALOG.map(s => s.action));
    expect(actions.has('zoom-reset')).toBe(true);
    expect(actions.has('zoom-in')).toBe(true);
    expect(actions.has('zoom-out')).toBe(true);
  });

  it('labels the zoom-in chord as Ctrl+= (the actual US-keyboard chord, issue #1264)', () => {
    // The previous `Ctrl++` label was the literal "press Ctrl and the
    // + key" rendering, but the actual US-keyboard chord is `Ctrl+=`
    // and the matcher in `terminalKeyAction.ts:60` accepts both `=`
    // and `+` for tolerance. Pin the display label here so a future
    // edit that flips it back to `Ctrl++` (or to `Ctrl+Shift+=`, the
    // "type a literal +" chord) is caught at test time.
    const zoomIn = SHORTCUT_CATALOG.find((s) => s.action === 'zoom-in');
    expect(zoomIn).toBeDefined();
    expect(zoomIn?.winKey).toBe('Ctrl+=');
    expect(zoomIn?.macKey).toBe('⌘+=');
  });

  it('surfaces terminal context-menu actions wired in TerminalRegistry.attachCustomKeyEventHandler', () => {
    const actions = new Set(SHORTCUT_CATALOG.map(s => s.action));
    expect(actions.has('term-copy')).toBe(true);
    expect(actions.has('term-paste')).toBe(true);
    expect(actions.has('term-select-all')).toBe(true);
    expect(actions.has('term-find')).toBe(true);
    expect(actions.has('term-clear')).toBe(true);
  });

  it('surfaces Escape (close-modal) — owned by the <Modal> primitive, not Tauri', () => {
    const close = SHORTCUT_CATALOG.find(s => s.action === 'close-modal');
    expect(close).toBeDefined();
    expect(close?.winKey).toBe('Esc');
    expect(close?.macKey).toBe('Esc');
    expect(close?.group).toBe('modal');
  });

  it('uses the same Alt+G / Cmd+G split for toggle-maximize-grid as App.tsx (#668)', () => {
    const toggle = SHORTCUT_CATALOG.find(s => s.action === 'toggle-maximize-grid');
    expect(toggle?.winKey).toBe('Alt+G');
    expect(toggle?.macKey).toBe('⌘+G');
  });

  it('surfaces the view-mode cycle (ticket #987) with the Alt/⌥-carrying binding', () => {
    // Ctrl+Alt+G / Cmd+Alt+G — the keyboard peer to the ViewModeSwitcher. The
    // extra Alt/⌥ modifier is what keeps it clear of Alt+G / ⌘+G (the Single
    // solo toggle), so pin the labels here against a regression that drops the
    // modifier and re-collides the two grid bindings.
    const cycle = SHORTCUT_CATALOG.find(s => s.action === 'cycle-grid-modes');
    expect(cycle).toBeDefined();
    expect(cycle?.group).toBe('grid');
    expect(cycle?.winKey).toBe('Ctrl+Alt+G');
    expect(cycle?.macKey).toBe('⌘+⌥+G');
    // Deliberately not a splash entry — the splash is the pre-spawn empty-state
    // hint, where there are no nodes to cycle Mesh/Pinned/All over.
    expect(cycle?.splash).toBeUndefined();
  });

  it('surfaces focus-grid-search (issue #998) with the macOS ⌘+⌥+F two-modifier collision carve-out', () => {
    // Cmd/Ctrl+F is the editors' universal "find" chord. On Win/Linux bare
    // Ctrl+F is free (no readline gesture uses it), but on macOS bare ⌘+F
    // is already taken by `term-find` (xterm's `attachCustomKeyEventHandler`
    // matches ⌘+F to the terminal's find action). The catalog MUST reflect
    // the platform split App.tsx wires: Ctrl+F on Win/Linux, ⌘+⌥+F on
    // macOS. A regression that drops the ⌘+⌥+F modifier would re-collide
    // with `term-find` and silently steal it from focused agent terminals.
    const focus = SHORTCUT_CATALOG.find(s => s.action === 'focus-grid-search');
    expect(focus).toBeDefined();
    expect(focus?.group).toBe('grid');
    expect(focus?.winKey).toBe('Ctrl+F');
    expect(focus?.macKey).toBe('⌘+⌥+F');
    // Not a splash entry — the splash advertises gestures useful on the
    // pre-spawn empty canvas, where there are no nodes to search.
    expect(focus?.splash).toBeUndefined();
  });

  it('surfaces clear-grid-search (issue #998) as Esc in the grid group, distinct from close-modal', () => {
    // Esc already lives in the catalog as `close-modal` (modal group, owned
    // by the <Modal> primitive's window keydown listener). The grid-search
    // Esc is a *contextual* clear — handled inside the input's own
    // onKeyDown so it only fires when the input has focus, never when
    // the user is closing a dialog. Pin both entries so the cheatsheet
    // doesn't accidentally collapse them into one.
    const clear = SHORTCUT_CATALOG.find(s => s.action === 'clear-grid-search');
    expect(clear).toBeDefined();
    expect(clear?.group).toBe('grid');
    expect(clear?.winKey).toBe('Esc');
    expect(clear?.macKey).toBe('Esc');
    // Not splash — the splash is the pre-spawn empty state, where there
    // is no grid to search.
    expect(clear?.splash).toBeUndefined();
  });

  it('does not advertise the obsolete Ctrl+Alt+D shortcut the README once phantom-documented', () => {
    // The keyboard-shortcut-conventions memory notes: "drift here has
    // shipped at least once (Ctrl+Alt+D was phantom-documented)".
    // Regression guard against reintroducing it.
    const actions = new Set(SHORTCUT_CATALOG.map(s => s.action));
    expect(actions.has('delete-mesh')).toBe(false);
    expect(actions.has('open-debug')).toBe(false);
  });

  it('flags the empty-state splash subset (issue #748)', () => {
    // The empty-state splash in AgentNodeView.tsx renders one row per
    // entry flagged `splash: true`. Pin the exact set so a refactor that
    // accidentally drops one (e.g. when deleting an entry) is caught here
    // instead of leaving a hole in the user-facing discoverability hint.
    const splashActions = SHORTCUT_CATALOG.filter(e => e.splash).map(e => e.action).sort();
    expect(splashActions).toEqual([
      'arrow-down',
      'arrow-left',
      'arrow-right',
      'arrow-up',
      'close-modal',
      'jump-to-next-awaiting',
      'new-agent',
      'open-cheatsheet',
      'open-omnibar',
      'toggle-maximize-grid',
    ]);
  });

  it('does not flag terminal-only shortcuts in the splash (splash is the discoverability hint, not the full catalog)', () => {
    // Terminal zoom / copy / paste / find / clear shortcuts are bound on
    // focused xterm instances, not on the empty-state splash. The splash
    // appears BEFORE any agent has spawned, so none of those actions are
    // meaningful to advertise there yet. Regression guard against a
    // refactor that blanket-flags every catalog entry with `splash: true`.
    const splashActions = new Set(
      SHORTCUT_CATALOG.filter(e => e.splash).map(e => e.action),
    );
    const terminalOnly = [
      'zoom-reset', 'zoom-in', 'zoom-out',
      'term-copy', 'term-paste', 'term-select-all', 'term-find', 'term-clear',
    ];
    for (const action of terminalOnly) {
      expect(splashActions.has(action), `${action} should NOT be splash`).toBe(false);
    }
  });
});

describe('shortcutLabel', () => {
  const entry: ShortcutEntry = {
    action: 'new-agent',
    group: 'app',
    description: 'New agent node',
    winKey: 'Ctrl+T',
    macKey: '⌘+T',
  };

  it('returns the macKey on macOS (matches the isMac convention in src/lib/platform.ts)', () => {
    expect(shortcutLabel(entry, true)).toBe('⌘+T');
  });

  it('returns the winKey on Windows/Linux', () => {
    expect(shortcutLabel(entry, false)).toBe('Ctrl+T');
  });
});

describe('groupedShortcutEntries', () => {
  it('groups entries under their group name in display order: app, grid, terminal, modal', () => {
    const groups = groupedShortcutEntries(SHORTCUT_CATALOG);
    const order = groups.map(g => g.group);
    // The first four group slots should be the canonical display order.
    expect(order.slice(0, 4)).toEqual<ShortcutGroup[]>(['app', 'grid', 'terminal', 'modal']);
  });

  it('preserves the original entry order within a group', () => {
    const groups = groupedShortcutEntries(SHORTCUT_CATALOG);
    for (const g of groups) {
      const actionsInGroup = SHORTCUT_CATALOG
        .filter(s => s.group === g.group)
        .map(s => s.action);
      expect(g.entries.map(e => e.action)).toEqual(actionsInGroup);
    }
  });

  it('only emits groups that have at least one entry (no empty sections)', () => {
    const groups = groupedShortcutEntries(SHORTCUT_CATALOG);
    for (const g of groups) {
      expect(g.entries.length).toBeGreaterThan(0);
    }
  });
});
