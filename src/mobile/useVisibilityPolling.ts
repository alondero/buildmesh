import { useEffect, useRef } from "react";

/**
 * Poll `refresh` on a `intervalMs` cadence while the document is visible.
 *
 * Issue #1261 — replaces the inline timer dance previously embedded in
 * `NodeList`'s body so the lifecycle / visibility logic lives in a
 * focused module that any other mobile screen can reuse. Mirrors
 * `useWsEvents`'s resume-on-foreground shape (issue #806): on
 * `visibilitychange` to `visible`, fire ONE refresh immediately so the
 * user sees fresh state without waiting out the next tick, then resume
 * polling; on `hidden`, just stop the interval.
 *
 * `refresh` is read through a ref so the effect doesn't re-run when
 * its callback-identity churns (a parent re-rendering with a fresh
 * inline arrow shouldn't reset polling state). The effect deps stay
 * `[intervalMs]` only.
 *
 * Concurrency: only one `refresh` may be in flight at a time. If a
 * tick fires while the previous refresh is still pending, the new tick
 * is dropped — the in-flight one wins. The same rule applies to the
 * visibility-resume refresh: a slow tick already in flight takes
 * precedence over a fresh resume. The caller's own gating
 * (`mountedRef` or equivalent) plus the next interval tick recovers
 * any drift; this avoids the "stale response overwrites fresh" race
 * that would need an AbortController passed through `listMeshes` /
 * `listNodes` to fully solve (out of scope here, deliberate).
 */
export function useVisibilityPolling(
  refresh: () => Promise<void> | void,
  intervalMs: number,
): void {
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;

  useEffect(() => {
    let timer: number | null = null;
    let inFlight = false;
    let disposed = false;

    const runOnce = async () => {
      if (disposed || inFlight) return;
      inFlight = true;
      try {
        await refreshRef.current();
      } finally {
        inFlight = false;
      }
    };

    const start = () => {
      if (timer !== null || disposed) return;
      timer = window.setInterval(runOnce, intervalMs);
    };
    const stop = () => {
      if (timer === null) return;
      window.clearInterval(timer);
      timer = null;
    };
    const onVisibility = () => {
      stop();
      if (document.hidden) return;
      void runOnce();
      start();
    };

    if (!document.hidden) {
      void runOnce();
      start();
    }
    document.addEventListener("visibilitychange", onVisibility);

    return () => {
      disposed = true;
      stop();
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [intervalMs]);
}
