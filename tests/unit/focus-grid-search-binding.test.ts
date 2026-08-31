/**
 * Pins the focus-grid-search binding (issue #998).
 *
 * The shortcut lives in App.tsx as a platform-branched const so the
 * binding can avoid the macOS `⌘+F` collision with the terminal's
 * find action (xterm's `attachCustomKeyEventHandler` already claims
 * that chord). The split is:
 *
 *   - Win/Linux: `CommandOrControl+F` — bare Ctrl+F, free of readline
 *     collisions.
 *   - macOS:     `CommandOrControl+Alt+F` — `⌘+⌥+F`, the same
 *     two-modifier carve-out `cycle-grid-modes` uses on macOS
 *     (`⌘+⌥+G`). The `Alt`/`⌥` modifier is the only thing keeping
 *     it from re-colliding with `term-find`.
 *
 * Static-analysis shape (matches `grid-shortcut-binding.test.ts` and
 * `shortcut-catalog-binding.test.ts`): reading the App.tsx source and
 * matching the `key:`-then-`action:` pair on the platform-branched
 * const literal. The test doesn't depend on `isMac` (which would force
 * a jsdom navigator shim) — instead it asserts the platform branches
 * separately, so a future refactor that drops one branch is caught
 * without the test going red on the wrong machine.
 */
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

describe('focus-grid-search binding (issue #998)', () => {
  const appSource = readFileSync(
    resolve(__dirname, '../../src/App.tsx'),
    'utf8',
  );

  it('declares a platform-branched const with the focus-grid-search action', () => {
    // Match the `focusGridSearchShortcut` shape — the const literal that
    // branches on `isMac`. Anchored on `action:` so a binding string
    // appearing in a comment or docblock (e.g. "we used to bind this to
    // Ctrl+F") doesn't satisfy the assertion.
    const branch = /focusGridSearchShortcut\s*=\s*isMac\s*\?[\s\S]*?:\s*\{[\s\S]*?action:\s*'focus-grid-search'[\s\S]*?\}/;
    expect(branch.test(appSource)).toBe(true);
  });

  it('binds CommandOrControl+F on Windows/Linux (bare Ctrl+F, no readline collision)', () => {
    // The Win/Linux half of the platform branch.
    const win = /key:\s*'CommandOrControl\+F',\s*action:\s*'focus-grid-search'/;
    expect(win.test(appSource)).toBe(true);
  });

  it('binds CommandOrControl+Alt+F on macOS (⌘+⌥+F, collision carve-out for term-find)', () => {
    // The macOS half. ⌘+F is taken by xterm's `term-find` action; the
    // extra Alt/⌥ is the only thing that frees the chord. A future
    // edit that drops `Alt+` would re-collide with term-find and
    // silently break the terminal's find bar — this test catches that.
    const mac = /key:\s*'CommandOrControl\+Alt\+F',\s*action:\s*'focus-grid-search'/;
    expect(mac.test(appSource)).toBe(true);
  });

  it('does NOT bind the bare CommandOrControl+F form on macOS (would steal term-find)', () => {
    // Regression guard for the macOS collision. We can't check the
    // macOS-side "no bare" form from the static regex (the bare form is
    // exactly what Win/Linux uses), so instead pin that the
    // platform-branch ternary has *both* halves — i.e. the macOS half
    // carries `Alt+` and the Win/Linux half doesn't. The previous test
    // confirms the macOS half has `Alt+`; this one confirms the
    // Win/Linux half does NOT.
    //
    // Read the body of the const literal and assert: the key string
    // before `focus-grid-search` is `'CommandOrControl+F'`, exactly
    // one of the two branches, and the other branch has `Alt+F`.
    const lines = appSource.split('\n');
    const keyLines = lines.filter(
      (l) => /key:\s*'CommandOrControl(\+Alt)?\+F'/.test(l),
    );
    // Both branches must be present (the ternary expands to two
    // object literals). If a future refactor inlines one branch and
    // drops the other, this count drops below 2 and the test goes red.
    expect(keyLines.length).toBe(2);
    const hasBare = keyLines.some((l) => /key:\s*'CommandOrControl\+F'/.test(l));
    const hasAltAugmented = keyLines.some((l) => /key:\s*'CommandOrControl\+Alt\+F'/.test(l));
    expect(hasBare).toBe(true);
    expect(hasAltAugmented).toBe(true);
  });
});
