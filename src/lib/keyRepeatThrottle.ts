/**
 * Rate-limit invocations fired by holding a key down (OS autorepeat).
 *
 * The Tauri global-shortcut plugin invokes its registered callback on every
 * keydown, including the autorepeat flood the OS emits after ~500 ms of
 * holding (~30 Hz on Windows, ~15 Hz on macOS). For shortcut gestures that
 * have a *repeat* semantic — e.g. Ctrl/Cmd+Alt+Arrow grid traversal — the
 * flood makes held-key behaviour unusable: focus jumps straight to the end
 * of the row before the user can lift their finger.
 *
 * Unlike `createShortcutGuard` (src/lib/shortcutGuard.ts), which blocks ALL
 * invocations within the cooldown and is the right shape for one-shot
 * toggles like Alt+G, this throttle ALLOWS the first call to pass and then
 * rate-limits subsequent ones to `intervalMs`. The first call must always
 * pass because the caller may have tapped the key (no autorepeat flood
 * coming) and we want the move to feel instantaneous.
 *
 * Pure: state lives in the closure, time comes from `performance.now()`
 * (monotonic, unjittered by wall-clock changes). Returned function is safe
 * to call from any context that has access to the binding it was created
 * for — usually a single useRef inside the same effect that owns the
 * global-shortcut handler.
 */
export function createKeyRepeatThrottle(intervalMs: number): () => boolean {
  // -Infinity sentinel: a plain `0` would compute `now - 0 < intervalMs`
  // and silently drop the user's first press if `performance.now()` is
  // still below `intervalMs` at the moment they hit the key (the webview
  // is fresh and the clock hasn't accumulated enough yet). `-Infinity`
  // guarantees the leading call passes regardless of clock state. The
  // vitest fake-timer case (where Date/performance both start at 0)
  // happens to exercise the same edge in tests, which is why the suite
  // catches a regression to `0`.
  let lastFireMs = -Infinity;
  return () => {
    const now = performance.now();
    if (now - lastFireMs < intervalMs) return false;
    lastFireMs = now;
    return true;
  };
}