/**
 * useResizable — shared resize drag state machine.
 *
 * Issue #301: the resize hooks in Sidebar / ChangedFilesPanel / SessionView
 * had a stale-closure bug. Their `mousedown` handler snapshotted
 * `startWidthRef.current = width` from closed-over state. The document-level
 * `mousemove`/`mouseup` listeners were installed in a `useEffect` with `[]`
 * deps (necessary — re-attaching mid-drag would lose pointer capture), so
 * the closure was captured once at mount. On the second drag of a fast
 * double-drag, React had not yet flushed the first drag's setWidth(...)
 * into a re-render, so the closed-over `width` was the pre-first-drag value.
 * The visible symptom: the handle visually jumped by 10–30px on the second
 * click, because the drag's baseline was a stale value.
 *
 * Fix
 * ---
 * Keep a `valueRef` updated synchronously in the render body:
 *
 *     const valueRef = useRef(value);
 *     valueRef.current = value;   // runs every render, BEFORE any effect
 *
 * The mousedown handler snapshots from `valueRef.current`, never from the
 * closed-over state. Because the render body runs *before* the effect that
 * installs the document listeners (and before any event handler fired by
 * user input), the ref is always at the latest value at the moment
 * mousedown runs.
 *
 * The effect's deps are `[min, max]`. In practice every call site passes
 * static bounds (module-level constants or literals), so the listeners
 * attach once and never re-attach mid-drag — equivalent to `[]` for the
 * current call sites. The deps are kept explicit so a future caller that
 * binds dynamic bounds gets a correct listener refresh. The stale-closure
 * fix is orthogonal to this — both are needed.
 *
 * Usage
 * -----
 *     const { isResizing, handleMouseDown } = useResizable({
 *       value: width,
 *       min: 192,
 *       max: 480,
 *       side: 'right',           // or 'left' for the left-edge mirror
 *       onChange: (w) => setWidth(w),
 *     });
 *
 *     <div onMouseDown={handleMouseDown} ... />
 *
 * For controlled-mode callers (e.g. ChangedFilesPanel where `width` is a
 * prop), pass the prop as `value` and `onWidthChange` as `onChange`. The
 * hook doesn't own the value — it just keeps its ref in sync and calls
 * `onChange` on every mousemove with the clamped new value.
 */
import { useCallback, useEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from 'react';

export type ResizeSide = 'left' | 'right';

export type UseResizableOptions = {
  /** The current value to use as the drag baseline. Read fresh every render. */
  value: number;
  /** Lower bound for the new value (clamped). */
  min: number;
  /** Upper bound for the new value (clamped). */
  max: number;
  /**
   * Which edge the handle sits on.
   *  - 'right' (default): drag-right grows the value (e.g. Sidebar on the left).
   *  - 'left':           drag-right shrinks the value (e.g. left-edge panel).
   */
  side?: ResizeSide;
  /** Called on each mousemove with the clamped new value. */
  onChange: (value: number) => void;
};

export type UseResizableResult = {
  /** True between mousedown and mouseup. Useful for cursor / handle styling. */
  isResizing: boolean;
  /** Attach to a DOM element's onMouseDown to start a drag. */
  handleMouseDown: (e: ReactMouseEvent) => void;
};

const clamp = (n: number, min: number, max: number) => Math.max(min, Math.min(max, n));

export function useResizable({ value, min, max, side = 'right', onChange }: UseResizableOptions): UseResizableResult {
  // Keep a synchronous ref of the latest value. This is the heart of the
  // stale-closure fix: mousedown reads from this ref, not the closed-over
  // `value` prop. The render body runs before any effect or event handler,
  // so the ref is always at the latest value at the moment mousedown fires.
  const valueRef = useRef(value);
  valueRef.current = value;

  const [isResizing, setIsResizing] = useState(false);
  const isResizingRef = useRef(false);
  const startXRef = useRef(0);
  // Snapshot the baseline at mousedown from the ref (NOT closed-over state).
  const startValueRef = useRef(value);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  // `side` only affects the sign of the delta. We snapshot it once at
  // mousedown so the sign is consistent for the whole drag, even if the
  // prop changes mid-drag (defensive — `side` is a static config in
  // practice, but cheap to handle correctly).
  const signRef = useRef<1 | -1>(side === 'right' ? 1 : -1);

  const handleMouseDown = useCallback((e: ReactMouseEvent) => {
    e.preventDefault();
    isResizingRef.current = true;
    startXRef.current = e.clientX;
    // BUG FIX (#301): read from valueRef, NOT from the closed-over `value`
    // prop. See the file-level JSDoc for the race this prevents.
    startValueRef.current = valueRef.current;
    signRef.current = side === 'right' ? 1 : -1;
    setIsResizing(true);
  }, [side]);

  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      if (!isResizingRef.current) return;
      const delta = (e.clientX - startXRef.current) * signRef.current;
      const next = clamp(startValueRef.current + delta, min, max);
      onChangeRef.current(next);
    };

    const handleMouseUp = () => {
      if (isResizingRef.current) {
        isResizingRef.current = false;
        setIsResizing(false);
      }
    };

    document.addEventListener('mousemove', handleMouseMove);
    document.addEventListener('mouseup', handleMouseUp);
    return () => {
      document.removeEventListener('mousemove', handleMouseMove);
      document.removeEventListener('mouseup', handleMouseUp);
    };
  }, [min, max]);
  // `min` and `max` are read in the listener at drag time via the closures
  // built inside this effect. Including them in deps is correct — clamping
  // bounds can change between renders (rare, but possible if the panel
  // resize-zone was previously capped while the sidebar wasn't).
  // `value` and `onChange` are intentionally NOT in deps: the ref pattern
  // reads them fresh, and we don't want to re-attach listeners mid-drag.

  return { isResizing, handleMouseDown };
}
