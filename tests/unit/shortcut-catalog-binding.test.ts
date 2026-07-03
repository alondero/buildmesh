/**
 * Static-analysis test: every `action` field in App.tsx's Tauri
 * global-shortcut `shortcuts` array MUST be a key in SHORTCUT_CATALOG.
 *
 * Issue #748. The catalog test (`tests/unit/shortcut-catalog.test.ts`) pins
 * the *forward* direction — required entries by name, so an orphan catalog
 * entry that no binding wires up is caught. But nothing pinned the
 * *reverse* direction: a binding added to App.tsx without a catalog entry
 * would silently be omitted from the cheatsheet (the discoverability
 * surface the user opens with `?`). Code review was the only guard.
 *
 * This file closes the gap by reading `src/App.tsx` source directly,
 * extracting the `shortcuts` array literal at the global-shortcut
 * registration site, and asserting each entry's `action` field is present
 * as a key in `SHORTCUT_CATALOG`. The literal-regex shape matches the
 * sibling test `tests/unit/grid-shortcut-binding.test.ts` — keyed on the
 * `key:`-then-`action:` pair so a binding string appearing in a comment
 * or docblock doesn't satisfy the assertion.
 *
 * Static-analysis shape (not runtime): the Tauri global-shortcut plugin's
 * `register()` returns an unlisten promise that can't be inspected cheaply
 * from a jsdom test, and mocking it would be brittle. Reading the source
 * is the cheapest way to keep the bindings honest — the regression caught
 * by issue #748 was a literal omission in source, not a runtime branch.
 */
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { SHORTCUT_CATALOG } from '../../src/lib/shortcutCatalog';

describe('App.tsx Tauri global-shortcut bindings map to SHORTCUT_CATALOG entries', () => {
  const appSource = readFileSync(
    resolve(__dirname, '../../src/App.tsx'),
    'utf8',
  );

  // Extract every `{ key: '...', action: '...' }` object literal inside
  // the `shortcuts = [ ... ]` array at the global-shortcut registration
  // site. Anchored on the `key:`-then-`action:` pair (same shape as the
  // sibling `grid-shortcut-binding.test.ts`) so a binding string
  // appearing in a comment or docblock doesn't satisfy the assertion —
  // e.g. a future `// Replaced action: 'foo' with bar` would otherwise
  // pull `'foo'` into the wired set and the "every wired action has a
  // catalog entry" test would silently enforce a phantom key.
  //
  // The platform-branched `gridToggleShortcut` const above the array is
  // a separate object literal; the regex matches it via its inline
  // `key:`-then-`action:` form, so platform-branched bindings aren't
  // missed.
  const entryMatches = appSource.matchAll(
    /key:\s*'[A-Za-z0-9+\-]+',\s*action:\s*'([a-z][a-z0-9-]*)'/g,
  );
  const wiredActions = new Set<string>();
  for (const m of entryMatches) wiredActions.add(m[1]);

  // Sanity check on the regex: App.tsx MUST have at least the four
  // arrow-traversal actions wired. If this fails, the regex itself has
  // drifted (e.g. someone added `action:` quotes somewhere that breaks
  // the shape) and the rest of the assertions would be vacuously green.
  it('regex finds the canonical App.tsx bindings (sanity check)', () => {
    expect(wiredActions.has('new-agent')).toBe(true);
    expect(wiredActions.has('arrow-left')).toBe(true);
    expect(wiredActions.has('arrow-right')).toBe(true);
    expect(wiredActions.has('arrow-up')).toBe(true);
    expect(wiredActions.has('arrow-down')).toBe(true);
    expect(wiredActions.has('toggle-maximize-grid')).toBe(true);
  });

  it('every wired action has a matching SHORTCUT_CATALOG entry', () => {
    const catalogActions = new Set(SHORTCUT_CATALOG.map(e => e.action));
    for (const action of wiredActions) {
      expect(
        catalogActions.has(action),
        `App.tsx wires action '${action}' but SHORTCUT_CATALOG has no entry for it — the cheatsheet will silently omit this binding (issue #748)`,
      ).toBe(true);
    }
  });

  it('does NOT wire the obsolete Ctrl+Alt+D shortcut the README once phantom-documented', () => {
    // Companion to the catalog test: the catalog already asserts the
    // catalog doesn't list `delete-mesh` / `open-debug`; here we assert
    // App.tsx doesn't wire them either. Belt-and-braces: a future
    // re-introduction would have to slip past both tests.
    expect(wiredActions.has('delete-mesh')).toBe(false);
    expect(wiredActions.has('open-debug')).toBe(false);
  });
});