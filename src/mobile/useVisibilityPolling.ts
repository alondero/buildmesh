import { useEffect, useRef } from "react";

/**
 * Run `refresh` on a `intervalMs` cadence WHILE the document is visible
 * and the browser is online.
 *
 * ## Lifecycle — mirror of `useWsEvents` (issue #806)
 *
 *   * On `visibilitychange` to `visible`, fire ONE refresh immediately
 *     so the user sees fresh state without waiting out the next tick,
 *     then resume the interval.
 *   * On `window.online`, fire ONE refresh unconditionally — network
 *     return can race ahead of foreground (the OS may have restored
 *     connectivity while the tab is still hidden), so the polling
 *     shouldn't have to wait for the user to re-focus.
 *   * On `visibilitychange` to `hidden`, just stop arming the next
 *     tick. (The OS will suspend timers anyway, but explicit stop
 *     keeps state clean across in-app tab switches.)
 *
 * ## Timing — chained `setTimeout`, never `setInterval`
 *
 * The previous draft used `setInterval(runOnce, intervalMs)`. That's
 * wrong for async polling: a slow fetch that drifts past `intervalMs`
 * either overlaps with the next tick or gets silently dropped,
 * leaving gaps. We schedule the next tick only AFTER the previous
 * refresh has settled, so each iteration is `intervalMs` APART, not
 * `intervalMs` FROM THE PREVIOUS TICK FIRE.
 *
 * ## Concurrency — sequence token, NOT in-flight drop
 *
 * Each `runOnce` invocation bumps a counter and captures its token in
 * a closure. The `refresh` callback receives an `isLatest()` predicate
 * and MUST check it before commit (setState) — if a newer refresh has
 * started, the older one's result is dropped.
 *
 * This fixes the "stale-on-foreground" bug the previous draft had: the
 * mobile-suspend-with-hung-fetch case (OS freezes a fetch through
 * backgrounding; on resume, the OS thaws it and the slow response
 * lands minutes later with stale data). With the in-flight drop, that
 * hung fetch's setState would clobber the fresh one. With the
 * sequence token, the hung fetch's setState is silently dropped
 * because `isLatest()` returns false by the time it resolves.
 *
 * The hook never itself aborts the fetch — that would require
 * plumbing `AbortSignal` through `listMeshes`/`listNodes` and the
 * `apiFetch` helper, which is out of scope for issue #1261. The
 * sequence-token approach is the lightest-weight correct fix that
 * doesn't require changes to the api surface.
 *
 * ## Errors — `onError` is REQUIRED for any non-trivial `refresh`
 *
 * Rejections are routed to `onError` (no-op if not provided). Without
 * this, a `refresh` that returns a rejecting promise would bubble up
 * to `window.onunhandledrejection` because `setTimeout`-callback
 * Promises have no implicit handler. A generic hook can't assume its
 * consumer wraps every `refresh` call in a try/catch — most won't.
 */
export function useVisibilityPolling(
  refresh: (isLatest: () => boolean) => Promise<void> | void,
  intervalMs: number,
  onError?: (error: unknown) => void,
): void {
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;

  useEffect(() => {
    let timeoutId: number | null = null;
    let currentToken = 0;

    const runOne = () => {
      currentToken += 1;
      const myToken = currentToken;
      let result: Promise<void> | void;
      try {
        // refresh may be sync or async; we wrap to normalise both.
        result = refreshRef.current(() => myToken === currentToken);
      } catch (error) {
        // Synchronous throw from refresh — route to onError and keep
        // the loop alive (a single bad tick shouldn't brick polling).
        onErrorRef.current?.(error);
        armNextTick();
        return;
      }
      Promise.resolve(result).then(
        () => {
          armNextTick();
        },
        (error) => {
          onErrorRef.current?.(error);
          armNextTick();
        },
      );
    };

    const armNextTick = () => {
      if (timeoutId !== null) return;
      timeoutId = window.setTimeout(() => {
        timeoutId = null;
        runOne();
      }, intervalMs);
    };

    const fireNowAndArmNext = () => {
      // Cancel any pending tick — we want the immediate refresh, not
      // a stale-deferred one firing microseconds later.
      if (timeoutId !== null) {
        window.clearTimeout(timeoutId);
        timeoutId = null;
      }
      runOne();
    };

    const onVisibility = () => {
      // Becoming hidden: cancel the pending tick so a tick that fires
      // after backgrounding can't keep the loop alive — the OS suspends
      // timers anyway, but an explicit cancel keeps state clean across
      // in-app tab switches. Becoming visible: fire immediate refresh
      // and re-arm.
      if (document.hidden) {
        if (timeoutId !== null) {
          window.clearTimeout(timeoutId);
          timeoutId = null;
        }
        return;
      }
      fireNowAndArmNext();
    };
    // `online` fires unconditionally — network return can race ahead
    // of foreground resume (the OS restored connectivity before the
    // user re-focused). Same precedent as `useWsEvents`.
    const onOnline = () => fireNowAndArmNext();

    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("online", onOnline);

    if (!document.hidden) {
      runOne();
    }

    return () => {
      if (timeoutId !== null) {
        window.clearTimeout(timeoutId);
        timeoutId = null;
      }
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("online", onOnline);
    };
  }, [intervalMs]);
}
