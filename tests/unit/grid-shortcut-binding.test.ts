import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

/**
 * Pins the modifier combo bound to the four arrow-traversal actions in
 * src/App.tsx. The previous binding, `CommandOrControl+Arrow*`, was captured
 * at the OS layer by Tauri's global-shortcut plugin and stole the readline
 * `backward-word` / `forward-word` gesture (Ctrl+←/→ in bash, zsh, fish,
 * PSReadLine, python/node REPLs, the Claude prompt, etc.) — every time the
 * user tried to fix a typo in the middle of a long agent prompt while a
 * terminal had focus, the keystroke moved the on-screen grid instead.
 *
 * The replacement adds a second modifier: `CommandOrControl+Alt+Arrow*`.
 * No readline or PTY gesture uses two modifiers, so the PTY still receives
 * bare Ctrl+Arrow for word movement. Same gesture as Windows Snap, tmux
 * `prefix+Arrow`, i3/bspwm, VS Code's "Move Editor Group".
 *
 * Static-analysis shape (matches the regression test for #688 — `CommandExt::
 * creation_flags` is setter-only and can't be asserted at runtime, so the
 * test pins the source literal directly). The binding string is a single
 * inline literal in `src/App.tsx`; reading the file is the cheapest way to
 * keep it honest without mocking the global-shortcut plugin.
 */
describe('grid traversal shortcut binding', () => {
  const appSource = readFileSync(
    resolve(__dirname, '../../src/App.tsx'),
    'utf8',
  );

  const ARROWS = ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'] as const;
  // Each entry pairs the key glyph with the action string the dispatch
  // listener at `src/App.tsx:212` keys off of. Asserting the pair (not just
  // the binding string) closes the coverage gap where a refactor could swap
  // the actions (e.g. ArrowLeft → 'arrow-right') and still pass a
  // substring-contains check on the modifier alone.
  const WIRING = [
    ['ArrowLeft', 'arrow-left'],
    ['ArrowRight', 'arrow-right'],
    ['ArrowUp', 'arrow-up'],
    ['ArrowDown', 'arrow-down'],
  ] as const;

  it.each(WIRING)('binds Ctrl/Cmd+Alt+%s → action %s', (arrow, action) => {
    // Match the `key:`-then-`action:` pair on the same object entry, so a
    // binding string appearing in a comment or docblock doesn't satisfy
    // the assertion.
    const entry = new RegExp(
      `key:\\s*'CommandOrControl\\+Alt\\+${arrow}',\\s*action:\\s*'${action}'`,
    );
    expect(entry.test(appSource)).toBe(true);
  });

  it('does NOT bind the bare CommandOrControl+Arrow* form', () => {
    // Regression guard: a future change that drops the `Alt+` would re-steal
    // readline word-movement. Match the array-entry shape, not the substring,
    // so `CommandOrControl+Alt+ArrowLeft` doesn't false-positive.
    const bareForm = /key:\s*'CommandOrControl\+Arrow(Left|Right|Up|Down)'/;
    expect(bareForm.test(appSource)).toBe(false);
  });
});