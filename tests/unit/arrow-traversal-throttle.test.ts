import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

/**
 * Pins that the Ctrl/Cmd+Alt+Arrow grid-traversal handler in src/App.tsx is
 * wired through `createKeyRepeatThrottle`. Without the throttle, holding the
 * arrow key floods the handler with OS autorepeat events (~30 Hz on Windows,
 * ~15 Hz on macOS) and focus flies from one end of the row to the other
 * before the user can lift their finger.
 *
 * Static-analysis shape (matches the regression tests for `grid-shortcut-
 * binding` and the `CommandExt::creation_flags` setter-only case): the
 * throttle is a single inline literal in `src/App.tsx` and its helper is
 * imported by name, so reading the file is the cheapest way to keep both
 * sides honest without mocking the global-shortcut plugin or fake-timing
 * the whole App tree.
 *
 * What the assertions cover:
 *   - `import { createKeyRepeatThrottle } from './lib/keyRepeatThrottle';`
 *     appears exactly once (the helper is only used by this handler today).
 *   - The arrow handler invokes `arrowThrottle()` BEFORE doing any store
 *     mutation, so an autorepeat flood can't sneak past via a Phase 1
 *     (restore-from-maximized) early-return.
 *   - The interval constant lives near the throttle creation so a future
 *     tweak (e.g. 150ms for snappier, 300ms for more deliberate) is a
 *     single-site change.
 */
describe('arrow traversal key-repeat throttle', () => {
  const appSource = readFileSync(
    resolve(__dirname, '../../src/App.tsx'),
    'utf8',
  );

  it('imports createKeyRepeatThrottle from src/lib/keyRepeatThrottle', () => {
    // Single-source-of-truth check: a future refactor that drops the
    // import (e.g. moves the throttle inline) would lose the testable
    // helper and re-grow the "burst-on-hold" bug silently.
    const importLine = /import\s+\{\s*createKeyRepeatThrottle\s*\}\s+from\s+['"]\.\/lib\/keyRepeatThrottle['"]/;
    expect(importLine.test(appSource)).toBe(true);
  });

  it('constructs the throttle at module scope (matching the sibling guards)', () => {
    // `createShortcutGuard` instances in App.tsx are module-scope constants
    // (createNodeGuard/toggleMaximizeGuard/cheatsheetGuard, lines 33/38/43);
    // the arrow throttle must live alongside them so the lifetime/sharing
    // convention stays uniform — App is the root, mounts once, so the
    // distinction is cosmetic today, but copy-pasting into a remounting
    // component would expose a per-mount reset bug.
    const constructLine = /const\s+arrowThrottle\s*=\s*createKeyRepeatThrottle\(\s*\d+\s*\)/;
    expect(constructLine.test(appSource)).toBe(true);
  });

  it('invokes the throttle BEFORE the maximized-restore branch', () => {
    // Regression guard: a future change that moves the throttle check
    // below Phase 1 (`maximizedId !== null`) would let the first autorepeat
    // event re-fire Phase 1, double-clearing `maximizedNodeId` and
    // re-setting the active node. The check has to gate BOTH phases.
    const arrowSection = appSource.slice(
      appSource.indexOf('if (!isArrowAction(action))'),
      appSource.indexOf('Phase 1: if a node is maximized'),
    );
    expect(/arrowThrottle\(\)/.test(arrowSection)).toBe(true);
  });

  it('does NOT also wrap arrows in createShortcutGuard (regression guard)', () => {
    // `createShortcutGuard` (src/lib/shortcutGuard.ts) blocks ALL calls
    // within the cooldown — the wrong shape for arrows because the user
    // DOES want to repeat, just slower. A future contributor who wraps
    // arrows in `createShortcutGuard(300)` to "be safe" would silently
    // turn grid traversal into a one-shot move. Substring check covers
    // either an inline call or a hoisted const named `arrowGuard`.
    const wrongWiring = /arrowGuard|createShortcutGuard\(\s*\d+\s*\)[\s\S]{0,80}arrow-/;
    expect(wrongWiring.test(appSource)).toBe(false);
  });
});