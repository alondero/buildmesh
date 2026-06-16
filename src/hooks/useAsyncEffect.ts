/**
 * useEffect with a built-in AbortSignal (issue #349).
 *
 * `useAsyncEffect(fn, deps)` is a `useEffect` wrapper that hands the
 * callback an `AbortSignal`. The signal is `aborted` on cleanup (unmount
 * or dep change). Async work inside `fn` can gate setState on
 * `signal.aborted` to drop stale resolutions, replacing the hand-rolled
 * `let cancelled = false` pattern.
 *
 * The callback may return a cleanup function (typed as `() => void`) for
 * symmetric teardown pairs — the helper aborts the signal first, then
 * runs the returned cleanup. The signal is also `EventTarget`-shaped, so
 * `signal.addEventListener('abort', cb)` works for the subscribe-effect
 * pattern in `usePathInvalidatedQuery`, where the bus callback's body
 * short-circuits if the effect has been cleaned up.
 *
 * If you're adding a new useEffect that awaits a fetch/IPC and
 * setStates the result, prefer this helper. If your effect owns a
 * long-lived non-React resource (websocket, xterm, RAF loop,
 * ResizeObserver) the cleanup must release, return a cleanup function
 * from the effect — the helper runs it after aborting the signal. The
 * `cancelled` flag belongs to the helper in that case.
 *
 * For the full migration list (per-file), see `git log --oneline --grep
 * '#349'` or the PR that closed the issue.
 */

import { DependencyList, useEffect } from 'react';

/**
 * `useAsyncEffect`'s callback may return:
 * - `void` (or `undefined`) — no cleanup needed beyond aborting the signal
 * - a cleanup function — runs after the next effect or on unmount
 * - a `Promise<void>` — ignored by the helper (callers check `signal.aborted` after `await`)
 * - a cleanup function wrapped in a Promise — runtime, the cleanup is invoked only when the
 *   function returns synchronously. We type this as the union of the synchronous shapes.
 */
export type AsyncEffectCallback = (signal: AbortSignal) => void | (() => void);

/**
 * Runs `effect(signal)` on mount and on every `deps` change. Aborts the
 * previous signal on cleanup so async work in the prior run can detect
 * the cleanup and skip its setState.
 *
 * Mirrors `useEffect`'s signature: `deps` is optional and uses
 * referential equality to decide whether to re-run. The callback may
 * return a cleanup function (typed as `() => void`) for symmetric
 * subscribe/unsubscribe pairs; the helper aborts the signal before
 * running that cleanup.
 */
export function useAsyncEffect(effect: AsyncEffectCallback, deps?: DependencyList): void {
  useEffect(() => {
    const controller = new AbortController();
    const result = effect(controller.signal);
    // `result` is typed `void | (() => void)`. If a caller accidentally
    // returns a Promise (e.g. an `async` callback whose cleanup happens
    // inside the promise body), treat it as "no cleanup" rather than
    // blowing up at unmount with "cleanup is not a function". The
    // signal abort is the actual contract; the returned cleanup is a
    // convenience for symmetric subscribe/unsubscribe pairs.
    const cleanup = typeof result === 'function' ? result : undefined;
    return () => {
      controller.abort();
      cleanup?.();
    };
  }, deps);
}
