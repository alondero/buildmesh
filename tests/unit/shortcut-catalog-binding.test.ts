/**
 * Catalog ↔ dispatch parity (issues #748 and #2240).
 *
 * The cheatsheet advertises a key the user can press, and the dispatch table
 * is what makes it do something. If those two disagree the app lies to the
 * user in one of two directions:
 *
 *   - a binding with no catalog row — the gesture works but is undiscoverable;
 *   - a catalog row with no handler — the cheatsheet advertises a key that
 *     does nothing.
 *
 * `SHORTCUT_CATALOG` is now built from the dispatch table's own rows
 * (`globalShortcutCatalogEntries()`), so the first direction cannot happen by
 * construction and is asserted anyway. The second direction is only possible
 * for the *context-bound* rows (terminal/modal/grid-search gestures owned by a
 * component, not the app dispatcher), so those are pinned by name: a new
 * orphan row has to be declared here with its owning surface, and
 * `dispatchShortcut` reports it as `'unknown-action'`.
 *
 * This file previously read `src/App.tsx` and regex-extracted `key:`/`action:`
 * pairs out of the bindings array. That approach could not see the
 * helper-generated bindings and missed anything the regex's shape did not
 * anticipate — the review at issue #2240 called the pair-check brittle and the
 * coverage one-dimensional.
 */
import { describe, it, expect } from 'vitest';

import { SHORTCUT_ACTIONS, dispatchShortcut } from '../../src/lib/shortcutDispatch';
import { SHORTCUT_CATALOG } from '../../src/lib/shortcutCatalog';

/**
 * Catalog rows with no dispatch-table handler, and where each one is actually
 * handled. Every catalog entry NOT listed here must have a handler — that is
 * the assertion below, so adding an undispatched row fails until it is
 * declared with its owner.
 */
const CONTEXT_BOUND: ReadonlyArray<readonly [action: string, owner: string]> = [
  ['clear-grid-search', "GridControls' search input onKeyDown"],
  ['zoom-reset', 'Terminal.tsx window keydown listener (resolveZoomKeyAction)'],
  ['zoom-in', 'Terminal.tsx window keydown listener (resolveZoomKeyAction)'],
  ['zoom-out', 'Terminal.tsx window keydown listener (resolveZoomKeyAction)'],
  ['term-copy', 'TerminalRegistry.attachCustomKeyEventHandler'],
  ['term-paste', 'TerminalRegistry.attachCustomKeyEventHandler'],
  ['term-select-all', 'TerminalRegistry.attachCustomKeyEventHandler'],
  ['term-find', 'TerminalRegistry.attachCustomKeyEventHandler'],
  ['term-clear', 'TerminalRegistry.attachCustomKeyEventHandler'],
  ['close-modal', 'the <Modal> primitive window keydown listener'],
];

describe('shortcut catalog ↔ dispatch table parity', () => {
  const tableActions = SHORTCUT_ACTIONS.map(s => s.action);
  const catalogActions = SHORTCUT_CATALOG.map(e => e.action);

  it('gives every dispatch-table action a catalog entry (a live binding is discoverable)', () => {
    const catalogSet = new Set(catalogActions);
    for (const action of tableActions) {
      expect(catalogSet.has(action), `'${action}' has a binding but no cheatsheet row`).toBe(true);
    }
  });

  it('gives every catalog entry a handler, directly or as a declared context-bound gesture', () => {
    const handled = new Set([...tableActions, ...CONTEXT_BOUND.map(([action]) => action)]);
    const orphans = catalogActions.filter(action => !handled.has(action));
    expect(
      orphans,
      'these cheatsheet rows have no handler — add one to the dispatch table, or declare the owning surface above',
    ).toEqual([]);
  });

  it('declares a real owning surface for every context-bound row', () => {
    // A declaration with an empty owner string would pass the orphan check
    // while telling the next reader nothing.
    for (const [action, owner] of CONTEXT_BOUND) {
      expect(catalogActions, `${action} is declared context-bound but has no catalog row`).toContain(action);
      expect(owner.trim().length, `${action} needs an owning surface`).toBeGreaterThan(0);
    }
  });

  it('reports the context-bound rows as unknown-action rather than silently claiming them', () => {
    // The flip side of the parity above: a row with no table entry must not be
    // swallowed by the dispatcher as "handled".
    for (const [action] of CONTEXT_BOUND) {
      expect(dispatchShortcut(action), `${action} must not be app-dispatched`).toBe('unknown-action');
    }
  });

  it('keeps action ids unique across the whole catalog', () => {
    expect(new Set(catalogActions).size).toBe(catalogActions.length);
  });

  it('keeps action ids unique across the table rows', () => {
    // A duplicate row would silently lose one of the two handlers: the lookup
    // map keeps the last row for an id, so the first row's accelerator would
    // still be registered while its guard and effect never run.
    expect(new Set(tableActions).size).toBe(tableActions.length);
  });

  it('gives every catalog row complete display metadata', () => {
    for (const entry of SHORTCUT_CATALOG) {
      expect(entry.group, `${entry.action} group`).toBeTruthy();
      expect(entry.description.trim(), `${entry.action} description`).not.toBe('');
      expect(entry.winKey.trim(), `${entry.action} winKey`).not.toBe('');
      expect(entry.macKey.trim(), `${entry.action} macKey`).not.toBe('');
    }
  });

  it('does NOT wire the obsolete Ctrl+Alt+D shortcut the README once phantom-documented', () => {
    // Companion to `tests/unit/shortcut-catalog.test.ts`: that file asserts the
    // catalog doesn't list `delete-mesh` / `open-debug`; here we assert neither
    // side of the app dispatches them either.
    expect(tableActions).not.toContain('delete-mesh');
    expect(tableActions).not.toContain('open-debug');
    expect(catalogActions).not.toContain('delete-mesh');
    expect(catalogActions).not.toContain('open-debug');
  });
});