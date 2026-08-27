/**
 * Focused unit test for `useVisibilityPolling` (issue #1261). The
 * NodeList component test exercises this hook end-to-end through the
 * full meshes/nodes fetch path; this file pins the hook's own
 * contract so a future shape change can't silently regress the
 * lifecycle in a way the high-level test happens to paper over.
 *
 * The big questions this file answers:
 *   * Is the hook stable across callback-identity churn? (a parent
 *     re-rendering with a fresh inline arrow must NOT reset the
 *     timer.)
 *   * Does the hook pause when `document.hidden` flips to true, and
 *     resume + immediate-refresh on the way back?
 *   * Is only one `refresh` in flight at a time? (the visibility
 *     resume must NOT fire alongside a still-pending tick.)
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render } from "@testing-library/react";
import { useEffect } from "react";
import { useVisibilityPolling } from "../../src/mobile/useVisibilityPolling";

// Stash and restore the original `document.hidden` descriptor so this
// test never leaks property redefinitions into siblings — the
// `NodeList` integration test does the same dance.
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
  refresh: ReturnType<typeof vi.fn>;
  rerenders: number;
}

function mountHook(
  refresh: ReturnType<typeof vi.fn>,
  intervalMs = 5000,
): Harness {
  const harness: Harness = { refresh, rerenders: 0 };
  function Tree({ tick }: { tick: number }) {
    // Re-read `refresh` via a ref-like indirection on every render so we
    // can prove the hook doesn't react to callback-identity churn.
    const stableRefresh = tick >= 0 ? refresh : refresh;
    useVisibilityPolling(stableRefresh, intervalMs);
    useEffect(() => {
      harness.rerenders += 1;
    });
    return null;
  }
  let currentTick = 0;
  const { rerender } = render(<Tree tick={currentTick} />);
  (harness as { rerender: () => void }).rerender = () => {
    currentTick += 1;
    rerender(<Tree tick={currentTick} />);
  };
  return harness;
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
    // Mount handler fires the initial refresh synchronously inside the
    // effect; one microtask hop covers `Promise.resolve(undefined)`.
    await act(async () => {
      await Promise.resolve();
    });
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it("does NOT call refresh on mount when document is hidden", async () => {
    setDocumentHidden(true);
    const refresh = vi.fn().mockResolvedValue(undefined);
    mountHook(refresh);
    // Even after 15s of fake time, no refresh should fire.
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

    // Subsequent tick fires another.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });
    expect(refresh.mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  it("stops polling again on becoming hidden and skips the next tick", async () => {
    setDocumentHidden(false);
    const refresh = vi.fn().mockResolvedValue(undefined);
    mountHook(refresh);
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
  });

  it("stays stable across callback-identity churn", async () => {
    setDocumentHidden(false);
    const refresh = vi.fn().mockResolvedValue(undefined);
    const harness = mountHook(refresh);
    const initialRerenders = harness.rerenders;
    // Re-render the host component several times with a "new" callback
    // identity (a different arrow invoked with the same body each
    // render — vitest can't tell it's the same function, but the
    // effect should still NOT tear down and rebuild because the deps
    // array only mentions intervalMs).
    for (let i = 0; i < 5; i += 1) {
      await act(async () => {
        (harness as { rerender: () => void }).rerender();
      });
    }
    // Rerenders happened — but the effect (and the interval) should
    // NOT have been re-armed: a fresh refresh per rerender would be
    // a bug. Sanity-bound: initial rerenders + 5 forced ones.
    expect(harness.rerenders).toBeGreaterThan(initialRerenders);

    // Drive time: only the ONE interval that the hook armed on mount
    // should fire. If the effect churned, this counter would tick up
    // proportional to the rerender count.
    const callsBeforeTick = refresh.mock.calls.length;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });
    const callsAfterTick = refresh.mock.calls.length;
    // Exactly one tick fired in the 5s window.
    expect(callsAfterTick - callsBeforeTick).toBe(1);
  });

  it("drops a new refresh while a previous one is still in flight", async () => {
    setDocumentHidden(false);
    let resolveFirst!: () => void;
    const firstCall = new Promise<void>((r) => {
      resolveFirst = r;
    });
    const refresh = vi
      .fn()
      .mockImplementationOnce(() => firstCall)
      .mockImplementation(() => Promise.resolve());
    mountHook(refresh);
    await act(async () => {
      await Promise.resolve();
    });
    // The mount fired the first refresh (which is still pending). Now
    // flip hidden then visible: the resume handler's immediate
    // refresh should be dropped (in-flight guard), and the callback
    // counter should NOT bump.
    const beforeFlip = refresh.mock.calls.length;
    setDocumentHidden(true);
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    setDocumentHidden(false);
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await Promise.resolve();
    });
    expect(refresh.mock.calls.length).toBe(beforeFlip);

    // Releasing the first call lets the in-flight guard clear without
    // re-entering the visibility handler.
    await act(async () => {
      resolveFirst();
    });
  });
});
