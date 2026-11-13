import { Fragment, useEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from 'react';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
import { useNodeActivityStore } from '../../stores/nodeActivityStore';
import { useScope } from '../../hooks/useScope';
import { useGridLayoutStore, resolveLayout } from '../../stores/gridLayoutStore';
import { terminalManager } from '../Terminal/Terminal';
import { NodeCard } from './NodeCard';
import { getGridRows, equalSizes, minimumGridSize } from '../../hooks/useGridLayout';
import { useResizable, SPLITTER_HANDLE_WIDTH } from '../../hooks/useResizable';
import { useActiveCardScroll } from './useActiveCardScroll';
import { useGridResizeFloor } from './useGridResizeFloor';
import { Separator } from '../shared/Separator';
import { clampResizeAdjacentPair, minPaneSizePx } from './paneResize';

interface GridSplitterProps {
  nodes: readonly AgentNode[];
  activityMembersByRoot: Readonly<Record<number, readonly number[]>>;
  // Pinned Grid mode disables card drag-reorder (wayfinder #982 / #986 —
  // custom pinned ordering is map fog). Defaults to draggable so Mesh/All
  // keep today's behaviour.
  draggable?: boolean;
}

type DragAxis = 'col' | 'row';

export function GridSplitter({ nodes, activityMembersByRoot, draggable = true }: GridSplitterProps) {
  const rowCounts = getGridRows(nodes.length);
  const rows = rowCounts.length;
  const rowKey = rowCounts.join(',');

  // Layout persistence is mesh-scoped: only Mesh Grid mode saves/restores
  // under the selected mesh's key (wayfinder #982 / #986). All Nodes and
  // Pinned render cross-mesh sets whose arrangement must not pollute any
  // single mesh's saved layout — they get session-local equal splits, the
  // same behaviour 'all' already had via a null selection.
  const scope = useScope();
  const layoutMeshId = scope.viewMode === 'mesh' ? scope.selectedMeshId : null;
  const viewportRef = useRef<HTMLDivElement>(null);
  const { floor, capture } = useGridResizeFloor(viewportRef, `${layoutMeshId}:${rowKey}`);

  const containerRef = useRef<HTMLDivElement>(null);
  const [containerSize, setContainerSize] = useState({ width: 1000, height: 600 });

  // colWidths is jagged: one width array per row, so a vertical handle only
  // resizes its own row. Heights stay one-per-row. Initialise from the per-mesh
  // store, shape-validated against the current layout (falls back to equal).
  const [colWidths, setColWidths] = useState<number[][]>(
    () => resolveLayout(useGridLayoutStore.getState().byMesh[layoutMeshId!], rowCounts).colWidths,
  );
  const [rowHeights, setRowHeights] = useState<number[]>(
    () => resolveLayout(useGridLayoutStore.getState().byMesh[layoutMeshId!], rowCounts).rowHeights,
  );

  const colWidthsRef = useRef(colWidths);
  const rowHeightsRef = useRef(rowHeights);
  const layoutMeshIdRef = useRef(layoutMeshId);
  colWidthsRef.current = colWidths;
  rowHeightsRef.current = rowHeights;
  layoutMeshIdRef.current = layoutMeshId;

  // Re-load when the active mesh OR the grid shape changes. Reading via
  // getState() (not a subscription) keeps the mouse-up persist below from
  // retriggering this effect into a loop.
  useEffect(() => {
    const l = resolveLayout(useGridLayoutStore.getState().byMesh[layoutMeshId!], getGridRows(nodes.length));
    setColWidths(l.colWidths);
    setRowHeights(l.rowHeights);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [layoutMeshId, rowKey]);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    // jsdom ships no ResizeObserver, and an environment without the webview
    // has none either. The grid's layout model only
    // needs the container size to convert the pixel floor into a
    // percentage, so the `containerSize` default below stands in when
    // no observer is available — same pattern as `useResizeWidth`.
    const Observer = (globalThis as unknown as { ResizeObserver?: typeof ResizeObserver }).ResizeObserver;
    if (!Observer) {
      const rect = el.getBoundingClientRect();
      if (rect.width > 0 && rect.height > 0) setContainerSize({ width: rect.width, height: rect.height });
      return;
    }
    const observer = new Observer(([entry]) => {
      const w = entry.contentRect.width;
      const h = entry.contentRect.height;
      // A zero-sized box is a transient (hidden container), not a real
      // measurement — keeping the last good size stops the keyboard
      // range from collapsing.
      if (w <= 0 || h <= 0) return;
      setContainerSize(prev => (prev.width === w && prev.height === h) ? prev : { width: w, height: h });
    });
    observer.observe(el);
    const rect = el.getBoundingClientRect();
    if (rect.width > 0 && rect.height > 0) setContainerSize({ width: rect.width, height: rect.height });
    return () => observer.disconnect();
  }, []);

  // Per-drag context, captured at mousedown, read inside both `compute`s and
  // the persist-on-end. The col drag has `i = separatorIndex` and
  // `rowIdx = which row the separator belongs to`. The row drag has
  // `i = rowIdx` (the dragged separator sits between `rowHeights[i]` and
  // `rowHeights[i+1]`) and no separate `rowIdx`.
  const idxRef = useRef<number>(0);
  const rowIdxRef = useRef<number>(0);
  // Container size is snapshotted at mousedown so the compute closure
  // (installed once at mount) always uses a fresh-enough size. Resize during
  // a drag is rare; we re-snapshot on the next drag.
  const dragContainerSizeRef = useRef<{ width: number; height: number } | null>(null);

  /**
   * Persist the widths/heights to the per-mesh layout store, then ask xterm
   * to re-measure the attached terminals. It is the `onEnd` of both the col
   * and the row `useResizable` hook, so a dragged resize reaches it on
   * mouseup.
   *
   * The optional arguments carry the NEXT values for the keyboard path
   * (issue #2424): a key press commits its value through React state, so
   * the refs still hold the PREVIOUS layout until the next render. The
   * keyboard handler therefore hands over the values it just computed,
   * and the store records those — a keyboard resize lands in the same
   * per-mesh layout a dragged one writes.
   */
  const persistAndFit = (
    nextColWidths: number[][] = colWidthsRef.current,
    nextRowHeights: number[] = rowHeightsRef.current,
  ) => {
    const meshId = layoutMeshIdRef.current;
    if (meshId != null) {
      useGridLayoutStore.getState().setLayout(meshId, {
        colWidths: nextColWidths,
        rowHeights: nextRowHeights,
      });
    }
    terminalManager.fitAll();
  };

  /**
   * Keyboard resize for a COLUMN separator (issue #2424).
   *
   * The separator controls the width of the pane in FRONT of it (the left
   * pane of the pair), so ArrowRight grows it and ArrowLeft shrinks it —
   * the same direction the handle moves when it is dragged. The value is
   * expressed in pixels of the container so the announced range matches
   * the pixel floor the drag enforces (`minPaneSizePx`).
   */
  const resizeColumnByKeyboard = (rowIdx: number, colIdx: number, nextPx: number) => {
    const baseline = colWidthsRef.current[rowIdx];
    if (!baseline) return;
    const containerWidth = containerSize.width;
    if (!containerWidth) return;
    const columns = rowCounts[rowIdx];
    const minPx = minPaneSizePx(columns);
    const currentPx = (baseline[colIdx] / 100) * containerWidth;
    const deltaPct = ((nextPx - currentPx) / containerWidth) * 100;
    const minPct = (minPx / containerWidth) * 100;
    const nextRow = clampResizeAdjacentPair(baseline, colIdx, deltaPct, minPct);
    const nextColWidths = colWidthsRef.current.map((row, idx) => (idx === rowIdx ? nextRow : row));
    setColWidths(nextColWidths);
    persistAndFit(nextColWidths, rowHeightsRef.current);
  };

  /**
   * Keyboard resize for a ROW separator (issue #2424). The separator
   * controls the height of the row in FRONT of it (the top row of the
   * pair), so ArrowDown grows it — the direction the handle moves when
   * it is dragged.
   */
  const resizeRowByKeyboard = (rowIdx: number, nextPx: number) => {
    const baseline = rowHeightsRef.current;
    if (!baseline) return;
    const containerHeight = containerSize.height;
    if (!containerHeight) return;
    const minPx = minPaneSizePx(rows);
    const currentPx = (baseline[rowIdx] / 100) * containerHeight;
    const deltaPct = ((nextPx - currentPx) / containerHeight) * 100;
    const minPct = (minPx / containerHeight) * 100;
    const nextRowHeights = clampResizeAdjacentPair(baseline, rowIdx, deltaPct, minPct);
    setRowHeights(nextRowHeights);
    persistAndFit(colWidthsRef.current, nextRowHeights);
  };

  // Two shared `useResizable` calls — one per axis. The col drag only edits
  // ONE row's widths array (the row that owns the dragged separator); the
  // row drag edits the full heights array. They share the body cursor +
  // userSelect lock contract (both pass `lockBody: true`) and the raf
  // throttle.
  //
  // Why two hooks: the value shape differs per axis. They coexist cleanly —
  // only one drag is active at a time, and each hook's `isResizingRef`
  // makes the inactive one's listener early-return.
  //
  // The col hook's `value` is a stable placeholder (`[]`); the per-drag
  // BASELINE is supplied via the `baselineOverride` second argument to
  // `handleMouseDown` inside `startDrag`. Earlier we passed
  // `value: colWidths[rowIdxRef.current]` — that reads at render time
  // and would snapshot the PREVIOUS drag's row widths, writing the wrong
  // array into the just-clicked row. The override pattern keeps the
  // baseline scoped to the exact row the user clicked.
  const { handleMouseDown: handleColMouseDown } = useResizable<number[]>({
    value: [],
    axis: 'col',
    throttle: 'raf',
    lockBody: true,
    compute: (baseline, deltaPx) => {
      const containerSize = dragContainerSizeRef.current?.width;
      if (!containerSize) return baseline;
      const columns = rowCounts[rowIdxRef.current];
      const minPct = (minPaneSizePx(columns) / containerSize) * 100;
      const deltaPct = (deltaPx / containerSize) * 100;
      return clampResizeAdjacentPair(baseline, idxRef.current, deltaPct, minPct);
    },
    onChange: (next: number[]) => {
      const rowIdx = rowIdxRef.current;
      setColWidths((prev) => prev.map((r, idx) => (idx === rowIdx ? next : r)));
    },
    onEnd: persistAndFit,
  });

  const { handleMouseDown: handleRowMouseDown } = useResizable<number[]>({
    value: rowHeights,
    axis: 'row',
    throttle: 'raf',
    lockBody: true,
    compute: (baseline, deltaPx) => {
      const containerSize = dragContainerSizeRef.current?.height;
      if (!containerSize) return baseline;
      const minPct = (minPaneSizePx(rows) / containerSize) * 100;
      const deltaPct = (deltaPx / containerSize) * 100;
      return clampResizeAdjacentPair(baseline, idxRef.current, deltaPct, minPct);
    },
    onChange: setRowHeights,
    onEnd: persistAndFit,
  });

  /**
   * Per-drag setup runs BEFORE `handleMouseDown`, so the hook sees the
   * right `idxRef` / `rowIdxRef` / `dragContainerSizeRef` AND the
   * correct baseline array (for the col axis, the row that the user
   * just clicked — read from the live `colWidthsRef` snapshot, not the
   * stale render-time value).
   */
  const startDrag = (axis: DragAxis, index: number, row = 0) => (e: ReactMouseEvent) => {
    idxRef.current = index;
    rowIdxRef.current = row;
    const el = containerRef.current;
    capture(axis === 'col' ? 'width' : 'height', el);
    if (el) {
      const rect = el.getBoundingClientRect();
      dragContainerSizeRef.current = { width: rect.width - 8, height: rect.height - 8 };
    }
    if (axis === 'col') {
      // `colWidthsRef.current[row]` is read SYNCHRONOUSLY here, after the
      // ref was updated, so the hook sees the clicked row's widths — not
      // the previous drag's row widths that were captured at the last
      // render. This is the entire reason `baselineOverride` exists on
      // `handleMouseDown`.
      const baseline = colWidthsRef.current[row] ? [...colWidthsRef.current[row]] : equalSizes(1);
      handleColMouseDown(e, baseline);
    } else {
      handleRowMouseDown(e);
    }
  };

  const activeNodeId = useAgentNodeStore(state => state.activeNodeId);
  const activateNode = useNodeActivityStore(state => state.activateNode);

  useActiveCardScroll(containerRef, activeNodeId);

  const totalHandleHeightPct = ((rows - 1) * SPLITTER_HANDLE_WIDTH / containerSize.height) * 100;

  return (
    <div ref={viewportRef} className="flex-1 min-h-0 min-w-0 overflow-auto" data-testid="grid-scroll-viewport">
    <div
      ref={containerRef}
      className="flex h-full flex-col p-1 bg-bg-surface"
      style={{ minWidth: Math.max(floor.width, Math.max(...rowCounts.map((count, row) => minimumGridSize(colWidths[row] ?? equalSizes(count), SPLITTER_HANDLE_WIDTH))) + 8),
        minHeight: Math.max(floor.height, minimumGridSize(rowHeights, SPLITTER_HANDLE_WIDTH) + 8) }}
    >
      {rowCounts.map((rowCount, rowIdx) => {
        const rowHeight = `calc(${rowHeights[rowIdx] ?? 100 / rows}% - ${totalHandleHeightPct / rows}%)`;

        const widths = colWidths[rowIdx] ?? equalSizes(rowCount);
        const totalHandleWidthPct = ((rowCount - 1) * SPLITTER_HANDLE_WIDTH / containerSize.width) * 100;
        const startIdx = rowCounts.slice(0, rowIdx).reduce((a, b) => a + b, 0);
        // The pixel floor for ONE pane of this row, and the container
        // width the percentages are measured against. The keyboard path
        // (issue #2424) announces these two numbers as the separator's
        // range, so the screen reader states the bounds the resize
        // obeys.
        const colMinPx = minPaneSizePx(rowCount);
        const rowMinPx = minPaneSizePx(rows);

        return (
          <div key={`row-${rowIdx}`} className="flex flex-col shrink-0" style={{ height: rowHeight }}>
            <div className="flex flex-1 overflow-hidden">
              {Array.from({ length: rowCount }, (_, colIdx) => {
                const node = nodes[startIdx + colIdx];

                const colStyle: React.CSSProperties = {
                  width: `calc(${widths[colIdx] ?? 100 / rowCount}% - ${totalHandleWidthPct / rowCount}%)`,
                  flexShrink: 0,
                };

                return (
                  <Fragment key={node.id}>
                  <div className="flex" style={colStyle}>
                    <NodeCard
                      nodeId={node.id}
                      memberIds={activityMembersByRoot[node.id] ?? [node.id]}
                      isActive={node.id === activeNodeId}
                      onActivate={activateNode}
                      draggable={draggable}
                    />
                  </div>
                    {colIdx < rowCount - 1 && (
                      /* Column separator — issue #2424 made it operable by
                         keyboard. The shared `Separator` primitive carries
                         the role, orientation, name, range and the
                         arrow-key key map; `resizeColumnByKeyboard`
                         applies the same pixel floor the drag enforces and
                         persists + refits the terminals exactly like the
                         pointer path. */
                      <Separator
                        label={`Resize column ${colIdx + 1} of ${rowCount} in row ${rowIdx + 1}`}
                        orientation="vertical"
                        value={(widths[colIdx] ?? 0) / 100 * containerSize.width}
                        min={colMinPx}
                        max={Math.max(colMinPx, containerSize.width - colMinPx)}
                        onResize={next => resizeColumnByKeyboard(rowIdx, colIdx, next)}
                        onMouseDown={startDrag('col', colIdx, rowIdx)}
                        className="cursor-col-resize hover:bg-accent-cyan/30 active:bg-accent-cyan/50 transition-colors shrink-0 self-stretch rounded-sm"
                        style={{ width: SPLITTER_HANDLE_WIDTH }}
                      />
                    )}
                  </Fragment>
                );
              })}
            </div>
            {rowIdx < rows - 1 && (
              /* Row separator — the horizontal twin of the column
                 separator above. ArrowDown grows the row above the
                 handle, matching the direction the handle travels when
                 it is dragged. */
              <Separator
                label={`Resize row ${rowIdx + 1} of ${rows}`}
                orientation="horizontal"
                value={(rowHeights[rowIdx] ?? 0) / 100 * containerSize.height}
                min={rowMinPx}
                max={Math.max(rowMinPx, containerSize.height - rowMinPx)}
                onResize={next => resizeRowByKeyboard(rowIdx, next)}
                onMouseDown={startDrag('row', rowIdx)}
                className="cursor-row-resize hover:bg-accent-cyan/30 active:bg-accent-cyan/50 transition-colors shrink-0 rounded-sm mx-1"
                style={{ height: SPLITTER_HANDLE_WIDTH }}
              />
            )}
          </div>
        );
      })}
    </div>
    </div>
  );
}
