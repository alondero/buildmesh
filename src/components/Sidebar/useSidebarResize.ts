import { useCallback, useEffect, useState } from 'react';
import { useResizable } from '../../hooks/useResizable';

const MIN_WIDTH = 192;
const MAX_WIDTH = 480;
const DEFAULT_WIDTH = 256;
const STORAGE_KEY = 'buildmesh.sidebar-width';

/** Public alias so tests can assert the runtime storage key directly
 *  (rather than re-declaring a local constant that could drift from the
 *  hook's actual key). */
export const SIDEBAR_STORAGE_KEY = STORAGE_KEY;

/** Read the persisted width, clamped so a stale/garbage value can't wedge
 * the sidebar off-screen. Falls back to the caller's initial width. */
function loadStoredWidth(fallback: number): number {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    const parsed = raw === null ? NaN : Number(raw);
    if (!Number.isFinite(parsed)) return fallback;
    return Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, parsed));
  } catch {
    return fallback; // storage unavailable (tests, privacy modes)
  }
}

/** Bound any candidate width to the configured [MIN_WIDTH, MAX_WIDTH]
 *  range. `useResizable` clamps the drag path with the same `min`/`max`;
 *  the keyboard / programmatic `setWidth` path is not covered by it, so
 *  this clamp is what bounds the arrow keys. */
function clampToBounds(n: number): number {
  return Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, n));
}

/**
 * Sidebar's width-tracker. Thin wrapper around the shared `useResizable`
 * hook (issue #301) — owns `width` as local state and exposes
 * `{ width, isResizing, handleMouseDown, setWidth }` for the resize
 * handle in `Sidebar.tsx`. `setWidth` is the keyboard path (issue
 * #2424): it clamps to the same [192, 480] range the drag does, so the
 * arrow keys stay inside the sidebar's bounds.
 *
 * The width persists to localStorage so a resize survives app restarts
 * (it previously reset to 256px on every launch).
 *
 * Previously this hook re-implemented the drag-state machine inline
 * (`isResizing` state, `resizingRef`/`startXRef`/`startWidthRef` refs, plus
 * a `useEffect` installing document mousemove/mouseup listeners). That
 * implementation had a stale-closure bug: the mousedown handler snapshotted
 * `startWidthRef.current = width` from closed-over state, and on the second
 * drag of a fast double-drag React had not yet flushed the first drag's
 * `setWidth(...)` — so the second drag started from a stale baseline and
 * the handle visibly jumped by 10–30px. The shared hook keeps a
 * `valueRef` updated synchronously in the render body and snapshots from
 * that ref. See `src/hooks/useResizable.ts` for the full analysis.
 */
export function useSidebarResize(initialWidth = DEFAULT_WIDTH, resolveWidth?: (visibleWidth: number) => number) {
  const [width, setRawWidth] = useState(() => loadStoredWidth(initialWidth));

  // Persist on settle rather than every mousemove frame — localStorage
  // writes are synchronous and would jank the drag.
  const { isResizing, handleMouseDown } = useResizable({
    value: width,
    min: MIN_WIDTH,
    max: MAX_WIDTH,
    side: 'right', // Sidebar sits on the left; dragging the right edge right grows it.
    onChange: (next: number) => setRawWidth(clampToBounds(resolveWidth?.(next) ?? next)),
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
  // programmatic path (issue #2424 — the sidebar's resize handle is
  // operable with the arrow keys). `useResizable` already clamps the
  // drag path, so the drag is covered; the keyboard handler uses the
  // clamped one so the call site does not have to remember the bounds.
  // Stable identity via useCallback so consumers can safely add it to
  // dependency arrays without retriggering effects.
  const setWidth = useCallback((next: number) => {
    setRawWidth(clampToBounds(resolveWidth?.(next) ?? next));
  }, [resolveWidth]);

  return { width, isResizing, handleMouseDown, setWidth };
}

/** Bounds + default, exported together so the call sites (the ARIA
 *  attributes on the resize handle in particular) reference one source
 *  of truth. Changing `MIN_WIDTH` here updates the clamp, the test
 *  contract, AND the screen-reader announcement in lockstep. */
export const SIDEBAR_BOUNDS = { MIN_WIDTH, MAX_WIDTH, DEFAULT_WIDTH } as const;
