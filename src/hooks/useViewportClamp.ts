/**
 * useViewportClamp — shift a menu up when it would overflow the
 * viewport's bottom edge.
 *
 * Issue #837 — extracted from two call sites where the same
 * `useLayoutEffect` that reads `getBoundingClientRect()` and applies
 * `translateY(-shift)` was duplicated:
 *
 *   - `src/components/BuildRun/BuildRunDropdown.tsx` (#814)
 *   - `src/components/Sidebar/ProviderDropdown.tsx` (#814)
 *
 * The pattern: the menu is anchored at its trigger (`right-0 top-full
 * mt-1` for the dropdown cases) and rendered *before* the browser
 * measures it. `useLayoutEffect` reads the rect and, if the menu's
 * bottom edge would push past the viewport, applies a CSS
 * `translateY(-shift)` offset that pulls the menu up. The original
 * anchor stays intact — only a transform is added — so the open
 * animation (`animate-scale-in origin-top-right`) still plays cleanly.
 *
 * The shift cap is `rect.top - MARGIN` (the space above the menu's
 * rendered top), NOT `rect.top - rect.height - MARGIN` — subtracting
 * the menu's own height would under-cap and leave the menu still
 * overflowing when the menu is taller than the gap between the
 * trigger and the viewport's top. This is the "subtle fix a hook
 * would lock in once" the issue called out.
 *
 * Cleanup resets `style.transform` so a remount starts from the
 * unclamped position — the parent's `animate-scale-in` runs every
 * open, so a stale transform would clip the opening frame.
 *
 * The `deps` array mirrors the React `useLayoutEffect` contract: pass
 * the values that, when changed, should re-measure the menu. Most
 * callers pass `[open]` (re-measure on each open flip); `ProviderDropdown`
 * passes `[providers]` because it has no open boolean (the menu is
 * always rendered when its parent is).
 *
 * Out of scope
 * ------------
 * `MeshItem.tsx`'s context menu uses `setState` repositioning (it
 * adjusts `contextMenu.x/y`, not a `transform`) because the menu is
 * anchored at the click point rather than a trigger. The issue
 * explicitly leaves MeshItem's anchor mechanism out of scope.
 * `KebabActions` and the portaled PR pill use `useAnchoredPosition` for
 * trigger-relative fixed coordinates; this hook remains for menus whose
 * existing anchor is already expressed in CSS and only needs vertical
 * clamping.
 */
import { useLayoutEffect, type RefObject } from 'react';

export interface UseViewportClampOptions {
  /** Pixel gap from the viewport edge to leave visible. Default `4`. */
  margin?: number;
}

export function useViewportClamp(
  ref: RefObject<HTMLElement | null>,
  deps: ReadonlyArray<unknown>,
  options: UseViewportClampOptions = {},
): void {
  const { margin = 4 } = options;

  useLayoutEffect(() => {
    const menu = ref.current;
    if (!menu) return;
    const rect = menu.getBoundingClientRect();
    const vh = window.innerHeight;
    const overflow = rect.bottom - (vh - margin);
    if (overflow <= 0) return;
    // Bound the shift by the space ABOVE the menu's current top so the
    // shifted-up top doesn't land at a negative y. `rect.top - MARGIN`
    // (not `rect.top - rect.height - MARGIN` — that would double-
    // subtract the menu's own height and produce too small a cap).
    const maxShift = Math.max(0, rect.top - margin);
    const shift = Math.min(overflow, maxShift);
    if (shift <= 0) return;
    menu.style.transform = `translateY(-${shift}px)`;
    return () => {
      menu.style.transform = '';
    };
    // The caller controls when re-measurement happens via `deps`. We
    // intentionally do NOT include `ref` or `margin` — a ref object is
    // stable across renders, and `margin` is a static config value in
    // practice (every current caller uses the default).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}
