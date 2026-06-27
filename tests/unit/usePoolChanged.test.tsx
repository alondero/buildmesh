import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render } from '@testing-library/react';
import {
  usePoolChanged,
  POOL_COUNT_CHANGED_EVENT,
} from '../../src/hooks/usePoolChanged';

const listenMock = vi.fn();

vi.mock('@tauri-apps/api/event', () => ({
  listen: (event: string, handler: (e: unknown) => void) => {
    listenMock(event, handler);
    return Promise.resolve(() => {});
  },
}));

function Harness({ refresh }: { refresh: () => void }) {
  usePoolChanged(refresh);
  return null;
}

describe('usePoolChanged', () => {
  beforeEach(() => {
    listenMock.mockClear();
  });

  it('subscribes to the pool-count-changed event on mount', () => {
    const refresh = vi.fn();
    render(<Harness refresh={refresh} />);
    expect(listenMock).toHaveBeenCalledWith(
      POOL_COUNT_CHANGED_EVENT,
      expect.any(Function),
    );
  });

  it('exposes the event name as a single source of truth (stringly-typed drift guard)', () => {
    // Symmetric to the Rust `pool_count_changed_event_name_matches_frontend_constant`
    // test — keeps the two halves of the IPC contract from drifting.
    // A rename here MUST be matched in src-tauri/src/services/warm_pool.rs.
    expect(POOL_COUNT_CHANGED_EVENT).toBe('pool-count-changed');
  });

  it('calls refresh when the event handler fires', () => {
    let captured: ((e: unknown) => void) | null = null;
    listenMock.mockImplementationOnce((_event, handler) => {
      captured = handler;
      return Promise.resolve(() => {});
    });

    const refresh = vi.fn();
    render(<Harness refresh={refresh} />);

    expect(refresh).not.toHaveBeenCalled();
    // Simulate the backend firing the event (payload is mesh_id — currently
    // ignored by the badge, but the hook must still fire refresh on it).
    captured?.({ payload: 42 });
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it('re-subscribes when the refresh function reference changes', () => {
    // Different refresh identities should tear down the old listener and
    // attach a new one — otherwise an unstable closure would re-fire on
    // a stale snapshot (the same invariant `useProviderListInvalidation`
    // enforces).
    const refreshA = vi.fn();
    const refreshB = vi.fn();
    const { rerender } = render(<Harness refresh={refreshA} />);
    expect(listenMock).toHaveBeenCalledTimes(1);

    rerender(<Harness refresh={refreshB} />);
    expect(listenMock).toHaveBeenCalledTimes(2);
  });
});
