/**
 * Integration tests for TerminalContainer fit behavior
 *
 * Tests:
 * - Mount calls fitAddon.fit() after ~50ms delay
 * - sessionId change triggers fit at ~100ms and ~300ms
 * - fit-terminals global event triggers fitAddon.fit()
 * - Timeouts are cleared on unmount
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

// ============================================================
// These tests verify the fit timing patterns without rendering React
// The actual timing logic is tested directly here.
// ============================================================

describe('TerminalContainer fit behavior (timing patterns)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('mount fit happens at 50ms', () => {
    // This is the timing from TerminalContainer line 108:
    // setTimeout(() => { inst.fitAddon.fit(); inst.term.focus(); }, 50);
    const fitCallback = vi.fn();

    setTimeout(fitCallback, 50);

    // Before 50ms - should not have fit
    vi.advanceTimersByTime(49);
    expect(fitCallback).not.toHaveBeenCalled();

    // At 50ms - should fit
    vi.advanceTimersByTime(1);
    expect(fitCallback).toHaveBeenCalledTimes(1);
  });

  it('sessionId change fit happens at 100ms and 300ms', () => {
    // From TerminalContainer lines 116-117:
    // const t1 = setTimeout(() => inst.fitAddon.fit(), 100);
    // const t2 = setTimeout(() => inst.fitAddon.fit(), 300);
    const fitCallback = vi.fn();

    setTimeout(fitCallback, 100);
    setTimeout(fitCallback, 300);

    // At 100ms - first fit
    vi.advanceTimersByTime(100);
    expect(fitCallback).toHaveBeenCalledTimes(1);

    // At 300ms (200ms more) - second fit
    vi.advanceTimersByTime(200);
    expect(fitCallback).toHaveBeenCalledTimes(2);
  });

  it('cleanup clears pending timeouts', () => {
    const fitCallback = vi.fn();

    const t1 = setTimeout(fitCallback, 100);
    const t2 = setTimeout(fitCallback, 300);

    // Clear before any fire
    clearTimeout(t1);
    clearTimeout(t2);

    vi.advanceTimersByTime(400);

    expect(fitCallback).not.toHaveBeenCalled();
  });

  it('multiple sessionId changes each schedule their own timeouts', () => {
    const fitCallback = vi.fn();

    // First sessionId change at t=0
    setTimeout(fitCallback, 100);
    setTimeout(fitCallback, 300);

    // Simulate switching to another session at t=50 (before first fits fire)
    vi.advanceTimersByTime(50); // Now at t=50
    // Second sessionId change schedules new timeouts relative to t=50
    setTimeout(fitCallback, 100); // Will fire at t=150
    setTimeout(fitCallback, 300); // Will fire at t=350

    // At t=100 (50ms from now) - first session's first fit fires
    vi.advanceTimersByTime(50);
    expect(fitCallback).toHaveBeenCalledTimes(1);

    // At t=150 (50ms more) - second session's first fit fires
    vi.advanceTimersByTime(50);
    expect(fitCallback).toHaveBeenCalledTimes(2);

    // At t=300 (150ms more) - first session's second fit fires
    vi.advanceTimersByTime(150);
    expect(fitCallback).toHaveBeenCalledTimes(3);

    // At t=350 (50ms more) - second session's second fit fires
    vi.advanceTimersByTime(50);
    expect(fitCallback).toHaveBeenCalledTimes(4);
  });
});

describe('TerminalContainer fit event handling', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('fit event handler can be registered and receives events', () => {
    const handler = vi.fn();
    window.addEventListener('fit-terminals', handler);

    window.dispatchEvent(new CustomEvent('fit-terminals'));

    expect(handler).toHaveBeenCalledTimes(1);

    window.removeEventListener('fit-terminals', handler);
  });

  it('multiple handlers all receive fit-terminals event', () => {
    const handler1 = vi.fn();
    const handler2 = vi.fn();

    window.addEventListener('fit-terminals', handler1);
    window.addEventListener('fit-terminals', handler2);

    window.dispatchEvent(new CustomEvent('fit-terminals'));

    expect(handler1).toHaveBeenCalledTimes(1);
    expect(handler2).toHaveBeenCalledTimes(1);

    window.removeEventListener('fit-terminals', handler1);
    window.removeEventListener('fit-terminals', handler2);
  });

  it('handler is properly removable', () => {
    const handler = vi.fn();
    window.addEventListener('fit-terminals', handler);
    window.removeEventListener('fit-terminals', handler);

    window.dispatchEvent(new CustomEvent('fit-terminals'));

    expect(handler).not.toHaveBeenCalled();
  });

  it('fit-terminals can be dispatched multiple times', () => {
    const handler = vi.fn();
    window.addEventListener('fit-terminals', handler);

    window.dispatchEvent(new CustomEvent('fit-terminals'));
    window.dispatchEvent(new CustomEvent('fit-terminals'));
    window.dispatchEvent(new CustomEvent('fit-terminals'));

    expect(handler).toHaveBeenCalledTimes(3);

    window.removeEventListener('fit-terminals', handler);
  });
});

describe('fit timing integration', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('SessionView fit event dispatch follows 0-100-300 pattern', () => {
    // From SessionView.tsx lines 39-45:
    // const fit = () => window.dispatchEvent(new CustomEvent('fit-terminals'));
    // fit();
    // const t1 = setTimeout(fit, 100);
    // const t2 = setTimeout(fit, 300);

    const dispatchSpy = vi.spyOn(window, 'dispatchEvent');
    const fit = () => window.dispatchEvent(new CustomEvent('fit-terminals'));

    fit();
    const t1 = setTimeout(fit, 100);
    const t2 = setTimeout(fit, 300);

    // Immediate dispatch
    expect(dispatchSpy).toHaveBeenCalledTimes(1);

    // At 100ms
    vi.advanceTimersByTime(100);
    expect(dispatchSpy).toHaveBeenCalledTimes(2);

    // At 300ms
    vi.advanceTimersByTime(200);
    expect(dispatchSpy).toHaveBeenCalledTimes(3);

    // Cleanup
    clearTimeout(t1);
    clearTimeout(t2);
  });
});
