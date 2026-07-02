/**
 * useProbeResize — issue #724.
 *
 * Thin wrapper around the shared `useResizable` hook (issue #301) that owns
 * the probe panel's body width as local state and persists the settled
 * value to localStorage so a resize survives app restarts.
 *
 * The bounds [240, 720] were chosen to keep the dock readable at the
 * narrow end (the empty state uses `max-w-[260px]` and several tabs rely
 * on `flex shrink-0` rows that should still fit) while letting wide-diff
 * reviewers reclaim most of a modern monitor at the wide end (AgentReview
 * diffs use `w-10`+`w-10`+`w-4` gutters — see `Diff.tsx`'s `DiffLineRow`,
 * total ~104px of fixed-width gutter). The sidebar's [192, 480] would
 * clip a typical diff, and going past ~720 leaves no useful content for
 * the center workspace on common laptop resolutions.
 *
 * Side is `'left'`, not `'right'`, even though the sidebar uses `'right'`.
 * Both conventions share the same UX rule: **drag the handle away from
 * the panel's outer edge to grow it**. The probe panel sits on the RIGHT
 * of the screen (outer edge = RIGHT) and its handle lives on the panel's
 * LEFT edge, so drag-LEFT grows it. The math works because the panel's
 * RIGHT edge is pinned by the always-visible `w-16` activity bar — the
 * handle visually tracks the cursor while the right edge stays put. With
 * `side: 'right'` the value would still grow on drag-right but the
 * handle would visibly lag behind the cursor (since `next = startValue
 * + delta` moves the LEFT edge to the left, away from the dragged-right
 * cursor). The sidebar has no fixed neighbor on its right, so
 * `side: 'right'` works there.
 *
 * Persistence shape: `useState(() => loadStoredWidth(initialWidth))` on
 * mount (lazy init runs once), `useEffect` on settle (`[isResizing, width]`
 * deps) writes the resolved value back. The `isResizing` guard keeps the
 * synchronous `localStorage.setItem` off the mousemove hot path so the
 * drag doesn't jank. The storage key is exported as
 * `PROBE_PANEL_STORAGE_KEY` so the test suite can pin the runtime key
 * (not just its local copy of the constant — a "uses a distinct key
 * from the sidebar" assertion that compares the test's local constant
 * would pass vacuously if the hook's key were ever renamed).
 */
import { useCallback, useEffect, useState } from 'react';
import { useResizable } from '../../hooks/useResizable';

const MIN_WIDTH = 240;
const MAX_WIDTH = 720;
const DEFAULT_WIDTH = 360;
const STORAGE_KEY = 'buildmesh.probe-panel-width';

/** Public alias so tests can assert the runtime key directly (rather
 *  than re-declaring a local constant that could drift from the hook's
 *  actual key). */
export const PROBE_PANEL_STORAGE_KEY = STORAGE_KEY;

/** Read the persisted width, clamped so a stale/garbage value can't wedge
 *  the probe panel off-screen. Falls back to the caller's initial width. */
function loadStoredWidth(fallback: number): number {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    // `null` means the key was never written. `''` is what
    // `localStorage.setItem(KEY, '')` produces — `Number('')` is 0
    // (finite), which would otherwise clamp down to MIN_WIDTH. Treat
    // both as missing so a stray empty write doesn't silently re-pin
    // the panel at 240px.
    if (raw === null || raw === '') return fallback;
    const parsed = Number(raw);
    if (!Number.isFinite(parsed)) return fallback;
    return Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, parsed));
  } catch {
    return fallback; // storage unavailable (tests, privacy modes)
  }
}

/** Bound any candidate width to the configured [MIN_WIDTH, MAX_WIDTH]
 *  range. Used by the exposed `setWidth` wrapper to enforce the
 *  invariant on every write — the drag path is also clamped internally
 *  by `useResizable`, but the keyboard / programmatic setWidth path is
 *  not, so this is the load-bearing clamp for that code path. */
function clampToBounds(n: number): number {
  return Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, n));
}

export function useProbeResize(initialWidth = DEFAULT_WIDTH) {
  const [width, setRawWidth] = useState(() => loadStoredWidth(initialWidth));

  // Persist on settle rather than every mousemove frame — localStorage
  // writes are synchronous and would jank the drag.
  const { isResizing, handleMouseDown } = useResizable({
    value: width,
    min: MIN_WIDTH,
    max: MAX_WIDTH,
    side: 'left',
    // useResizable clamps to its own [min, max] before calling onChange,
    // so the raw setter is safe to wire here.
    onChange: setRawWidth,
  });

  useEffect(() => {
    if (isResizing) return;
    try {
      window.localStorage.setItem(STORAGE_KEY, String(width));
    } catch {
      // best-effort — a full or unavailable store just means no persistence
    }
  }, [isResizing, width]);

  // Exposed `setWidth` is the load-bearing clamp for the keyboard /
  // programmatic path. `useResizable` already clamps on the drag path,
  // so the drag is covered. The drag uses the raw setter directly
  // (one less closure per frame); the keyboard handler uses the clamped
  // one (no need to remember bounds at the call site). Stable identity
  // via useCallback so consumers can safely add it to dependency arrays
  // without retriggering effects.
  const setWidth = useCallback((next: number) => {
    setRawWidth(clampToBounds(next));
  }, []);

  return { width, isResizing, handleMouseDown, setWidth };
}

/** Bounds + default + storage key, exported together so call sites (the
 *  ARIA attributes on the resize handle in particular) reference one
 *  source of truth. Changing `MIN_WIDTH` here updates the clamp, the
 *  test contract, AND the screen-reader announcement in lockstep. */
export const PROBE_PANEL_BOUNDS = { MIN_WIDTH, MAX_WIDTH, DEFAULT_WIDTH } as const;