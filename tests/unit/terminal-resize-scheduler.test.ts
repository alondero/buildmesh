import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  TerminalResizeScheduler,
  TERMINAL_RESIZE_QUIET_MS,
} from '../../src/components/Terminal/TerminalResizeScheduler';

type TestObserver = {
  trigger: () => void;
};

const observers: TestObserver[] = [];
const originalResizeObserver = globalThis.ResizeObserver;

class MockResizeObserver {
  private readonly callback: ResizeObserverCallback;

  constructor(callback: ResizeObserverCallback) {
    this.callback = callback;
    observers.push({
      trigger: () => this.callback([], this as unknown as ResizeObserver),
    });
  }

  observe(): void {}
  disconnect(): void {}
}

describe('TerminalResizeScheduler', () => {
  let frames: Array<() => void>;

  beforeEach(() => {
    vi.useFakeTimers();
    globalThis.ResizeObserver = MockResizeObserver as unknown as typeof ResizeObserver;
    frames = [];
    observers.length = 0;
  });

  afterEach(() => {
    vi.useRealTimers();
    globalThis.ResizeObserver = originalResizeObserver;
  });

  it('coalesces a burst and performs measurement on the next animation frame', () => {
    const fit = vi.fn();
    const scheduler = new TerminalResizeScheduler(fit, (callback) => frames.push(callback));
    scheduler.attach(document.createElement('div'));

    observers[0].trigger();
    vi.advanceTimersByTime(TERMINAL_RESIZE_QUIET_MS - 1);
    observers[0].trigger();
    vi.advanceTimersByTime(TERMINAL_RESIZE_QUIET_MS - 1);

    expect(fit).not.toHaveBeenCalled();
    expect(frames).toHaveLength(0);

    vi.advanceTimersByTime(1);
    expect(fit).not.toHaveBeenCalled();
    expect(frames).toHaveLength(1);

    frames.shift()!();
    expect(fit).toHaveBeenCalledTimes(1);
  });

  it('waits for a long drag to settle before fitting', () => {
    const fit = vi.fn();
    const scheduler = new TerminalResizeScheduler(fit, (callback) => frames.push(callback));
    scheduler.attach(document.createElement('div'));

    observers[0].trigger();
    const notificationInterval = TERMINAL_RESIZE_QUIET_MS - 1;
    for (
      let elapsed = 0;
      elapsed < TERMINAL_RESIZE_QUIET_MS * 3;
      elapsed += notificationInterval
    ) {
      vi.advanceTimersByTime(notificationInterval);
      observers[0].trigger();
    }

    expect(frames).toHaveLength(0);
    expect(fit).not.toHaveBeenCalled();

    vi.advanceTimersByTime(TERMINAL_RESIZE_QUIET_MS - 1);
    expect(frames).toHaveLength(0);
    vi.advanceTimersByTime(1);
    expect(frames).toHaveLength(1);

    frames.shift()!();
    expect(fit).toHaveBeenCalledTimes(1);
  });

  it('does not fit after detaching, including a frame already queued by the timer', () => {
    const fit = vi.fn();
    const scheduler = new TerminalResizeScheduler(fit, (callback) => frames.push(callback));
    scheduler.attach(document.createElement('div'));
    observers[0].trigger();

    vi.advanceTimersByTime(TERMINAL_RESIZE_QUIET_MS);
    scheduler.detach();
    frames.shift()!();

    expect(fit).not.toHaveBeenCalled();
  });

  it('invalidates a queued frame when an attachment is replaced', () => {
    const fit = vi.fn();
    const scheduler = new TerminalResizeScheduler(fit, (callback) => frames.push(callback));
    scheduler.attach(document.createElement('div'));
    scheduler.fitNextFrame();
    scheduler.attach(document.createElement('div'));
    scheduler.fitNextFrame();

    frames.shift()!();
    expect(fit).not.toHaveBeenCalled();
    frames.shift()!();
    expect(fit).toHaveBeenCalledTimes(1);
  });
});
