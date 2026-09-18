/**
 * useWindowFocused — drives the caption buttons' *inactive* state (ADR-0035),
 * the fifth of Microsoft's caption-button states and the cue that tells you
 * which window is taking your keystrokes.
 *
 * Pins the two paths that matter: the initial query (an app launched behind
 * another window never receives a focus-change event, so it has to ask), and
 * the OS-driven change (alt-tab, taskbar click). It also pins the deliberate
 * failure behaviour — defaulting to *focused* — because an unlit caption strip
 * on a focused window is a worse lie than a missed dim.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { renderHook, act } from '@testing-library/react';

const windowApi = vi.hoisted(() => ({
  isFocused: vi.fn(),
  onFocusChanged: vi.fn<
    (cb: (event: { payload: boolean }) => void) => Promise<() => void>
  >(),
}));

vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => windowApi }));

import { useWindowFocused } from '../../src/hooks/useWindowFocused';

let focusHandler: ((event: { payload: boolean }) => void) | null = null;

/** Fire a focus-change event at the registered handler. */
function emitFocus(focused: boolean) {
  act(() => {
    focusHandler!({ payload: focused });
  });
}

beforeEach(() => {
  focusHandler = null;
  windowApi.isFocused.mockReset().mockResolvedValue(true);
  windowApi.onFocusChanged.mockReset().mockImplementation(
    (cb: (event: { payload: boolean }) => void) => {
      focusHandler = cb;
      return Promise.resolve(() => {});
    },
  );
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('useWindowFocused', () => {
  it('asks for the current state instead of assuming focus', async () => {
    // Launched behind another window: no change event is ever delivered, so a
    // hook that only listened would stay lit forever.
    windowApi.isFocused.mockResolvedValue(false);
    const { result } = renderHook(() => useWindowFocused());
    await act(async () => {});

    expect(result.current).toBe(false);
  });

  it('starts optimistic until the query resolves', async () => {
    const { result } = renderHook(() => useWindowFocused());
    expect(result.current).toBe(true);
    await act(async () => {});
  });

  it('follows OS-driven focus changes', async () => {
    const { result } = renderHook(() => useWindowFocused());
    await act(async () => {});
    expect(result.current).toBe(true);

    emitFocus(false);
    expect(result.current).toBe(false);

    emitFocus(true);
    expect(result.current).toBe(true);
  });

  it('unsubscribes on unmount and ignores a late event', async () => {
    const unlisten = vi.fn();
    windowApi.onFocusChanged.mockImplementation((cb: (event: { payload: boolean }) => void) => {
      focusHandler = cb;
      return Promise.resolve(unlisten);
    });
    const { result, unmount } = renderHook(() => useWindowFocused());
    await act(async () => {});

    unmount();
    expect(unlisten).toHaveBeenCalledTimes(1);

    // The handler closure outlives the effect, so it must re-check `disposed` —
    // otherwise React warns about updating an unmounted component.
    emitFocus(false);
    expect(result.current).toBe(true);
  });

  it('unsubscribes even when unmount lands while the subscription is still pending', async () => {
    // Subscribing is an awaited call, so unmount can land between the call and
    // its resolution — by which point the cleanup has already run with nothing
    // to tear down. Without a `disposed` re-check after that await the listener
    // outlives the component forever, and StrictMode's mount → cleanup → mount
    // makes the interleaving routine rather than exotic.
    const unlisten = vi.fn();
    let resolveSubscription: ((stop: () => void) => void) | null = null;
    windowApi.onFocusChanged.mockImplementation(
      () =>
        new Promise<() => void>((resolve) => {
          resolveSubscription = resolve;
        }),
    );

    const { unmount } = renderHook(() => useWindowFocused());
    // `isFocused` has resolved and `onFocusChanged` is now in flight.
    await act(async () => {});

    unmount();
    // Nothing to call yet — the cleanup could not have known about the listener.
    expect(unlisten).not.toHaveBeenCalled();

    // The subscription lands after the component is gone and must be torn down
    // immediately, not merely ignored.
    await act(async () => { resolveSubscription!(unlisten); });
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it('stays lit when the query fails rather than dimming on an error', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    windowApi.isFocused.mockRejectedValue(new Error('focus unavailable'));
    const { result } = renderHook(() => useWindowFocused());
    await act(async () => {});

    expect(result.current).toBe(true);
    expect(warn).toHaveBeenCalled();
  });
});
