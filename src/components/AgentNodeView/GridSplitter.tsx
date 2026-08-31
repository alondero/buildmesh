import { useEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from 'react';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { useGridLayoutStore, resolveLayout } from '../../stores/gridLayoutStore';
import { terminalManager } from '../Terminal/Terminal';
import { NodeCard } from './NodeCard';
import { getGridRows, equalSizes } from '../../hooks/useGridLayout';
import { useResizable, SPLITTER_HANDLE_WIDTH } from '../../hooks/useResizable';

const MIN_PANE_PX = 300;

/**
 * Pull the i-th and (i+1)-th cells of `baseline` toward `+delta` and
 * `-delta` respectively, clamped to `minPct` on either side (when one
 * side hits the floor the other soaks up the slack so left+right stays
 * constant). Used by both the col-axis and row-axis `compute`s in
 * `GridSplitter`; collapses ~17 lines of duplicated math into one helper
 * so future bugfixes land in a single place.
 */
function clampResizeAdjacentPair(baseline: number[], i: number, deltaPct: number, minPct: number): number[] {
  let newLeft = baseline[i] + deltaPct;
  let newRight = baseline[i + 1] - deltaPct;
  if (newLeft < minPct) { newLeft = minPct; newRight = baseline[i] + baseline[i + 1] - minPct; }
  if (newRight < minPct) { newRight = minPct; newLeft = baseline[i] + baseline[i + 1] - minPct; }
  const next = [...baseline];
  next[i] = newLeft;
  next[i + 1] = newRight;
  return next;
}

interface GridSplitterProps {
  nodes: AgentNode[];
  onBuildRun: (nodeId: number, mode: 'build' | 'run' | 'terminal') => void;
  buildRunOpen: { nodeId: number; mode: 'build' | 'run' | 'terminal' } | null;
  setBuildRunOpen: (val: { nodeId: number; mode: 'build' | 'run' | 'terminal' } | null) => void;
  // Pinned Grid mode disables card drag-reorder (wayfinder #982 / #986 —
  // custom pinned ordering is map fog). Defaults to draggable so Mesh/All
  // keep today's behaviour.
  draggable?: boolean;
}

type DragAxis = 'col' | 'row';

export function GridSplitter({ nodes, onBuildRun, buildRunOpen, setBuildRunOpen, draggable = true }: GridSplitterProps) {
  const rowCounts = getGridRows(nodes.length);
  const rows = rowCounts.length;
  const rowKey = rowCounts.join(',');

  const selectedMeshId = useMeshStore(state => state.selectedMeshId);
  // Layout persistence is mesh-scoped: only Mesh Grid mode saves/restores
  // under the selected mesh's key (wayfinder #982 / #986). All Nodes and
  // Pinned render cross-mesh sets whose arrangement must not pollute any
  // single mesh's saved layout — they get session-local equal splits, the
  // same behaviour 'all' already had via a null selection.
  const viewMode = useUIStore(state => state.viewMode);
  const layoutMeshId = viewMode === 'mesh' ? selectedMeshId : null;

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
    const observer = new ResizeObserver(([entry]) => {
      const w = entry.contentRect.width;
      const h = entry.contentRect.height;
      setContainerSize(prev => (prev.width === w && prev.height === h) ? prev : { width: w, height: h });
    });
    observer.observe(el);
    const rect = el.getBoundingClientRect();
    setContainerSize({ width: rect.width, height: rect.height });
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
   * Persist the most recent widths/heights to the per-mesh layout store,
   * then ask xterm to re-measure every terminal. Called on mouseup by both
   * the col and row drag hooks via their shared onEnd helper.
   */
  const persistAndFit = () => {
    const meshId = layoutMeshIdRef.current;
    if (meshId != null) {
      useGridLayoutStore.getState().setLayout(meshId, {
        colWidths: colWidthsRef.current,
        rowHeights: rowHeightsRef.current,
      });
    }
    terminalManager.fitAll();
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
      const minPct = (MIN_PANE_PX / containerSize) * 100;
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
      const minPct = (MIN_PANE_PX / containerSize) * 100;
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
    if (el) {
      const rect = el.getBoundingClientRect();
      dragContainerSizeRef.current = { width: rect.width, height: rect.height };
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
  const setActiveNode = useAgentNodeStore(state => state.setActiveNode);

  const totalHandleHeightPct = ((rows - 1) * SPLITTER_HANDLE_WIDTH / containerSize.height) * 100;

  const needsScroll = rows * MIN_PANE_PX + (rows - 1) * SPLITTER_HANDLE_WIDTH > containerSize.height;

  return (
    <div
      ref={containerRef}
      className="flex-1 flex flex-col p-1 bg-bg-surface overflow-x-hidden"
      style={{ overflowY: needsScroll ? 'auto' : 'hidden' }}
    >
      {rowCounts.map((rowCount, rowIdx) => {
        const rowHeight = needsScroll
          ? `${MIN_PANE_PX}px`
          : `calc(${rowHeights[rowIdx] ?? 100 / rows}% - ${totalHandleHeightPct / rows}%)`;

        const widths = colWidths[rowIdx] ?? equalSizes(rowCount);
        const totalHandleWidthPct = ((rowCount - 1) * SPLITTER_HANDLE_WIDTH / containerSize.width) * 100;
        const startIdx = rowCounts.slice(0, rowIdx).reduce((a, b) => a + b, 0);

        return (
          <div key={`row-${rowIdx}`} className="flex flex-col" style={{ height: rowHeight, flexShrink: needsScroll ? 0 : undefined }}>
            <div className="flex flex-1 overflow-hidden">
              {Array.from({ length: rowCount }, (_, colIdx) => {
                const node = nodes[startIdx + colIdx];

                const colStyle: React.CSSProperties = {
                  width: `calc(${widths[colIdx] ?? 100 / rowCount}% - ${totalHandleWidthPct / rowCount}%)`,
                  flexShrink: 0,
                };

                return (
                  <div key={node.id} className="flex" style={colStyle}>
                    <NodeCard
                      nodeId={node.id}
                      isActive={node.id === activeNodeId}
                      onActivate={setActiveNode}
                      onBuildRun={onBuildRun}
                      buildRunOpen={buildRunOpen}
                      setBuildRunOpen={setBuildRunOpen}
                      draggable={draggable}
                    />
                    {colIdx < rowCount - 1 && (
                      <div
                        onMouseDown={startDrag('col', colIdx, rowIdx)}
                        className="cursor-col-resize hover:bg-accent-cyan/30 active:bg-accent-cyan/50 transition-colors shrink-0 self-stretch rounded-sm"
                        style={{ width: SPLITTER_HANDLE_WIDTH }}
                      />
                    )}
                  </div>
                );
              })}
            </div>
            {rowIdx < rows - 1 && (
              <div
                onMouseDown={startDrag('row', rowIdx)}
                className="cursor-row-resize hover:bg-accent-cyan/30 active:bg-accent-cyan/50 transition-colors shrink-0 rounded-sm mx-1"
                style={{ height: SPLITTER_HANDLE_WIDTH }}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}
