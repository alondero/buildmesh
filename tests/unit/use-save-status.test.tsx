/**
 * `useSaveStatus` hook tests — issue #729.
 *
 * The hook's contract is small and the surface is shared across at least
 * two probe tabs (Scratchpad and MeshProperties), so the state machine
 * gets its own suite rather than relying on consumer-tab tests to pin
 * each branch. That keeps a regression on, say, the saved→idle auto-
 * clear timer from forcing the author to dig through `ScratchpadTab`
 * to understand which transition broke.
 *
 * Test mechanics
 * --------------
 * The hook returns an object of bound methods; we render it through a
 * tiny `<Harness>` component so we can drive transitions in `act()`
 * and assert on the visible state via a sentinel `<span>` that mirrors
 * each piece of state. Fake timers cover the `savedClearMs` auto-clear
 * path so the test doesn't `await new Promise(r => setTimeout(r, 1500))`.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, act, cleanup, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useSaveStatus, type SaveStatus } from '../../src/hooks/useSaveStatus';

interface HarnessProps {
  savedClearMs?: number;
}

function Harness({ savedClearMs }: HarnessProps) {
  const { status, error, start, success, fail, reset } = useSaveStatus({ savedClearMs });
  return (
    <div>
      <span data-testid="status">{status}</span>
      <span data-testid="error">{error ?? ''}</span>
      <button type="button" onClick={() => start()} data-testid="start">
        start
      </button>
      <button type="button" onClick={() => success()} data-testid="success">
        success
      </button>
      <button type="button" onClick={() => fail(new Error('boom'))} data-testid="fail-err">
        fail-err
      </button>
      <button type="button" onClick={() => fail('string-only')} data-testid="fail-str">
        fail-str
      </button>
      <button type="button" onClick={() => fail({ code: 'X' })} data-testid="fail-obj">
        fail-obj
      </button>
      <button type="button" onClick={() => reset()} data-testid="reset">
        reset
      </button>
    </div>
  );
}

async function renderHarness(opts: HarnessProps = {}) {
  render(<Harness {...opts} />);
  return {
    status: () => screen.getByTestId('status').textContent as SaveStatus,
    error: () => screen.getByTestId('error').textContent ?? '',
  };
}

function getStatus(): SaveStatus {
  return screen.getByTestId('status').textContent as SaveStatus;
}

function getError(): string {
  return screen.getByTestId('error').textContent ?? '';
}

beforeEach(() => {
  // Default to real timers; individual tests opt into fake timers.
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe('useSaveStatus — state machine (issue #729)', () => {
  it('starts in the `idle` state with no error', async () => {
    await renderHarness();
    expect(getStatus()).toBe('idle');
    expect(getError()).toBe('');
  });

  it('transitions idle → saving → saved on a successful save', async () => {
    const u = userEvent.setup();
    render(<Harness />);

    expect(getStatus()).toBe('idle');
    await u.click(screen.getByTestId('start'));
    expect(getStatus()).toBe('saving');
    await u.click(screen.getByTestId('success'));
    expect(getStatus()).toBe('saved');
    expect(getError()).toBe('');
  });

  it('transitions idle → saving → error on a rejected save', async () => {
    const u = userEvent.setup();
    render(<Harness />);

    await u.click(screen.getByTestId('start'));
    await u.click(screen.getByTestId('fail-err'));
    expect(getStatus()).toBe('error');
    expect(getError()).toBe('boom');
  });

  it('coerces non-Error rejections to a string in `error`', async () => {
    const u = userEvent.setup();
    render(<Harness />);

    await u.click(screen.getByTestId('start'));
    await u.click(screen.getByTestId('fail-str'));
    expect(getError()).toBe('string-only');

    await u.click(screen.getByTestId('start'));
    await u.click(screen.getByTestId('fail-obj'));
    // Objects stringify via `String({...})` to "[object Object]" —
    // pin the exact shape so a future "use JSON.stringify" tweak is
    // forced through this test, not silently shipped.
    expect(getError()).toBe('[object Object]');
  });

  it('clears prior `saved` when a new save starts', async () => {
    const u = userEvent.setup();
    render(<Harness />);

    await u.click(screen.getByTestId('start'));
    await u.click(screen.getByTestId('success'));
    expect(getStatus()).toBe('saved');

    await u.click(screen.getByTestId('start'));
    expect(getStatus()).toBe('saving');
    // No leftover error from the previous (successful) save.
    expect(getError()).toBe('');
  });

  it('clears prior `error` when a new save starts', async () => {
    const u = userEvent.setup();
    render(<Harness />);

    await u.click(screen.getByTestId('start'));
    await u.click(screen.getByTestId('fail-err'));
    expect(getStatus()).toBe('error');
    expect(getError()).toBe('boom');

    await u.click(screen.getByTestId('start'));
    expect(getStatus()).toBe('saving');
    expect(getError()).toBe('');
  });

  it('auto-clears `saved` → `idle` after `savedClearMs`', async () => {
    vi.useFakeTimers();
    render(<Harness savedClearMs={500} />);

    // `fireEvent` is synchronous and doesn't schedule its own timers,
    // so it composes cleanly with `vi.useFakeTimers()`. `userEvent`
    // would hang here for the same reason it does in
    // `scratchpad-tab.test.tsx`.
    fireEvent.click(screen.getByTestId('start'));
    fireEvent.click(screen.getByTestId('success'));
    expect(getStatus()).toBe('saved');

    // Advance past the 500ms auto-clear. The hook's setTimeout callback
    // fires the setState, which needs to flush through React's act()
    // boundary so the test sees the new state synchronously.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(600);
    });
    expect(getStatus()).toBe('idle');
  });

  it('does NOT auto-clear when a new save has already started (race guard)', async () => {
    vi.useFakeTimers();
    render(<Harness savedClearMs={500} />);

    fireEvent.click(screen.getByTestId('start'));
    fireEvent.click(screen.getByTestId('success'));

    // Mid-debounce, kick off a new save.
    fireEvent.click(screen.getByTestId('start'));
    expect(getStatus()).toBe('saving');

    // If the original 500ms timer fired now, it would naïvely call
    // setStatus('idle') and undo the new save. The hook's guard
    // `(s) => s === 'saved' ? 'idle' : s` keeps it in `saving`.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(600);
    });
    expect(getStatus()).toBe('saving');
  });

  it('cancels a pending auto-clear when `reset()` is called explicitly', async () => {
    vi.useFakeTimers();
    render(<Harness savedClearMs={500} />);

    fireEvent.click(screen.getByTestId('start'));
    fireEvent.click(screen.getByTestId('success'));
    fireEvent.click(screen.getByTestId('reset'));
    expect(getStatus()).toBe('idle');

    await act(async () => {
      await vi.advanceTimersByTimeAsync(600);
    });
    // No late-firing setStatus — the timer was cancelled.
    expect(getStatus()).toBe('idle');
  });

  it('disabled auto-clear (`savedClearMs={0}`) sticks in `saved` indefinitely', async () => {
    vi.useFakeTimers();
    render(<Harness savedClearMs={0} />);

    fireEvent.click(screen.getByTestId('start'));
    fireEvent.click(screen.getByTestId('success'));
    expect(getStatus()).toBe('saved');

    await act(async () => {
      await vi.advanceTimersByTimeAsync(60_000);
    });
    expect(getStatus()).toBe('saved');
  });

  it('cancels the auto-clear on unmount (no late setState)', async () => {
    vi.useFakeTimers();

    // Render with a separate dispose path so we can unmount directly.
    const { unmount } = render(<Harness savedClearMs={500} />);

    fireEvent.click(screen.getByTestId('start'));
    fireEvent.click(screen.getByTestId('success'));
    expect(getStatus()).toBe('saved');
    unmount();

    // Advance well past the auto-clear. If the cleanup effect didn't
    // fire `clearTimeout`, React 18+ would log a "setState on
    // unmounted component" warning in dev (visible in test output via
    // the jsdom error handler). The test's job is to not throw AND to
    // leave no warning behind.
    const warnings: string[] = [];
    const origConsoleError = console.error;
    console.error = (...args: unknown[]) => {
      warnings.push(args.map(String).join(' '));
    };
    try {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(2_000);
      });
    } finally {
      console.error = origConsoleError;
    }
    // Filter to React's "memory leak" / "state update on unmounted"
    // warnings — unrelated console.error calls (e.g., the IPC layer
    // we'd never hit in this hook test) must not count.
    const reactWarnings = warnings.filter((w) =>
      /unmounted|memory leak|cannot perform a React state update/i.test(w),
    );
    expect(reactWarnings).toEqual([]);
  });
});
