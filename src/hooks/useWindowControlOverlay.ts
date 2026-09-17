import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { isWindows } from '../lib/platform';
import { setTitlebarMaximizeMetrics } from '../lib/tauri';

/** The events the native snap overlay emits (ADR-0035). Mirrors the constants
 *  in `src-tauri/src/windowing/snap_overlay.rs`; the pair is pinned by
 *  `tests/unit/use-window-control-overlay.test.tsx`. */
export const WINDOW_CONTROL_OVERLAY_EVENTS = {
  hover: 'titlebar-overlay:hover',
  leave: 'titlebar-overlay:leave',
  press: 'titlebar-overlay:press',
  release: 'titlebar-overlay:release',
  click: 'titlebar-overlay:click',
} as const;

/** The maximise button's hover / press state as reported by the native snap
 *  overlay, for a caption button whose own `:hover` and `:active` cannot fire.
 *
 *  Windows 11 offers the Snap Layouts flyout only to a window whose
 *  `WM_NCHITTEST` answers `HTMAXBUTTON`, which a frameless Tauri window can't do
 *  for itself: its WebView2 child covers the client area and answers the hit
 *  test first (ADR-0035). A native child window is parked over the maximise
 *  button instead — and because that window then owns the mouse in its
 *  rectangle, the button's DOM hover and click never fire on Windows. This hook
 *  measures the button so the overlay can be positioned on it, and translates
 *  the overlay's events back into the button's states plus the maximise toggle.
 *
 *  Windows-only and best-effort throughout: if the overlay never installs the
 *  button keeps its plain CSS hover and its DOM `onClick`, so the controls stay
 *  fully usable everywhere. Keyboard activation never goes through the overlay
 *  at all — it still runs the button's own handler.
 *
 *  Note the maximise glyph is deliberately *not* state this hook owns: it still
 *  follows the real window state through the `onResized` re-query, so a click
 *  here can't desync it. */
export function useWindowControlOverlay(
  maximizeButtonRef: React.RefObject<HTMLButtonElement | null>,
  onToggleMaximize: () => void,
) {
  const [hovered, setHovered] = useState(false);
  const [pressed, setPressed] = useState(false);

  // Held in a ref so the native listeners are registered once instead of being
  // torn down and re-established on every render — `onToggleMaximize` is an
  // inline closure at the call site.
  const toggle = useRef(onToggleMaximize);
  useEffect(() => {
    toggle.current = onToggleMaximize;
  }, [onToggleMaximize]);

  useEffect(() => {
    if (!isWindows) return;

    let disposed = false;
    let frame = 0;
    const unlisteners: Array<() => void> = [];

    /** Report the button's real box. Logical (CSS) pixels: the root element's
        right edge and the client rect share the viewport's coordinate space,
        which is the client area the overlay is positioned in, and the backend
        applies the window's DPI scale. */
    const reportMetrics = () => {
      const button = maximizeButtonRef.current;
      if (!button) return;
      const rect = button.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) return;
      // Measured against the root element's right edge, NOT `window.innerWidth`:
      // the latter is an integer while `getBoundingClientRect` is fractional, so
      // mixing them biases the inset by up to a pixel at fractional display
      // scaling — and because the bias flips as the viewport width changes, the
      // overlay drifts about a pixel and ends up overlapping the neighbouring
      // close button. This is the same viewport edge without the rounding.
      const viewportRight = document.documentElement.getBoundingClientRect().right;
      // No layout to measure against (jsdom without mocked geometry, or a
      // document that has not been styled): a zero viewport right would make the
      // inset negative and describe a box that cannot be true, so skip the
      // report — the same discipline as the degenerate button rect above — rather
      // than send nonsense and park the overlay off the right edge.
      if (viewportRight <= 0) return;
      setTitlebarMaximizeMetrics({
        rightInset: viewportRight - rect.right,
        top: rect.top,
        width: rect.width,
        height: rect.height,
      }).catch(() => {
        // Best effort: no metrics means no overlay, and the button simply keeps
        // its DOM hover and click. Nothing for the user to act on.
      });
    };

    // The backend stores the button's right *inset*, so it repositions itself
    // through a live resize without help; re-measuring is the safety net for a
    // bar layout change that moves the button's box.
    const onResize = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(reportMetrics);
    };

    const setup = async () => {
      try {
        const handlers: Array<[string, () => void]> = [
          [WINDOW_CONTROL_OVERLAY_EVENTS.hover, () => setHovered(true)],
          [
            WINDOW_CONTROL_OVERLAY_EVENTS.leave,
            () => {
              setHovered(false);
              setPressed(false);
            },
          ],
          [WINDOW_CONTROL_OVERLAY_EVENTS.press, () => setPressed(true)],
          [WINDOW_CONTROL_OVERLAY_EVENTS.release, () => setPressed(false)],
          // The overlay swallows the mouse click, so on Windows this is the
          // only path a click takes. It calls the same toggle the button's own
          // `onClick` does, which is what keeps the single-writer contract.
          [WINDOW_CONTROL_OVERLAY_EVENTS.click, () => toggle.current()],
        ];

        for (const [event, handler] of handlers) {
          const unlisten = await listen(event, handler);
          if (disposed) {
            unlisten();
            return;
          }
          unlisteners.push(unlisten);
        }

        window.addEventListener('resize', onResize);
        frame = requestAnimationFrame(reportMetrics);
      } catch (e) {
        console.warn('Failed to set up the window-control snap overlay:', e);
      }
    };

    void setup();

    return () => {
      disposed = true;
      cancelAnimationFrame(frame);
      window.removeEventListener('resize', onResize);
      for (const unlisten of unlisteners) unlisten();
    };
    // `maximizeButtonRef` must be a stable ref object (a `useRef` at the call
    // site, which is what TitleBar passes). It is only *read* inside the
    // effect, so its identity carries no meaning — but a caller re-rendering
    // with a fresh object would rebuild the native listeners each render, so
    // the rule's insistence is the right default here.
  }, [maximizeButtonRef]);

  return { hovered, pressed };
}
