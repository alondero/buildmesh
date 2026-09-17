/**
 * useWindowControlOverlay — the Windows-only bridge between the bespoke title
 * bar's maximise button and the native child window that makes the shell offer
 * Snap Layouts (ADR-0035).
 *
 * Pins the three things the native side depends on: the button's real box is
 * measured and reported in right-inset form (logical pixels — the backend
 * applies the window DPI scale, which the frontend cannot see), the overlay's
 * events map onto hover/press, and a click routes through the *same*
 * `onToggleMaximize` the DOM handler calls. That last one matters: the overlay
 * swallows the mouse click, and it must not become a second writer of window
 * state — the maximise glyph still follows the `onResized` re-query alone.
 *
 * The hook is inert off Windows, which is what keeps the default
 * `title-bar.test.tsx` (jsdom, `isWindows` false) on the plain DOM path.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { renderHook, act } from '@testing-library/react';

const overlay = vi.hoisted(() => {
  const handlers = new Map<string, (event: unknown) => void>();
  return {
    handlers,
    listen: vi.fn(async (event: string, handler: (event: unknown) => void) => {
      handlers.set(event, handler);
      return () => {
        handlers.delete(event);
      };
    }),
  };
});

const reportMetrics = vi.hoisted(() => ({
  set: vi.fn<() => Promise<void>>().mockResolvedValue(undefined),
}));

// Mutable so one test can flip the platform and prove the hook stays inert off
// Windows without needing a second file; the hook reads `isWindows` when its
// effect runs, not at import time.
const platform = vi.hoisted(() => ({ isWindows: true, isMac: false }));

vi.mock('@tauri-apps/api/event', () => ({ listen: overlay.listen }));
vi.mock('../../src/lib/tauri', () => ({ setTitlebarMaximizeMetrics: reportMetrics.set }));
vi.mock('../../src/lib/platform', () => ({
  get isWindows() {
    return platform.isWindows;
  },
  get isMac() {
    return platform.isMac;
  },
}));

import {
  useWindowControlOverlay,
  WINDOW_CONTROL_OVERLAY_EVENTS,
} from '../../src/hooks/useWindowControlOverlay';

/** A button whose measured box is the maximise button of a 1280px-wide window:
    46px wide, sitting 46px in from the right edge (Close is to its right). */
function maximizeButton(box: Partial<DOMRect> = {}): HTMLButtonElement {
  const button = document.createElement('button');
  const rect = {
    left: 1188,
    right: 1234,
    top: 0,
    bottom: 32,
    width: 46,
    height: 32,
    x: 1188,
    y: 0,
    toJSON: () => ({}),
    ...box,
  } as DOMRect;
  button.getBoundingClientRect = () => rect;
  document.body.appendChild(button);
  return button;
}

/** A button plus the stable ref the hook expects, matching how TitleBar wires
    it (`useRef`, so the identity is fixed for the component's life). A fresh
    object literal per render would rebuild the native listeners every render —
    which is exactly what the hook's dependency on the ref object prevents, and
    why these tests must not pass one. */
function maximizeTarget(box: Partial<DOMRect> = {}) {
  const button = maximizeButton(box);
  return { button, ref: { current: button } };
}

/** Fire one of the overlay's events. The handlers ignore the payload, so an
    empty object keeps the intent obvious. */
function emit(event: string) {
  act(() => {
    overlay.handlers.get(event)!({});
  });
}

beforeEach(() => {
  overlay.handlers.clear();
  overlay.listen.mockClear();
  reportMetrics.set.mockClear();
  platform.isWindows = true;
  // The hook measures the inset against the root element's right edge (a
  // fractional-safe stand-in for the viewport width), which jsdom leaves at all
  // zeroes. Give it a 1280px viewport to match the button box below.
  document.documentElement.getBoundingClientRect = () =>
    ({
      left: 0,
      right: 1280,
      top: 0,
      bottom: 800,
      width: 1280,
      height: 800,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    }) as DOMRect;
  // Run the measure frame synchronously so assertions don't race a real rAF.
  vi.spyOn(window, 'requestAnimationFrame').mockImplementation((cb: FrameRequestCallback) => {
    cb(0);
    return 1;
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  document.body.innerHTML = '';
});

describe('useWindowControlOverlay', () => {
  it('reports the maximise button box as a right inset so the overlay lands on it', async () => {
    const { ref } = maximizeTarget();
    renderHook(() => useWindowControlOverlay(ref, () => {}));
    await act(async () => {});

    // viewport right edge (1280) − rect.right (1234) = 46. An inset, not an
    // absolute x, so the backend can follow a live resize without a round trip.
    expect(reportMetrics.set).toHaveBeenCalledWith({
      rightInset: 46,
      top: 0,
      width: 46,
      height: 32,
    });
  });

  it('subscribes to every overlay event', async () => {
    const { ref } = maximizeTarget();
    renderHook(() => useWindowControlOverlay(ref, () => {}));
    await act(async () => {});

    expect([...overlay.handlers.keys()].sort()).toEqual(
      Object.values(WINDOW_CONTROL_OVERLAY_EVENTS).sort(),
    );
  });

  it('drives hover and press from the overlay, which owns the mouse', async () => {
    const { ref } = maximizeTarget();
    const { result } = renderHook(() => useWindowControlOverlay(ref, () => {}));
    await act(async () => {});
    expect(result.current).toEqual({ hovered: false, pressed: false });

    emit(WINDOW_CONTROL_OVERLAY_EVENTS.hover);
    expect(result.current.hovered).toBe(true);

    emit(WINDOW_CONTROL_OVERLAY_EVENTS.press);
    expect(result.current.pressed).toBe(true);

    emit(WINDOW_CONTROL_OVERLAY_EVENTS.release);
    expect(result.current.pressed).toBe(false);
    // Release does not end the hover — the pointer is still on the button.
    expect(result.current.hovered).toBe(true);

    emit(WINDOW_CONTROL_OVERLAY_EVENTS.leave);
    expect(result.current).toEqual({ hovered: false, pressed: false });
  });

  it('clears a press that never got its release when the pointer leaves', async () => {
    const { ref } = maximizeTarget();
    const { result } = renderHook(() => useWindowControlOverlay(ref, () => {}));
    await act(async () => {});

    emit(WINDOW_CONTROL_OVERLAY_EVENTS.hover);
    emit(WINDOW_CONTROL_OVERLAY_EVENTS.press);
    expect(result.current.pressed).toBe(true);

    // Tabbing away or dragging off must not strand the button in its
    // pressed fill.
    emit(WINDOW_CONTROL_OVERLAY_EVENTS.leave);
    expect(result.current).toEqual({ hovered: false, pressed: false });
  });

  it('routes an overlay click through onToggleMaximize instead of owning window state', async () => {
    const { ref } = maximizeTarget();
    const toggle = vi.fn();
    renderHook(() => useWindowControlOverlay(ref, toggle));
    await act(async () => {});

    expect(toggle).not.toHaveBeenCalled();
    emit(WINDOW_CONTROL_OVERLAY_EVENTS.click);
    expect(toggle).toHaveBeenCalledTimes(1);
  });

  it('uses the latest toggle after a re-render without re-subscribing', async () => {
    const { ref } = maximizeTarget();
    const first = vi.fn();
    const second = vi.fn();
    const { rerender } = renderHook(
      ({ toggle }: { toggle: () => void }) => useWindowControlOverlay(ref, toggle),
      { initialProps: { toggle: first } },
    );
    await act(async () => {});

    rerender({ toggle: second });
    emit(WINDOW_CONTROL_OVERLAY_EVENTS.click);

    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
    // Registered once, not once per render — the stable ref plus the internal
    // ref for the callback is what keeps the native listener set stable. (This
    // is the case that regressed when a fresh ref literal was passed per
    // render, which is why the caller contract is spelled out above.)
    expect(overlay.listen).toHaveBeenCalledTimes(
      Object.keys(WINDOW_CONTROL_OVERLAY_EVENTS).length,
    );
  });

  it('tears every listener down on unmount', async () => {
    const { ref } = maximizeTarget();
    const { unmount } = renderHook(() => useWindowControlOverlay(ref, () => {}));
    await act(async () => {});
    expect(overlay.handlers.size).toBe(Object.keys(WINDOW_CONTROL_OVERLAY_EVENTS).length);

    unmount();
    expect(overlay.handlers.size).toBe(0);
  });

  it('skips a zero-size measurement rather than reporting a useless box', async () => {
    // Before layout settles the rect is all zeroes; reporting it would put the
    // overlay at a 1px degenerate size.
    const { ref } = maximizeTarget({ left: 0, right: 0, top: 0, bottom: 0, width: 0, height: 0 });
    renderHook(() => useWindowControlOverlay(ref, () => {}));
    await act(async () => {});

    expect(reportMetrics.set).not.toHaveBeenCalled();
  });

  it('escapes a rejected measurement so a missing overlay stays silent', async () => {
    reportMetrics.set.mockRejectedValueOnce(new Error('no window handle'));
    const { ref } = maximizeTarget();
    renderHook(() => useWindowControlOverlay(ref, () => {}));
    await act(async () => {});

    // The button simply keeps its DOM hover and click; nothing to surface.
    expect(reportMetrics.set).toHaveBeenCalledTimes(1);
  });

  it('is inert off Windows — no listeners, no metrics, no calls', async () => {
    platform.isWindows = false;
    const { ref } = maximizeTarget();
    const { result } = renderHook(() => useWindowControlOverlay(ref, () => {}));
    await act(async () => {});

    expect(overlay.listen).not.toHaveBeenCalled();
    expect(reportMetrics.set).not.toHaveBeenCalled();
    expect(result.current).toEqual({ hovered: false, pressed: false });
  });
});
