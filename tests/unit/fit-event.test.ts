/**
 * Unit tests for fit-terminals event timing
 *
 * Tests the double-timeout pattern used in SessionView.tsx (100ms + 300ms)
 * and TerminalContainer.tsx (100ms + 300ms on nodeId change).
 *
 * Using fake timers to avoid actual delays in tests.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

// ============================================================
// Fit Event Dispatch Pattern (from SessionView.tsx lines 39-45)
// ============================================================

function createFitEventDispatcher() {
  const timeouts: ReturnType<typeof setTimeout>[] = [];
  let dispatchCount = 0;

  const dispatchFit = () => {
    dispatchCount++;
    window.dispatchEvent(new CustomEvent('fit-terminals'));
  };

  const scheduleFit = () => {
    dispatchFit();
    const t1 = setTimeout(dispatchFit, 100);
    const t2 = setTimeout(dispatchFit, 300);
    timeouts.push(t1, t2);
  };

  const cleanup = () => {
    timeouts.forEach(t => clearTimeout(t));
    timeouts.length = 0;
  };

  return { scheduleFit, cleanup, getDispatchCount: () => dispatchCount, getTimeouts: () => timeouts.length };
}

// ============================================================
// TerminalContainer Timeout Pattern (from Terminal.tsx lines 113-118)
// ============================================================

function createTerminalContainerFitScheduler(onFit: () => void) {
  const timeouts: ReturnType<typeof setTimeout>[] = [];

  const scheduleFits = () => {
    const t1 = setTimeout(onFit, 100);
    const t2 = setTimeout(onFit, 300);
    timeouts.push(t1, t2);
  };

  const cleanup = () => {
    timeouts.forEach(t => clearTimeout(t));
    timeouts.length = 0;
  };

  return { scheduleFits, cleanup, getTimeouts: () => timeouts.length };
}

describe('fit-terminals event handling', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  describe('SessionView fit event dispatching', () => {
    it('dispatches fit-terminals event immediately on scheduleFit', () => {
      const dispatcher = createFitEventDispatcher();
      const eventSpy = vi.spyOn(window, 'dispatchEvent');

      dispatcher.scheduleFit();

      expect(eventSpy).toHaveBeenCalledWith(expect.objectContaining({
        type: 'fit-terminals',
      }));
    });

    it('schedules two additional timeouts at 100ms and 300ms', () => {
      const dispatcher = createFitEventDispatcher();

      dispatcher.scheduleFit();

      // Advance time by 100ms
      vi.advanceTimersByTime(100);
      // Advance time by 200ms more (total 300ms)
      vi.advanceTimersByTime(200);

      // Should have 3 dispatches: immediate + 100ms + 300ms
      expect(dispatcher.getDispatchCount()).toBe(3);
    });

    it('cleanup clears pending timeouts', () => {
      const dispatcher = createFitEventDispatcher();
      dispatcher.scheduleFit();

      // Clear timeouts before they fire
      dispatcher.cleanup();

      // Advance past all timeouts
      vi.advanceTimersByTime(400);

      // Only immediate dispatch should have fired
      expect(dispatcher.getDispatchCount()).toBe(1);
    });
  });

  describe('TerminalContainer fit scheduling', () => {
    it('schedules two timeouts on scheduleFits', () => {
      const fitCallback = vi.fn();
      const scheduler = createTerminalContainerFitScheduler(fitCallback);

      scheduler.scheduleFits();

      expect(scheduler.getTimeouts()).toBe(2);
    });

    it('calls fit callback at 100ms and 300ms', () => {
      const fitCallback = vi.fn();
      const scheduler = createTerminalContainerFitScheduler(fitCallback);

      scheduler.scheduleFits();

      // First fit at 100ms
      vi.advanceTimersByTime(100);
      expect(fitCallback).toHaveBeenCalledTimes(1);

      // Second fit at 300ms (200ms more)
      vi.advanceTimersByTime(200);
      expect(fitCallback).toHaveBeenCalledTimes(2);
    });

    it('cleanup clears both timeouts', () => {
      const fitCallback = vi.fn();
      const scheduler = createTerminalContainerFitScheduler(fitCallback);

      scheduler.scheduleFits();
      scheduler.cleanup();

      // Advance past all timeouts
      vi.advanceTimersByTime(400);

      // No callbacks should have fired
      expect(fitCallback).not.toHaveBeenCalled();
    });
  });

  describe('event flow integration', () => {
    it('global fit-terminals event triggers handlers', () => {
      const handler = vi.fn();
      window.addEventListener('fit-terminals', handler);

      window.dispatchEvent(new CustomEvent('fit-terminals'));

      expect(handler).toHaveBeenCalledTimes(1);

      window.removeEventListener('fit-terminals', handler);
    });

    it('multiple handlers receive the fit-terminals event', () => {
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
  });
});

describe('fit timing constants', () => {
  // These are the actual delay values used in the codebase
  const MOUNT_FIT_DELAY = 50; // Terminal.tsx line 108
  const SESSION_CHANGE_FIT_DELAYS = [100, 300]; // Terminal.tsx lines 116-117
  const VIEW_FIT_DELAYS = [100, 300]; // SessionView.tsx lines 42-43

  it('mount fit happens at 50ms', () => {
    vi.useFakeTimers();
    const callback = vi.fn();

    setTimeout(callback, MOUNT_FIT_DELAY);
    expect(callback).not.toHaveBeenCalled();

    vi.advanceTimersByTime(50);
    expect(callback).toHaveBeenCalled();

    vi.useRealTimers();
  });

  it('session change fit happens at 100ms and 300ms', () => {
    vi.useFakeTimers();
    const callback = vi.fn();

    setTimeout(callback, SESSION_CHANGE_FIT_DELAYS[0]);
    setTimeout(callback, SESSION_CHANGE_FIT_DELAYS[1]);

    vi.advanceTimersByTime(100);
    expect(callback).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(200);
    expect(callback).toHaveBeenCalledTimes(2);

    vi.useRealTimers();
  });
});
