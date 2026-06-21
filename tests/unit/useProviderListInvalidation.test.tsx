import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render } from '@testing-library/react';
import { useProviderListInvalidation, PROVIDER_LIST_CHANGED_EVENT } from '../../src/hooks/useProviderListInvalidation';

const listenMock = vi.fn();
const emitMock = vi.fn();

vi.mock('@tauri-apps/api/event', () => ({
  listen: (event: string, handler: (e: unknown) => void) => {
    listenMock(event, handler);
    return Promise.resolve(() => {});
  },
}));

function Harness({ refresh }: { refresh: () => void }) {
  useProviderListInvalidation(refresh);
  return null;
}

describe('useProviderListInvalidation', () => {
  beforeEach(() => {
    listenMock.mockClear();
  });

  it('subscribes to the provider-list-changed event on mount', () => {
    const refresh = vi.fn();
    render(<Harness refresh={refresh} />);
    expect(listenMock).toHaveBeenCalledWith(PROVIDER_LIST_CHANGED_EVENT, expect.any(Function));
  });

  it('exposes the event name as a single source of truth (stringly-typed drift guard)', () => {
    // If a future rename happens in the backend, this constant is the
    // single grep-able symbol — guard against accidental divergence.
    expect(PROVIDER_LIST_CHANGED_EVENT).toBe('provider-list-changed');
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
    // Simulate the backend firing the event.
    captured?.({ payload: undefined });
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it('re-subscribes when the refresh function reference changes', () => {
    // Different refresh identities should tear down the old listener and
    // attach a new one — otherwise an unstable closure would re-fire on
    // a stale snapshot.
    const refreshA = vi.fn();
    const refreshB = vi.fn();
    const { rerender } = render(<Harness refresh={refreshA} />);
    expect(listenMock).toHaveBeenCalledTimes(1);

    rerender(<Harness refresh={refreshB} />);
    expect(listenMock).toHaveBeenCalledTimes(2);
  });
});
