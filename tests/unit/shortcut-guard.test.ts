import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { createShortcutGuard } from '../../src/lib/shortcutGuard';

describe('createShortcutGuard', () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  it('executes the first call immediately', () => {
    const guard = createShortcutGuard(300);
    const action = vi.fn(() => Promise.resolve());
    guard(action);
    expect(action).toHaveBeenCalledTimes(1);
  });

  it('blocks subsequent calls within cooldown', async () => {
    const guard = createShortcutGuard(300);
    const action = vi.fn(() => Promise.resolve());
    guard(action);
    await vi.advanceTimersByTimeAsync(0);
    guard(action);
    guard(action);
    expect(action).toHaveBeenCalledTimes(1);
  });

  it('unblocks after cooldown elapses', async () => {
    const guard = createShortcutGuard(300);
    const action = vi.fn(() => Promise.resolve());
    guard(action);
    await vi.advanceTimersByTimeAsync(300);
    guard(action);
    expect(action).toHaveBeenCalledTimes(2);
  });

  it('stays blocked while action is pending even after cooldown', async () => {
    const guard = createShortcutGuard(100);
    let resolve!: () => void;
    const slow = vi.fn(() => new Promise<void>(r => { resolve = r; }));
    guard(slow);
    await vi.advanceTimersByTimeAsync(200);
    guard(slow);
    expect(slow).toHaveBeenCalledTimes(1);
    resolve();
    await vi.advanceTimersByTimeAsync(0);
    guard(slow);
    expect(slow).toHaveBeenCalledTimes(2);
  });
});
