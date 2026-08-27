/**
 * Focused unit test for `useVisibilityPolling` (issue #1261). The
 * NodeList integration test exercises this hook end-to-end through
 * the meshes/nodes fetch path; this file pins the hook's own
 * contract so a future shape change can't silently regress the
 * lifecycle in a way the high-level test happens to paper over.
 *
 * Contract pinned here:
 *   * Initial mount while visible: refresh is called once.
 *   * Mount while hidden: refresh is NOT called.
 *   * `visibilitychange` to visible: refresh is called once
 *     immediately, then the interval resumes.
 *   * `visibilitychange` to hidden: the pending tick is cancelled
 *     and no further refresh fires while hidden.
 *   * `window.online`: refresh is called (network return can race
 *     ahead of foreground resume — mirrors `useWsEvents`).
 *   * Callback-identity churn does NOT tear down / re-arm the
 *     interval. The previous version of this test passed the same
 *     `refresh` reference across renders, which proved nothing; here
 *     each render passes a fresh inline arrow (`() => refresh()`)
 *     so the hook is forced to consider whether to re-arm.
 *   * Rejections from `refresh` are routed to `onError`, NOT
 *     propagated as unhandled promise rejections. A bare
 *     `setTimeout`-callback Promise has no implicit handler.
 *   * The sequence token: when a newer refresh starts while the old
 *     is in flight, the older one's `isLatest()` predicate returns
 *     false — the older refresh's caller MUST check this and bail
 *     before setState (NodeList does). This is what stops a
 *     hung-during-mobile-suspend fetch from clobbering fresh data
 *     minutes later.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, type RenderResult } from "@testing-library/react";
import { useEffect } from "react";
import { useVisibilityPolling } from "../../src/mobile/useVisibilityPolling";

const originalHiddenDescriptor = Object.getOwnPropertyDescriptor(
  document,
  "hidden",
);

function setDocumentHidden(value: boolean) {
  Object.defineProperty(document, "hidden", {
    configurable: true,
    get: () => value,
  });
}

function restoreDocumentHidden() {
  if (originalHiddenDescriptor) {
    Object.defineProperty(document, "hidden", originalHiddenDescriptor);
  } else {
    delete (document as { hidden?: unknown }).hidden;
  }
}

interface Harness {
  refresh: (isLatest: () => boolean) => Promise<void> | void;
  rerender: () => void;
  rerenders: number;
}

/**
 * Mounts the hook and returns controls. `refreshProp` is the underlying
 * `vi.fn`-backed refresh — the host Tree passes a fresh inline arrow
 * `() => refreshProp(...)` so every render creates a NEW callback
 * identity. That's the case the previous version of this file missed:
 * passing the same `refresh` reference across renders proved nothing
 * about callback-churn tolerance.
 */
function mountHook(
  refreshProp: ReturnType<typeof vi.fn>,
  opts?: { intervalMs?: number; onError?: (e: unknown) => void },
): Harness & { getRerender: () => () => void } {
  const harness: Harness = {
    refresh: refreshProp as unknown as Harness["refresh"],
    rerender: () => {},
    rerenders: 0,
  };
  let tick = 0;

  function Tree() {
    // Inline arrow — a NEW function reference every render. This is
    // exactly what a consumer who passes `(isLatest) => doStuff(...)`
    // to the hook will produce.
    useVisibilityPolling(
      (isLatest) => refreshProp(isLatest),
      opts?.intervalMs ?? 5000,
      opts?.onError,
    );
    useEffect(() => {
      harness.rerenders += 1;
    });
    return null;
  }

  let rendered: RenderResult | null = null;
  act(() => {
    rendered = render(<Tree />);
  });

  harness.rerender = () => {
    tick += 1;
    // NO `key` change — we WANT React to preserve the component
    // instance across rerenders so the hook's effect stays mounted.
    // A `key` change would force unmount + remount, which would re-arm
    // the interval — exactly what we're trying to prove DOESN'T happen.
    act(() => {
      rendered!.rerender(<Tree />);
    });
  };

  return Object.assign(harness, { getRerender: () => harness.rerender });
}

describe("useVisibilityPolling", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    restoreDocumentHidden();
  });

  it("calls refresh on mount when document is visible", async () => {
    setDocumentHidden(false);
    const refresh = vi.fn().mockResolvedValue(undefined);
    mountHook(refresh);
    // Initial refresh fires synchronously inside the effect; one
    // microtask hop covers the `Promise.resolve(undefined)`.
    await act(async () => {
      await Promise.resolve();
    });
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it("does NOT call refresh on mount when document is hidden", async () => {
    setDocumentHidden(true);
    const refresh = vi.fn().mockResolvedValue(undefined);
    mountHook(refresh);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(15000);
    });
    expect(refresh).not.toHaveBeenCalled();
  });

  it("fires once immediately on becoming visible, then resumes polling", async () => {
    setDocumentHidden(true);
    const refresh = vi.fn().mockResolvedValue(undefined);
    mountHook(refresh);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(15000);
    });
    expect(refresh).not.toHaveBeenCalled();

    setDocumentHidden(false);
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await Promise.resolve();
    });
    expect(refresh).toHaveBeenCalledTimes(1);

    // Subsequent tick fires another refresh.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });
    expect(refresh.mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  it("stops polling again on becoming hidden and skips the next tick", async () => {
    setDocumentHidden(false);
    const refresh = vi.fn().mockResolvedValue(undefined);
    const harness = mountHook(refresh);
    await act(async () => {
      await Promise.resolve();
    });
    const beforeHide = refresh.mock.calls.length;

    setDocumentHidden(true);
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await Promise.resolve();
    });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(15000);
    });
    expect(refresh).toHaveBeenCalledTimes(beforeHide);
    // sanity: harness was kept around so we can rerender etc.
    expect(harness.rerenders).toBeGreaterThan(0);
  });

  it("fires refresh on the online event (network-return race)", async () => {
    // Network return can race ahead of foreground resume (the OS may
    // restore connectivity before the user re-focuses). Mirror the
    // `useWsEvents` precedent — `online` triggers a refresh
    // unconditionally.
    setDocumentHidden(false);
    const refresh = vi.fn().mockResolvedValue(undefined);
    mountHook(refresh);
    await act(async () => {
      await Promise.resolve();
    });
    expect(refresh).toHaveBeenCalledTimes(1);

    await act(async () => {
      window.dispatchEvent(new Event("online"));
      await Promise.resolve();
    });
    expect(refresh.mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  it("stays stable across callback-identity churn (real new closure per render)", async () => {
    // The previous version of this test passed the SAME `refresh`
    // reference across renders, which proved nothing — the hook could
    // have been re-arming on every render and the test wouldn't have
    // noticed. Here every render passes a fresh inline arrow (see
    // mountHook's Tree), so the only thing keeping the interval
    // alive is the `[intervalMs]`-only dep array.
    setDocumentHidden(false);
    const refresh = vi.fn().mockResolvedValue(undefined);
    const harness = mountHook(refresh);
    await act(async () => {
      await Promise.resolve();
    });
    const initialRerenders = harness.rerenders;

    for (let i = 0; i < 5; i += 1) {
      harness.rerender();
    }
    expect(harness.rerenders).toBeGreaterThan(initialRerenders);

    // Advance 5s — exactly ONE tick should fire. If the effect churned
    // on each rerender, this counter would tick up proportional to
    // the rerender count.
    const beforeTick = refresh.mock.calls.length;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });
    const afterTick = refresh.mock.calls.length;
    expect(afterTick - beforeTick).toBe(1);
  });

  it("routes refresh rejections to onError (no unhandledrejection)", async () => {
    // A bare `setTimeout`-callback Promise has no implicit handler, so
    // a rejecting refresh would bubble to window.onunhandledrejection
    // without this. Track that too — vi's unhandled-rejection
    // listener should NOT fire across the test.
    setDocumentHidden(false);
    const rejection = new Error("network down");
    const refresh = vi.fn().mockRejectedValue(rejection);
    const onError = vi.fn();
    const unhandled = vi.fn();
    const onUnhandled = (e: PromiseRejectionEvent) => {
      unhandled(e.reason);
      e.preventDefault();
    };
    window.addEventListener("unhandledrejection", onUnhandled);

    try {
      mountHook(refresh, { onError });
      await act(async () => {
        await Promise.resolve();
      });
      expect(onError).toHaveBeenCalledWith(rejection);
      expect(unhandled).not.toHaveBeenCalled();
    } finally {
      window.removeEventListener("unhandledrejection", onUnhandled);
    }
  });

  it("the sequence token: old refresh's isLatest() returns false once a newer one starts", async () => {
    // This is the hung-during-mobile-suspend case. Refresh1 hangs,
    // user foregrounds → Refresh2 starts. Refresh2's
    // `isLatest() === true`; Refresh1's `isLatest() === false`.
    // Caller (NodeList) checks `isLatest()` before setState and
    // bails — the old fetch's eventual resolution is dropped, no
    // stale-data clobber.
    setDocumentHidden(false);
    let firstResolve!: () => void;
    const firstCall = new Promise<void>((r) => {
      firstResolve = r;
    });
    const captured: Array<() => boolean> = [];
    const refresh = vi.fn().mockImplementation((isLatest) => {
      captured.push(isLatest);
      return captured.length === 1 ? firstCall : Promise.resolve();
    });

    mountHook(refresh);
    await act(async () => {
      await Promise.resolve();
    });

    expect(captured.length).toBeGreaterThanOrEqual(1);
    const firstIsLatest = captured[0];
    expect(firstIsLatest()).toBe(true);

    // Fire another refresh via online — increments the token.
    await act(async () => {
      window.dispatchEvent(new Event("online"));
      await Promise.resolve();
    });

    expect(captured.length).toBeGreaterThanOrEqual(2);
    // The OLD refresh's predicate now reports stale.
    expect(firstIsLatest()).toBe(false);
    // The NEW refresh's predicate reports current.
    expect(captured[captured.length - 1]()).toBe(true);

    // Cleanup: resolve the hung promise so the test doesn't leak.
    await act(async () => {
      firstResolve();
    });
  });
});
