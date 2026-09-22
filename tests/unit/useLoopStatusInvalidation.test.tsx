import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render } from '@testing-library/react';
import {
  useLoopStatusInvalidation,
  LOOP_STATUS_INVALIDATION_EVENTS,
  LOOP_STATUS_FALLBACK_MS,
} from '../../src/hooks/useLoopStatusInvalidation';

const listenMock = vi.fn();

vi.mock('@tauri-apps/api/event', () => ({
  listen: (event: string, handler: (e: unknown) => void) => {
    listenMock(event, handler);
    return Promise.resolve(() => {});
  },
}));

function Harness({ refresh, enabled = true }: { refresh: () => void; enabled?: boolean }) {
  useLoopStatusInvalidation(refresh, enabled);
  return null;
}

describe('useLoopStatusInvalidation (issue #1751)', () => {
  beforeEach(() => {
    listenMock.mockClear();
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('subscribes to every loop-status lifecycle event on mount', () => {
    const refresh = vi.fn();
    render(<Harness refresh={refresh} />);
    expect(listenMock).toHaveBeenCalledTimes(LOOP_STATUS_INVALIDATION_EVENTS.length);
    for (const name of LOOP_STATUS_INVALIDATION_EVENTS) {
      expect(listenMock).toHaveBeenCalledWith(name, expect.any(Function));
    }
  });

  it('covers the spawn/submit/finish/close transitions that move the loop ledger', () => {
    expect(LOOP_STATUS_INVALIDATION_EVENTS).toEqual(
      expect.arrayContaining([
        'agent-lifecycle',
        'node-created',
        'autopilot-submitted',
        'autopilot-finishing',
        'autopilot-pr-created',
        'autopilot-finish-failed',
        'autopilot-node-closed',
      ]),
    );
  });

  it('keeps the slow stale-while-revalidate fallback in the 30-60s band', () => {
    expect(LOOP_STATUS_FALLBACK_MS).toBeGreaterThanOrEqual(30_000);
    expect(LOOP_STATUS_FALLBACK_MS).toBeLessThanOrEqual(60_000);
  });

  it('calls refresh (debounced) when an event fires', () => {
    const handlers = new Map<string, (e: unknown) => void>();
    listenMock.mockImplementation((event: string, handler: (e: unknown) => void) => {
      handlers.set(event, handler);
      return Promise.resolve(() => {});
    });

    const refresh = vi.fn();
    render(<Harness refresh={refresh} />);

    expect(refresh).not.toHaveBeenCalled();
    // Simulate the backend firing one of the lifecycle events.
    handlers.get('autopilot-submitted')?.({ payload: { node_id: 7, issue: 42 } });
    // Debounced: not yet.
    expect(refresh).not.toHaveBeenCalled();
    vi.advanceTimersByTime(200);
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it('coalesces an event burst into a single refresh', () => {
    const handlers = new Map<string, (e: unknown) => void>();
    listenMock.mockImplementation((event: string, handler: (e: unknown) => void) => {
      handlers.set(event, handler);
      return Promise.resolve(() => {});
    });

    const refresh = vi.fn();
    render(<Harness refresh={refresh} />);

    // A spawn storm: created + spawn-completed + lifecycle in one tick.
    handlers.get('node-created')?.({ payload: { id: 9 } });
    handlers.get('node-spawn-completed')?.({ payload: { node_id: 9 } });
    handlers.get('agent-lifecycle')?.({ payload: { session_id: 9 } });
    vi.advanceTimersByTime(200);
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it('attaches nothing while disabled (issue-driven mode issues no loop IPC)', () => {
    const refresh = vi.fn();
    render(<Harness refresh={refresh} enabled={false} />);
    expect(listenMock).not.toHaveBeenCalled();
    expect(refresh).not.toHaveBeenCalled();
  });

  it('subscribes when toggled from disabled to enabled', () => {
    const refresh = vi.fn();
    const { rerender } = render(<Harness refresh={refresh} enabled={false} />);
    expect(listenMock).not.toHaveBeenCalled();

    rerender(<Harness refresh={refresh} enabled />);
    expect(listenMock).toHaveBeenCalledTimes(LOOP_STATUS_INVALIDATION_EVENTS.length);
  });

  it('drops a pending debounced refresh on unmount (no orphaned timer)', () => {
    const handlers = new Map<string, (e: unknown) => void>();
    listenMock.mockImplementation((event: string, handler: (e: unknown) => void) => {
      handlers.set(event, handler);
      return Promise.resolve(() => {});
    });

    const refresh = vi.fn();
    const { unmount } = render(<Harness refresh={refresh} />);
    handlers.get('autopilot-blocked')?.({ payload: { node_id: 3, issue: 1 } });
    unmount();
    vi.advanceTimersByTime(1000);
    expect(refresh).not.toHaveBeenCalled();
  });
});
