import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { createKeyRepeatThrottle } from '../../src/lib/keyRepeatThrottle';

/**
 * Tests the leading-edge throttle used by the arrow-traversal handler in
 * src/App.tsx to tame the OS autorepeat flood (Windows ~30 Hz, macOS ~15 Hz).
 *
 * Behavior contract:
 *   - First call always passes (the user may have tapped the key — we can't
 *     tell from the global-shortcut plugin whether the press is a fresh one
 *     or an autorepeat, but a fresh tap MUST feel instantaneous).
 *   - Subsequent calls within `intervalMs` of the last fire are blocked.
 *   - After `intervalMs` elapses, the next call passes and resets the timer.
 *
 * `performance.now()` is faked by `vi.useFakeTimers()` (vitest 4's default
 * mock surface), so the test relies only on `vi.advanceTimersByTimeAsync`
 * to step the clock — no real-time waits.
 */
describe('createKeyRepeatThrottle', () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  it('passes the very first call', () => {
    const throttle = createKeyRepeatThrottle(200);
    expect(throttle()).toBe(true);
  });

  it('blocks calls within the interval', async () => {
    const throttle = createKeyRepeatThrottle(200);
    expect(throttle()).toBe(true);
    await vi.advanceTimersByTimeAsync(50);
    expect(throttle()).toBe(false);
    await vi.advanceTimersByTimeAsync(100);
    expect(throttle()).toBe(false);
  });

  it('passes again after the interval elapses', async () => {
    const throttle = createKeyRepeatThrottle(200);
    expect(throttle()).toBe(true);
    await vi.advanceTimersByTimeAsync(199);
    expect(throttle()).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(throttle()).toBe(true);
  });

  it('resets the interval window on each successful fire', async () => {
    // The interval is measured from the LAST fire, not the original one.
    // Walk: T=0 fires (lastFire=0); T=199 blocked (199<200, still inside the
    // first window); T=200 fires (lastFire=200); T=399 blocked (199<200,
    // inside the SECOND window); T=400 fires (lastFire=400).
    const throttle = createKeyRepeatThrottle(200);
    expect(throttle()).toBe(true);
    await vi.advanceTimersByTimeAsync(199);
    expect(throttle()).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(throttle()).toBe(true);
    await vi.advanceTimersByTimeAsync(199);
    expect(throttle()).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(throttle()).toBe(true);
  });

  it('throttles an OS-style autorepeat flood (33ms intervals) to one fire per window', async () => {
    // Mirrors the user's complaint: holding the key fires ~30 events/sec
    // and focus jumps to the end of the row. With a 200ms throttle, the
    // flood is collapsed to a single fire per window.
    const throttle = createKeyRepeatThrottle(200);
    let fires = 0;
    const tick = () => { if (throttle()) fires++; };

    tick();
    expect(fires).toBe(1);

    // Simulate 30 autorepeat events over ~1 second (~33ms apart).
    for (let i = 0; i < 30; i++) {
      await vi.advanceTimersByTimeAsync(33);
      tick();
    }

    // 30 × 33ms = 990ms total. With a 200ms throttle and one initial fire
    // at t=0, additional fires land at t≈200, 400, 600, 800 (4 more) for a
    // total of 5. The 190ms tail (t=990) is < 200ms since the last fire
    // (t=800), so it stays blocked. Bound is [5, 5] in steady state; the
    // extra slack on the upper end guards against rounding when the test
    // is later retuned.
    expect(fires).toBeGreaterThanOrEqual(5);
    expect(fires).toBeLessThanOrEqual(6);
  });

  it('fires immediately after a long gap (simulates release + re-press)', async () => {
    // User holds, releases for a couple seconds, then presses again. The
    // re-press must fire on the very first event — no "carry-over"
    // blocking from the previous hold.
    const throttle = createKeyRepeatThrottle(200);
    expect(throttle()).toBe(true);
    await vi.advanceTimersByTimeAsync(2000);
    expect(throttle()).toBe(true);
  });

  it('keeps independent state across throttle instances', async () => {
    // Two throttles don't share their last-fire timestamps. Useful when
    // multiple bindings need their own rate-limit budget (e.g. one for
    // arrow traversal, one for a hypothetical future repeat-key). Walking
    // both up and down verifies they unblock together but neither "burns"
    // the other's budget.
    const a = createKeyRepeatThrottle(200);
    const b = createKeyRepeatThrottle(200);
    expect(a()).toBe(true);
    expect(b()).toBe(true);
    expect(a()).toBe(false);
    expect(b()).toBe(false);
    await vi.advanceTimersByTimeAsync(200);
    expect(a()).toBe(true);
    expect(b()).toBe(true);
  });
});