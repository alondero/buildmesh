import { useEffect, useRef } from "react";

/**
 * Run `refresh` on a `intervalMs` cadence WHILE the document is visible
 * and the browser is online.
 *
 * ## Ownership — one chain per hook instance, disposed on unmount
 *
 * The timer, the sequence token and the `AbortController` all live in
 * the effect's closure, so each mounted screen owns exactly one chain
 * and no two instances can touch each other's timer. Cleanup (issue
 * #1576) sets `disposed` FIRST, then cancels the pending tick, then
 * aborts the in-flight request. That order matters: an aborted fetch
 * rejects immediately, and the settlement handlers below check
 * `disposed` before re-arming or reporting, so a late settle from a
 * destroyed screen can never re-arm a timer, invoke `refresh` again,
 * or call `onError` on a consumer that has already torn down.
 *
 * Every path that could start work — the timer, `visibilitychange`,
 * `online`, the initial tick — reads `disposed`, and the recurring
 * ones read `hidden` and `offline` as well, so navigating in and out
 * of a screen leaves no background polling loop behind.
 *
 * ## Lifecycle — mirror of `useWsEvents` (issue #806)
 *
 *   * On `visibilitychange` to `visible`, fire ONE refresh immediately
 *     so the user sees fresh state without waiting out the next tick,
 *     then resume the interval.
 *   * On `window.online`, fire ONE refresh — but only once the
 *     document is visible again. Network return can precede foreground
 *     (the OS may restore connectivity while the tab is still hidden),
 *     so the `online` handler re-arms the chain and the deferred
 *     request goes out on the next `visibilitychange`. Polling never
 *     issues a request the user cannot see the answer to.
 *   * On `visibilitychange` to `hidden`, or on `window.offline`, just
 *     cancel the pending tick. (The OS will suspend timers anyway, but
 *     an explicit stop keeps state clean across in-app tab switches
 *     and stops a known-dead network from being probed every tick.)
 *
 * The FIRST tick is deliberately not gated on `navigator.onLine`: it
 * is the probe that tells the screen whether the desktop is reachable
 * at all, and `onLine === false` is a hint, not proof (captive portal,
 * VPN transition, a half-restored radio). Skipping it would leave a
 * phone that launched offline showing an empty list with no error.
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
 * started, or the hook has been disposed, the older one's result is
 * dropped.
 *
 * This fixes the "stale-on-foreground" bug the previous draft had: the
 * mobile-suspend-with-hung-fetch case (OS freezes a fetch through
 * backgrounding; on resume, the OS thaws it and the slow response
 * lands minutes later with stale data). With the in-flight drop, that
 * hung fetch's setState would clobber the fresh one. With the
 * sequence token, the hung fetch's setState is silently dropped
 * because `isLatest()` returns false by the time it resolves.
 *
 * The second argument is an `AbortSignal` that fires when the hook is
 * disposed. Pass it to `fetch`-based refreshes so an unmounted screen
 * cancels its own in-flight request instead of holding the connection
 * open for a response nobody will read. The commit check stays
 * `isLatest()`: only the polling path carries a signal, and NodeList's
 * WebSocket path calls `refresh` directly with none.
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
  refresh: (isLatest: () => boolean, signal: AbortSignal) => Promise<void> | void,
  intervalMs: number,
  onError?: (error: unknown) => void,
): void {
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;

  useEffect(() => {
    // `disposed` is the instance's terminal state: once true it never
    // reverts, and every exit from this closure reads it.
    let disposed = false;
    // Seeded from the browser, not left false: a screen mounted while
    // the radio is already down never sees an `offline` event, so
    // without this it would probe a dead network every tick forever.
    let offline = typeof navigator !== "undefined" && navigator.onLine === false;
    let timeoutId: number | null = null;
    let currentToken = 0;
    const controller = new AbortController();

    const cancelPendingTick = () => {
      if (timeoutId === null) return;
      window.clearTimeout(timeoutId);
      timeoutId = null;
    };

    // Single exit for every failed tick, sync or async. Reporting is
    // gated on `disposed` because a consumer's `onError` normally
    // touches state, and an unmounted screen has no state left to set.
    const reportError = (error: unknown) => {
      if (!disposed) onErrorRef.current?.(error);
    };

    const runOne = () => {
      // A timer that survived cleanup, or a listener that was removed
      // between dispatch and delivery, must not start a request for a
      // screen that no longer exists.
      if (disposed) return;
      currentToken += 1;
      const myToken = currentToken;
      let result: Promise<void> | void;
      try {
        // refresh may be sync or async; we wrap to normalise both.
        result = refreshRef.current(
          () => !disposed && myToken === currentToken,
          controller.signal,
        );
      } catch (error) {
        // Synchronous throw from refresh — keep the loop alive, since a
        // single bad tick shouldn't brick polling.
        reportError(error);
        armNextTick();
        return;
      }
      Promise.resolve(result).then(
        () => {
          armNextTick();
        },
        (error) => {
          reportError(error);
          armNextTick();
        },
      );
    };

    const armNextTick = () => {
      if (disposed || offline || document.hidden || timeoutId !== null) return;
      timeoutId = window.setTimeout(() => {
        timeoutId = null;
        runOne();
      }, intervalMs);
    };

    const fireNowAndArmNext = () => {
      // Cancel any pending tick — we want the immediate refresh, not
      // a stale-deferred one firing microseconds later.
      cancelPendingTick();
      // Hidden or offline: stay quiet. Whichever condition clears
      // first re-enters through `onVisibility` / `onOnline`.
      if (disposed || offline || document.hidden) return;
      runOne();
    };

    const onVisibility = () => {
      // Becoming hidden: cancel the pending tick so a tick that fires
      // after backgrounding can't keep the loop alive — the OS suspends
      // timers anyway, but an explicit cancel keeps state clean across
      // in-app tab switches. Becoming visible: fire immediate refresh
      // and re-arm.
      if (document.hidden) {
        cancelPendingTick();
        return;
      }
      fireNowAndArmNext();
    };
    const onOffline = () => {
      offline = true;
      cancelPendingTick();
    };
    // Network return can race ahead of foreground resume (the OS
    // restored connectivity before the user re-focused). `fireNowAndArmNext`
    // still defers the request while the document is hidden, so the
    // single immediate refresh happens exactly once — on whichever
    // of the two conditions clears last.
    const onOnline = () => {
      offline = false;
      fireNowAndArmNext();
    };

    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("offline", onOffline);
    window.addEventListener("online", onOnline);

    if (!document.hidden) {
      runOne();
    }

    return () => {
      // Order is load-bearing: `disposed` before the abort, so the
      // rejection this triggers is recognised as ours and dropped.
      disposed = true;
      cancelPendingTick();
      controller.abort();
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("offline", onOffline);
      window.removeEventListener("online", onOnline);
    };
  }, [intervalMs]);
}
