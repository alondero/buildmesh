import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render } from '@testing-library/react';
import {
  useOpencodeAccountInvalidation,
  OPENCODE_CONSOLE_CHANGED_EVENT,
} from '../../src/hooks/useOpencodeAccountInvalidation';

const listenMock = vi.fn();

vi.mock('@tauri-apps/api/event', () => ({
  listen: (event: string, handler: (e: unknown) => void) => {
    listenMock(event, handler);
    return Promise.resolve(() => {});
  },
}));

function Harness({ refresh }: { refresh: () => void }) {
  useOpencodeAccountInvalidation(refresh);
  return null;
}

describe('useOpencodeAccountInvalidation', () => {
  beforeEach(() => {
    listenMock.mockClear();
  });

  it('subscribes to the opencode-console-changed event on mount', () => {
    const refresh = vi.fn();
    render(<Harness refresh={refresh} />);
    expect(listenMock).toHaveBeenCalledWith(
      OPENCODE_CONSOLE_CHANGED_EVENT,
      expect.any(Function),
    );
  });

  it('exposes the event name as a single source of truth (stringly-typed drift guard)', () => {
    // If a future rename happens in the backend, this constant is the
    // single grep-able symbol — guard against accidental divergence
    // from the Rust-side `OPENCODE_CONSOLE_CHANGED_EVENT` constant
    // in `services/opencode_oauth.rs`.
    expect(OPENCODE_CONSOLE_CHANGED_EVENT).toBe('opencode-console-changed');
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
    captured?.({ payload: undefined });
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it('re-subscribes when the refresh function reference changes', () => {
    // Mirrors the canonical useProviderListInvalidation pattern: an
    // unstable refresh closure would otherwise keep firing on a
    // stale snapshot. The Usage tab passes a stable `useCallback`-ed
    // refresh (see `src/components/Probe/UsageTab.tsx:159-181`).
    const refreshA = vi.fn();
    const refreshB = vi.fn();
    const { rerender } = render(<Harness refresh={refreshA} />);
    expect(listenMock).toHaveBeenCalledTimes(1);

    rerender(<Harness refresh={refreshB} />);
    expect(listenMock).toHaveBeenCalledTimes(2);
  });
});
