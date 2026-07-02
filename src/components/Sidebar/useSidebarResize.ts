import { useEffect, useState } from 'react';
import { useResizable } from '../../hooks/useResizable';

const MIN_WIDTH = 192;
const MAX_WIDTH = 480;
const DEFAULT_WIDTH = 256;
const STORAGE_KEY = 'buildmesh.sidebar-width';

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

/**
 * Sidebar's width-tracker. Thin wrapper around the shared `useResizable`
 * hook (issue #301) — owns `width` as local state and exposes
 * `{ width, isResizing, handleMouseDown }` for the resize handle in
 * `Sidebar.tsx`.
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
export function useSidebarResize(initialWidth = DEFAULT_WIDTH) {
  const [width, setWidth] = useState(() => loadStoredWidth(initialWidth));

  // Persist on settle rather than every mousemove frame — localStorage
  // writes are synchronous and would jank the drag.
  const { isResizing, handleMouseDown } = useResizable({
    value: width,
    min: MIN_WIDTH,
    max: MAX_WIDTH,
    side: 'right', // Sidebar sits on the left; dragging the right edge right grows it.
    onChange: setWidth,
  });

  useEffect(() => {
    if (isResizing) return;
    try {
      window.localStorage.setItem(STORAGE_KEY, String(width));
    } catch {
      // best-effort — a full or unavailable store just means no persistence
    }
  }, [isResizing, width]);

  return { width, isResizing, handleMouseDown };
}
