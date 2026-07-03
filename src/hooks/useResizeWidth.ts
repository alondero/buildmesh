/**
 * useResizeWidth — observe an element's content-box width via `ResizeObserver`
 * and expose it as a number (px) to drive responsive render branches.
 *
 * Issue #736 — the agent-node header hides progressively as the pane gets
 * narrower. We need a deterministic width signal (not a CSS-only
 * `display: none` we can't see in jsdom) so the component can switch
 * tiers in JS and tests can pin behaviour at 200/300/400/600 px.
 *
 * Test seam: `tests/unit/grid-node-header-responsive.test.tsx` installs a
 * `FakeResizeObserver` via `vi.stubGlobal` whose `observe()` stashes the
 * callback in a per-element `Map`; the test calls `fireResize(el, width)`
 * to push a synthetic `ResizeObserverEntry`. jsdom has no native
 * `ResizeObserver`, so this hook short-circuits in tests (no widths
 * observed → width stays at whatever the synchronous measurement found).
 */
import { useEffect, useLayoutEffect, useState, type RefObject } from 'react';

export function useResizeWidth(ref: RefObject<HTMLElement | null>): number {
  // `Infinity` is the "no measurement yet" sentinel. It's not a real
  // pixel width — `getHeaderTier(Infinity) === 'xl'` renders every chip
  // — and it's only ever replaced by the synchronous snapshot below or
  // by a ResizeObserver callback. jsdom reports `width=0` from
  // `getBoundingClientRect()`, so the layout effect leaves the sentinel
  // in place there; existing tests that don't stub ResizeObserver
  // therefore continue to render at `xl` (their pre-#736 behaviour).
  const [width, setWidth] = useState<number>(Infinity);

  // Synchronous first measurement in `useLayoutEffect` lands before the
  // first paint. The ResizeObserver's own first callback is async per
  // spec — without this snap, a narrow pane would commit one frame at
  // the sentinel (everything visible) then collapse, which the user
  // reads as the very bug #736 is meant to fix. Reading
  // `getBoundingClientRect` here gives us the number RO would have
  // reported on its first tick, with no flash.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measured = el.getBoundingClientRect().width;
    // jsdom reports 0 for every rect; leave the sentinel in that case
    // so the test seam continues to render at the widest tier.
    if (measured > 0) setWidth(measured);
  }, [ref]);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const RO = (globalThis as unknown as { ResizeObserver?: typeof ResizeObserver }).ResizeObserver;
    if (!RO) return;

    const ro = new RO((entries) => {
      const entry = entries[entries.length - 1];
      if (!entry) return;
      setWidth((prev) => {
        const next = entry.contentRect.width;
        // Skip no-op updates so a Splitter drag doesn't queue a render
        // per mousemove when only sub-pixel rounding changed.
        return Object.is(prev, next) ? prev : next;
      });
    });
    ro.observe(el);
    return () => {
      ro.disconnect();
    };
    // Depend on `ref.current` directly — `RefObject` identity is stable
    // across renders, so `[ref]` would be a one-shot. A consumer can
    // unmount/remount the host (conditional render, Suspense,
    // StrictMode double-invoke) and we'd miss the swap without this.
  }, [ref.current]);

  return width;
}
