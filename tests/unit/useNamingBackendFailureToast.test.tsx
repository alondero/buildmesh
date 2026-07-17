import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render } from '@testing-library/react';
import {
  useNamingBackendFailureToast,
  NAMING_BACKEND_FAILED_EVENT,
} from '../../src/hooks/useNamingBackendFailureToast';

const listenMock = vi.fn();

vi.mock('@tauri-apps/api/event', () => ({
  listen: (event: string, handler: (e: unknown) => void) => {
    listenMock(event, handler);
    return Promise.resolve(() => {});
  },
}));

function Harness({ onFailure }: { onFailure: (p: { node_id: number; reason: string }) => void }) {
  useNamingBackendFailureToast(onFailure);
  return null;
}

describe('useNamingBackendFailureToast', () => {
  beforeEach(() => {
    listenMock.mockClear();
  });

  it('subscribes to the naming-backend-failed event on mount', () => {
    const onFailure = vi.fn();
    render(<Harness onFailure={onFailure} />);
    expect(listenMock).toHaveBeenCalledWith(
      NAMING_BACKEND_FAILED_EVENT,
      expect.any(Function),
    );
  });

  // Drift guard: the backend emit (`session_naming.rs` sticky-lockout branch)
  // and this listener must agree on the event name. One grep-able symbol beats
  // a stringly-typed `'naming-backend-failed'` literal duplicated across files.
  it('exposes the event name as a single source of truth (drift guard)', () => {
    expect(NAMING_BACKEND_FAILED_EVENT).toBe('naming-backend-failed');
  });

  it('forwards the event payload to the onFailure callback', () => {
    let captured: ((e: unknown) => void) | null = null;
    listenMock.mockImplementationOnce((_event, handler) => {
      captured = handler;
      return Promise.resolve(() => {});
    });

    const onFailure = vi.fn();
    render(<Harness onFailure={onFailure} />);

    expect(onFailure).not.toHaveBeenCalled();
    captured?.({ payload: { node_id: 42, reason: 'CLI timed out after 30s' } });
    expect(onFailure).toHaveBeenCalledTimes(1);
    expect(onFailure).toHaveBeenCalledWith({ node_id: 42, reason: 'CLI timed out after 30s' });
  });

  it('re-subscribes when the onFailure callback reference changes', () => {
    // Mirrors `useProviderListInvalidation`: unstable callback identity would
    // tear down and re-attach on every parent render — keep the assertion so
    // a regression that drops the dep array fails loudly here.
    const onFailureA = vi.fn();
    const onFailureB = vi.fn();
    const { rerender } = render(<Harness onFailure={onFailureA} />);
    expect(listenMock).toHaveBeenCalledTimes(1);

    rerender(<Harness onFailure={onFailureB} />);
    expect(listenMock).toHaveBeenCalledTimes(2);
  });
});