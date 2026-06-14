import { describe, it, expect, beforeEach, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';

// Override the global window mock so this file can capture the onFocusChanged
// callback and fire a focus event on demand. (The shared setup mock resolves to
// a no-op and never invokes the callback.)
let focusCb: ((e: { payload: boolean }) => void) | null = null;
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({
    onFocusChanged: (cb: (e: { payload: boolean }) => void) => {
      focusCb = cb;
      return Promise.resolve(() => {
        focusCb = null;
      });
    },
  }),
}));

import { createPathInvalidatedCache } from '../../src/lib/pathInvalidatedCache';
import { usePathInvalidatedQuery } from '../../src/hooks/usePathInvalidatedQuery';

beforeEach(() => {
  focusCb = null;
});

describe('usePathInvalidatedQuery refetchOnFocus', () => {
  it('refetches when the window regains focus (opt-in)', async () => {
    let calls = 0;
    const client = createPathInvalidatedCache<string, number>({
      fetcher: async () => ++calls,
    });

    const { result } = renderHook(() =>
      usePathInvalidatedQuery(client, 'k', 'k', { refetchOnFocus: true }),
    );
    await waitFor(() => expect(result.current.data).toBe(1));

    // A focus listener was registered; firing it refetches.
    expect(focusCb).not.toBeNull();
    await act(async () => {
      focusCb?.({ payload: true });
    });
    await waitFor(() => expect(result.current.data).toBe(2));

    // Losing focus does NOT refetch.
    await act(async () => {
      focusCb?.({ payload: false });
    });
    await new Promise((r) => setTimeout(r, 0));
    expect(calls).toBe(2);
  });

  it('does not subscribe to focus when the option is off (default)', async () => {
    let calls = 0;
    const client = createPathInvalidatedCache<string, number>({
      fetcher: async () => ++calls,
    });

    const { result } = renderHook(() =>
      usePathInvalidatedQuery(client, 'k', 'k'),
    );
    await waitFor(() => expect(result.current.data).toBe(1));

    // No focus listener was ever registered.
    expect(focusCb).toBeNull();
    expect(calls).toBe(1);
  });
});
