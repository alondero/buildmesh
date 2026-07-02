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
 * Two shapes (selected by which fields are present on the options):
 *
 * 1. Number shape (Sidebar and any single-pixel-value panel):
 *
 *      useResizable({
 *        value: width,
 *        min: 192,
 *        max: 480,
 *        side: 'right',
 *        onChange: setWidth,
 *      });
 *
 * 2. Splitter shape (multi-pane splitters — AgentNodeView/ResizablePanes,
 *    AgentNodeView/GridSplitter):
 *
 *      const dragIndexRef = useRef(0);
 *      useResizable({
 *        value: widths,                                          // number[] | number[][]
 *        compute: (baseline, deltaPx) => resizeAdjacentPair(...),
 *        axis: 'col',                                            // 'col' (default) | 'row'
 *        throttle: 'raf',                                        // 'sync' (default) | 'raf'
 *        lockBody: true,                                         // default
 *        onChange: setWidths,
 *        onEnd: () => terminalManager.fitAll(),
 *      });
 *
 *      <div onMouseDown={(e) => {
 *        idxRef.current = idx;
 *        // Optional: override the per-drag baseline. Most callers omit
 *        // this; pass it when `value` is a derived slice (e.g.
 *        // `colWidths[rowIdxRef.current]`) and the row index flips BEFORE
 *        // the hook reads — without the override the hook would snapshot
 *        // the previous drag's row widths.
 *        handleMouseDown(e, colWidths[rowIdx]);
 *      }} />
 *
 * Shared guarantees from both shapes:
 *   - `valueRef` is updated synchronously every render (the #301 fix).
 *   - The document mousemove/mouseup listeners live in a `useEffect` with
 *     empty deps; every value the listener reads at drag time comes from a
 *     ref, so no re-attach mid-drag and no stale-closure.
 *   - `lockBody: true` (default on the splitter shape) sets `document.body`'s
 *     `cursor` and `userSelect` on mousedown and clears them on either
 *     mouseup **or** unmount-mid-drag — the modernization pass closed the
 *     unmount-mid-drag leak for GridSplitter; the hook now makes it the
 *     default for every splitter.
 *   - `onEnd` fires once on mouseup.
 *   - `startValueRef` is the SHALLOW-COPY baseline used by every mousemove,
 *     so two mousemoves separated by a React render don't compound the
 *     delta (the original ResizablePanes / GridSplitter each did the same
 *     defensive spread at mousedown).
 */
import {
  useCallback, useEffect, useRef, useState,
  type MouseEvent as ReactMouseEvent,
} from 'react';

export type ResizeSide = 'left' | 'right';
export type ResizeAxis = 'col' | 'row';
export type ResizeThrottle = 'sync' | 'raf';

/**
 * Single handle width shared by every pane-splitter in AgentNodeView. Replaces
 * the duplicated `RESIZE_HANDLE_WIDTH=4` (1–2 panes) and `HANDLE_PX=5`
 * (3+ panes) constants — issue #726 collapses them to one value. The grid's
 * shipped 5 px wins so the 3+ panes aren't visually narrowed; the 1–2 pane
 * path gets +1 px, which is imperceptible next to its 15 % MIN_PANE_PERCENT
 * pane width.
 */
export const SPLITTER_HANDLE_WIDTH = 5;

/** Number shape (Sidebar etc.) — discriminated by the presence of `min`/`max`. */
export type UseResizableNumberOptions = {
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

/**
 * Splitter shape (multi-pane splitters). Discriminated by the presence of
 * `compute` — callers pass either `min`/`max` (number shape) OR `compute`
 * (splitter shape), never both.
 *
 * `compute` does NOT receive container size; callers that need it
 * (e.g. for percent conversion) measure their own `containerRef` inside
 * the compute body. Keeping the hook free of DOM lookups lets it stay
 * pure and unit-testable; the call sites already own the container refs.
 */
export type UseResizableSplitterOptions<T> = {
  /**
   * The current value to use as the drag baseline. Read fresh every render
   * via a ref. Typically `number[]` (ResizablePanes, GridSplitter row
   * heights) or `number[][]` (GridSplitter colWidths[row]).
   */
  value: T;
  /**
   * Pure compute: given the baseline value and the signed pointer delta in
   * px along the drag axis (positive = pointer moved toward the increasing
   * direction of the axis), return the new value. The hook enforces no
   * bounds — the caller clamps to its own MIN_PANE_PERCENT or MIN_PANE_PX.
   * `deltaPx` is `e.clientX - startX` for `'col'` axis, `e.clientY - startY`
   * for `'row'` axis.
   */
  compute: (baseline: T, deltaPx: number) => T;
  /** Which pointer axis drives the delta. Default 'col'. */
  axis?: ResizeAxis;
  /**
   * 'sync' (default) fires `onChange` synchronously on every mousemove.
   * 'raf' coalesces through `requestAnimationFrame` — chosen for the 3+
   * panes grid where every mousemove re-renders several NodeCards.
   */
  throttle?: ResizeThrottle;
  /**
   * When true (default), sets `document.body.style.cursor` to the matching
   * axis cursor and `userSelect: 'none'` on mousedown, and clears both on
   * mouseup **or** unmount-mid-drag. Both splitters in AgentNodeView share
   * this contract.
   *
   * Treat `lockBody` as a STATIC CONFIG (don't toggle mid-drag). The
   * unmount-mid-drag cleanup reads the most recent ref value, so flipping
   * `lockBody` from true to false mid-drag can leak the body lock if the
   * component then unmounts. The current call sites all pass a literal
   * `true`; this note is for future callers.
   */
  lockBody?: boolean;
  /** Called on each mousemove (or each raf frame when throttled) with the next value. */
  onChange: (next: T) => void;
  /** Called once on mouseup. The caller reads `valueRef.current` if needed. */
  onEnd?: () => void;
};

/** Discriminated union — the hook selects based on which fields are populated. */
export type UseResizableOptions<T = number> =
  | UseResizableNumberOptions
  | UseResizableSplitterOptions<T>;

export type UseResizableResult = {
  /** True between mousedown and mouseup. Useful for cursor / handle styling. */
  isResizing: boolean;
  /**
   * Attach to a DOM element's onMouseDown to start a drag.
   *
   * `baselineOverride?` — OPTIONAL pre-mousedown baseline. When supplied,
   * the hook uses it for `startValueRef` instead of `valueRef.current`.
   * Needed only when `value` is a derived slice (e.g.
   * `colWidths[rowIdxRef.current]`) where the underlying index flips
   * between the last render and the hook's internal snapshot read.
   */
  handleMouseDown: (e: ReactMouseEvent, baselineOverride?: unknown) => void;
};

const clamp = (n: number, min: number, max: number) => Math.max(min, Math.min(max, n));

// ----- internal helpers ------------------------------------------------------

/**
 * True when the options object is the splitter shape (carries a `compute`).
 * A `compute: undefined` defaults-prop pattern falls into the number shape
 * — `typeof undefined === 'undefined'`, not `'function'`.
 */
const isSplitterShape = <T>(
  opts: UseResizableOptions<T>,
): opts is UseResizableSplitterOptions<T> =>
  typeof opts === 'object' && opts !== null && 'compute' in opts && typeof (opts as UseResizableSplitterOptions<T>).compute === 'function';

/** Resets body cursor + userSelect set by `lockBody`. Idempotent. */
const releaseBodyLock = () => {
  document.body.style.cursor = '';
  document.body.style.userSelect = '';
};

/** Sets body cursor + userSelect used while a drag is in progress. */
const acquireBodyLock = (axis: ResizeAxis) => {
  document.body.style.cursor = axis === 'row' ? 'row-resize' : 'col-resize';
  document.body.style.userSelect = 'none';
};

/**
 * Shallow-clone helper for the drag baseline. Splitters work on plain
 * arrays (`number[]`); the original ResizablePanes / GridSplitter both did
 * `[...widthsRef.current]` at mousedown as a defensive copy so a caller
 * `compute` that mutated the baseline in place couldn't bleed across
 * subsequent mousemoves. The hook keeps that invariant — if a future
 * caller adopts in-place mutation it stays isolated to a single drag.
 */
const shallowClone = <T>(value: T): T => {
  if (Array.isArray(value)) {
    return [...value] as unknown as T;
  }
  return value;
};

// ----- the hook --------------------------------------------------------------

export function useResizable<T extends number | unknown[]>(
  opts: UseResizableOptions<T>,
): UseResizableResult {
  const isSplitter = isSplitterShape<T>(opts);

  // --- shared state ----------------------------------------------------------
  // The synchronous ref of the latest value is the heart of the stale-closure
  // fix (#301): mousedown reads from this ref, not the closed-over `value`
  // prop. The render body runs before any effect or event handler, so the
  // ref is always at the latest value at the moment mousedown fires.
  const valueRef = useRef<T | number>(opts.value);
  valueRef.current = opts.value;

  const onChangeRef = useRef<unknown>(
    isSplitter ? (opts as UseResizableSplitterOptions<T>).onChange : (opts as UseResizableNumberOptions).onChange,
  );
  onChangeRef.current = isSplitter
    ? (opts as UseResizableSplitterOptions<T>).onChange
    : (opts as UseResizableNumberOptions).onChange;

  const onEndRef = useRef<undefined | (() => void)>(
    isSplitter ? (opts as UseResizableSplitterOptions<T>).onEnd : undefined,
  );
  if (isSplitter) onEndRef.current = (opts as UseResizableSplitterOptions<T>).onEnd;

  const [isResizing, setIsResizing] = useState(false);
  const isResizingRef = useRef(false);

  // --- number-shape fields (refs so the listener reads them on drag) ---------
  // We snapshot `min`/`max` into refs every render and read those refs at
  // drag time. Keeping them as refs means the listener closure never needs
  // to re-attach when bounds change between renders — and re-attaching
  // mid-drag would lose pointer capture. Same ref-discipline as the
  // `#301` fix for `value`.
  //
  // `startValueRef` is the baseline snapshot at mousedown (shallow-cloned
  // for arrays). For the number shape (Sidebar), it's the value at drag
  // start; for the splitter shape, it's the array at drag start. Storing
  // it once here means EVERY mousemove computes against the same baseline.
  const minRef = useRef<number>(isSplitter ? 0 : (opts as UseResizableNumberOptions).min);
  const maxRef = useRef<number>(isSplitter ? 0 : (opts as UseResizableNumberOptions).max);
  const startValueRef = useRef<unknown>(opts.value);
  const startXRef = useRef(0);
  const startYRef = useRef(0);
  const signRef = useRef<1 | -1>(1);

  if (!isSplitter) {
    const n = opts as UseResizableNumberOptions;
    minRef.current = n.min;
    maxRef.current = n.max;
  }

  // --- splitter-shape fields -------------------------------------------------
  // `compute`, `axis`, `throttle`, `lockBody` are all read by the listener
  // via refs so a prop flip between renders lands safely.
  const computeRef = useRef<((b: T, d: number) => T) | null>(null);
  const axisRef = useRef<ResizeAxis>('col');
  const throttleRef = useRef<ResizeThrottle>('sync');
  const lockBodyRef = useRef<boolean>(false);

  if (isSplitter) {
    const s = opts as UseResizableSplitterOptions<T>;
    computeRef.current = s.compute;
    axisRef.current = s.axis ?? 'col';
    throttleRef.current = s.throttle ?? 'sync';
    lockBodyRef.current = s.lockBody ?? true;
  }

  // --- handleMouseDown --------------------------------------------------------
  const handleMouseDown = useCallback((e: ReactMouseEvent, baselineOverride?: unknown) => {
    e.preventDefault();
    isResizingRef.current = true;
    startXRef.current = e.clientX;
    startYRef.current = e.clientY;

    // Bug fix #301 on the number shape, GENERIC START-BASELINE on the
    // splitter shape: snapshot the latest `valueRef.current` here, then
    // every mousemove reads from THIS snapshot rather than `valueRef`.
    //
    // Why the snapshot matters for splitters specifically: the original
    // ResizablePanes rebuilt the widths array from `startWidths`
    // (snapshotted at mousedown) on every mousemove, so a single drag was
    // always computed against the original baseline — the divider moved
    // exactly `deltaPercent` from its mousedown position, not compounded
    // from the previous mousemove's intermediate widths. Snapshotting once
    // at mousedown keeps both shapes behaviorally identical to their
    // original implementations.
    //
    // `baselineOverride` lets the caller provide a freshly-derived slice
    // (e.g. `colWidths[rowIdx]` when the user clicks a column separator in
    // a specific row). Without the override, the hook would fall back to
    // `valueRef.current`, which captured the value at the LAST render
    // body — by the time `handleMouseDown` fires, `rowIdxRef.current` has
    // been updated, but `valueRef.current` hasn't been re-read.
    startValueRef.current = shallowClone(baselineOverride ?? valueRef.current);

    if (!isSplitter) {
      signRef.current = (opts as UseResizableNumberOptions).side === 'left' ? -1 : 1;
    } else if (lockBodyRef.current) {
      acquireBodyLock(axisRef.current);
    }

    setIsResizing(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  // Empty deps: the callback reads every config via refs; no reason to
  // re-allocate each render. The stable identity (stable across renders)
  // also lets future callers use it inside an effect dep array without
  // perpetual re-runs.

  // --- document listeners -----------------------------------------------------
  // The listener reads EVERY value (min, max, axis, throttle, lockBody, compute,
  // onChange, value baseline, sign) via refs kept in sync every render.
  // Effects run on mount only; no re-attach on prop change, no risk of
  // losing pointer capture mid-drag.
  useEffect(() => {
    let rafId: number | null = null;
    let lastDelta = 0;

    const handleMouseMove = (e: MouseEvent) => {
      if (!isResizingRef.current) return;

      if (!isSplitter) {
        const delta = (e.clientX - startXRef.current) * signRef.current;
        const next = clamp((startValueRef.current as number) + delta, minRef.current, maxRef.current);
        (onChangeRef.current as (v: number) => void)(next);
        return;
      }

      // Splitter shape.
      const axis = axisRef.current;
      const deltaRaw = axis === 'row'
        ? e.clientY - startYRef.current
        : e.clientX - startXRef.current;
      lastDelta = deltaRaw;

      const tick = () => {
        rafId = null;
        if (!isResizingRef.current) return;
        const compute = computeRef.current;
        if (!compute) return;
        // Pass the mousedown-time SNAPSHOT of `value` (captured in
        // `handleMouseDown`) to `compute`, NOT the latest `valueRef.current`.
        // See the rationale in `handleMouseDown`.
        const baseline = startValueRef.current as T;
        const next = compute(baseline, lastDelta);
        (onChangeRef.current as (v: T) => void)(next);
      };

      if (throttleRef.current === 'raf') {
        if (rafId !== null) return; // a frame is already queued; coalesce.
        rafId = requestAnimationFrame(tick);
      } else {
        tick();
      }
    };

    const handleMouseUp = () => {
      if (!isResizingRef.current) return;
      isResizingRef.current = false;
      setIsResizing(false);

      if (isSplitter && lockBodyRef.current) {
        releaseBodyLock();
      }

      // Drain any raf queued during the drag so we don't fire onChange AFTER
      // mouseup. For the grid path this was previously cancelled inline; the
      // hook generalises it.
      if (rafId !== null) {
        cancelAnimationFrame(rafId);
        rafId = null;
      }

      // Splitter `onEnd` fires exactly once at mouseup, AFTER the last
      // onChange. The caller reads `valueRef.current` (or its own settled
      // state) if they need the persisted value.
      if (isSplitter && onEndRef.current) {
        onEndRef.current();
      }
    };

    document.addEventListener('mousemove', handleMouseMove);
    document.addEventListener('mouseup', handleMouseUp);
    return () => {
      document.removeEventListener('mousemove', handleMouseMove);
      document.removeEventListener('mouseup', handleMouseUp);
      if (rafId !== null) cancelAnimationFrame(rafId);
      // Unmount-mid-drag safety: if the component went away while the user
      // was mid-drag, release the body lock so the cursor/user-select don't
      // stick. The modernization pass closed this leak for GridSplitter;
      // the hook makes it the default for every splitter. Note: relies on
      // `lockBodyRef.current` being consistent across the drag — callers
      // must NOT toggle `lockBody` mid-drag (see JSDoc on the option).
      if (isResizingRef.current && isSplitter && lockBodyRef.current) {
        releaseBodyLock();
        isResizingRef.current = false;
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  // Intentionally empty deps: every value the listener reads at drag time
  // comes from a ref kept in sync during the render body. See the
  // `#301` fix note in the file-level JSDoc.

  return { isResizing, handleMouseDown };
}
