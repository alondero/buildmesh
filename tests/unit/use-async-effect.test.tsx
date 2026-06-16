/**
 * Tests for the useAsyncEffect helper (issue #349).
 *
 * `useAsyncEffect(fn, deps)` is a `useEffect` wrapper that hands the
 * callback an `AbortSignal`. The signal is `aborted` on cleanup (unmount
 * or dep change). Async work inside `fn` can gate setState on
 * `signal.aborted` to drop stale resolutions — replacing the hand-rolled
 * `let cancelled = false` pattern from the path-invalidated cache hooks.
 *
 * These tests pin the contract: signal is fresh per effect run, the
 * signal is aborted on every cleanup path (unmount and dep change), and
 * `signal.addEventListener('abort', cb)` works for the subscribe-effect
 * "drop a stale callback" case.
 */

import { describe, it, expect, vi } from 'vitest';
import { renderHook } from '@testing-library/react';
import { useAsyncEffect } from '../../src/hooks/useAsyncEffect';

describe('useAsyncEffect (issue #349)', () => {
  it('runs the effect on mount with a non-aborted signal', () => {
    const effect = vi.fn();
    renderHook(() => useAsyncEffect(effect));
    expect(effect).toHaveBeenCalledTimes(1);
    const signal = effect.mock.calls[0][0] as AbortSignal;
    expect(signal).toBeInstanceOf(AbortSignal);
    expect(signal.aborted).toBe(false);
  });

  it('aborts the signal on unmount', () => {
    let captured: AbortSignal | null = null;
    const { unmount } = renderHook(() =>
      useAsyncEffect((signal) => {
        captured = signal;
      }),
    );
    expect(captured).not.toBeNull();
    expect(captured!.aborted).toBe(false);
    unmount();
    expect(captured!.aborted).toBe(true);
  });

  it('aborts the previous signal when deps change and hands a fresh one to the new run', () => {
    const signals: AbortSignal[] = [];
    const { rerender } = renderHook(
      ({ dep }: { dep: number }) =>
        useAsyncEffect(
          (signal) => {
            signals.push(signal);
          },
          [dep],
        ),
      { initialProps: { dep: 1 } },
    );
    expect(signals).toHaveLength(1);
    expect(signals[0].aborted).toBe(false);

    rerender({ dep: 2 });
    expect(signals).toHaveLength(2);
    expect(signals[0].aborted).toBe(true); // previous run cleaned up
    expect(signals[1].aborted).toBe(false); // new run is fresh
  });

  it('fires the abort event listener on cleanup (subscribe-effect pattern)', () => {
    // Mirrors the usePathInvalidatedQuery subscribe effect: the effect
    // registers a bus handler whose body must short-circuit on cleanup.
    // `addEventListener('abort', ...)` is the API a subscribe-effect
    // would use if it stored the signal rather than re-reading a boolean.
    const onAbort = vi.fn();
    const { unmount } = renderHook(() =>
      useAsyncEffect((signal) => {
        signal.addEventListener('abort', onAbort);
      }),
    );
    expect(onAbort).not.toHaveBeenCalled();
    unmount();
    expect(onAbort).toHaveBeenCalledTimes(1);
  });

  it('lets async work drop a stale setState by checking signal.aborted after await', async () => {
    // This is the exact pattern the path-invalidated cache hooks use:
    //   const result = await fetchSomething();
    //   if (signal.aborted) return;
    //   setState(result);
    // The second run resolves AFTER the first run's signal is aborted.
    // Only the second run's setState must fire.
    let resolveFirst!: (v: string) => void;
    let resolveSecond!: (v: string) => void;
    const seen: string[] = [];
    const { rerender, unmount } = renderHook(
      ({ dep }: { dep: number }) =>
        useAsyncEffect(async (signal) => {
          // Stash a local copy of dep so the closure can't be confused
          // by the deps array changing.
          const myDep = dep;
          const result = await new Promise<string>((resolve) => {
            if (myDep === 1) resolveFirst = resolve;
            else resolveSecond = resolve;
          });
          if (signal.aborted) return;
          seen.push(result);
        }, [dep]),
      { initialProps: { dep: 1 } },
    );

    // First run is in flight; switch deps to start the second run.
    rerender({ dep: 2 });
    // Now resolve the FIRST run — it must be dropped because its signal
    // was aborted when deps changed.
    resolveFirst('first');
    // Let the microtask queue drain.
    await Promise.resolve();
    expect(seen).toEqual([]);

    // Resolve the SECOND run — it must land.
    resolveSecond('second');
    await Promise.resolve();
    expect(seen).toEqual(['second']);

    unmount();
  });

  it('does not block cleanup if the effect returned a still-pending Promise', () => {
    // The helper must call cleanup synchronously even if the effect
    // returned an unresolved promise. A consumer that awaits the
    // returned promise from the effect function should not need to
    // worry about that promise rejecting on its own — the helper just
    // aborts the signal and moves on.
    const effect = vi.fn(() => new Promise<void>(() => { /* never resolves */ }));
    const { unmount } = renderHook(() => useAsyncEffect(effect));
    expect(effect).toHaveBeenCalledTimes(1);
    // Synchronous unmount must not throw.
    expect(() => unmount()).not.toThrow();
  });

  it('does not re-run when deps are the same array reference across renders (default useEffect semantics)', () => {
    const effect = vi.fn();
    const { rerender } = renderHook(() => useAsyncEffect(effect, []));
    expect(effect).toHaveBeenCalledTimes(1);
    rerender();
    expect(effect).toHaveBeenCalledTimes(1); // no re-run with stable []
  });

  it('runs the returned cleanup on unmount, after aborting the signal', () => {
    // Mirrors the WS + retry-timer sites: the effect owns a long-lived
    // resource (timer, websocket) and must release it on cleanup. The
    // helper must (a) call the user's cleanup and (b) abort the signal
    // BEFORE the cleanup runs — otherwise a cleanup that re-checks
    // `signal.aborted` would race the helper's own abort.
    let signalAtCleanupTime: boolean | null = null;
    const cleanup = vi.fn(() => {
      signalAtCleanupTime = capturedSignal?.aborted ?? null;
    });
    let capturedSignal: AbortSignal | null = null;
    const { unmount } = renderHook(() =>
      useAsyncEffect((signal) => {
        capturedSignal = signal;
        return cleanup;
      }),
    );
    expect(cleanup).not.toHaveBeenCalled();
    unmount();
    expect(cleanup).toHaveBeenCalledTimes(1);
    // Signal must already be aborted when the user cleanup runs, so a
    // user cleanup that consults the signal sees the truth.
    expect(signalAtCleanupTime).toBe(true);
  });

  it('runs the returned cleanup on dep change for the PREVIOUS run only', () => {
    // The contract: a dep change re-runs the effect, but the previous
    // run's cleanup is invoked first. This is the same contract as
    // useEffect — the helper just runs `controller.abort()` first.
    const cleanups: string[] = [];
    const { rerender, unmount } = renderHook(
      ({ dep }: { dep: number }) =>
        useAsyncEffect(
          (_signal) => {
            return () => cleanups.push(`cleanup-for-${dep}`);
          },
          [dep],
        ),
      { initialProps: { dep: 1 } },
    );
    rerender({ dep: 2 });
    expect(cleanups).toEqual(['cleanup-for-1']);
    rerender({ dep: 3 });
    expect(cleanups).toEqual(['cleanup-for-1', 'cleanup-for-2']);
    unmount();
    expect(cleanups).toEqual(['cleanup-for-1', 'cleanup-for-2', 'cleanup-for-3']);
  });
});
