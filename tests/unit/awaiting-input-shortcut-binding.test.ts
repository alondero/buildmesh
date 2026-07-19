import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

/**
 * Pins the Ctrl/Cmd+. binding for the "jump to next awaiting node" cycle
 * (issue #64). Same static-analysis shape as
 * `tests/unit/grid-shortcut-binding.test.ts`: Tauri's global-shortcut plugin
 * captures at the OS layer (before xterm.js sees the keydown), so this
 * binding fires even when an agent terminal has focus — exactly the state
 * the user is in when they notice a node is waiting for input.
 *
 * The accelerator string is `CommandOrControl+Period`: Tauri aliases
 * `Period` to the `.` key (via the underlying `global_hotkey` crate, which
 * also accepts the literal `.`). `CommandOrControl` resolves to Cmd on macOS
 * and Ctrl on Windows/Linux, so the same string works cross-platform.
 *
 * Match the `key:`-then-`action:` pair on the same object entry — a binding
 * string appearing in a comment or docblock doesn't satisfy the assertion.
 * This closes the coverage gap where a refactor could swap the action
 * (e.g. `Period` → `Comma`) and still pass a substring-contains check on
 * the modifier alone.
 */
describe('awaiting-input shortcut binding (issue #64)', () => {
  const appSource = readFileSync(
    resolve(__dirname, '../../src/App.tsx'),
    'utf8',
  );

  it('binds CommandOrControl+Period → action jump-to-next-awaiting', () => {
    const entry = new RegExp(
      `key:\\s*'CommandOrControl\\+Period',\\s*action:\\s*'jump-to-next-awaiting'`,
    );
    expect(entry.test(appSource)).toBe(true);
  });

  it('uses CommandOrControl (not bare Ctrl/Cmd) so the binding matches both platforms', () => {
    // Regression guard: a future change that hard-codes `Ctrl+Period` would
    // leave macOS users without a way to cycle. The accelerator must use the
    // platform-aware `CommandOrControl` combinator — the global_hotkey crate
    // (and Tauri's plugin on top) resolves it to Cmd on Mac, Ctrl elsewhere.
    const bareCtrl = /key:\s*'Ctrl\+Period'/;
    const bareCmd = /key:\s*'Cmd\+Period'/;
    expect(bareCtrl.test(appSource), 'Ctrl+Period is not cross-platform').toBe(false);
    expect(bareCmd.test(appSource), 'Cmd+Period is not cross-platform').toBe(false);
  });

  it('dispatches the action in the shortcut-triggered handler', () => {
    // The App.tsx useEffect listener must contain a branch that calls
    // jumpToNextAwaitingNode() for the action — otherwise the binding
    // registers but never fires. Pair-check (binding + dispatch) closes the
    // gap where one half of the wiring exists without the other. The 800-char
    // budget accommodates the explanatory comment block between the action
    // match and the call site; tight enough that a stale or orphaned branch
    // would still fail.
    const dispatch = new RegExp(
      `action === 'jump-to-next-awaiting'[\\s\\S]{0,800}jumpToNextAwaitingNode\\(\\)`,
    );
    expect(dispatch.test(appSource)).toBe(true);
  });
});
