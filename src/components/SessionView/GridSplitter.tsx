import { useEffect, useRef, useState, useCallback, type MouseEvent as ReactMouseEvent } from 'react';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useGridLayoutStore, resolveLayout } from '../../stores/gridLayoutStore';
import { terminalManager } from '../Terminal/Terminal';
import { NodeCard } from './NodeCard';
import { getGridRows, equalSizes } from '../../hooks/useGridLayout';

const MIN_PANE_PX = 300;
const HANDLE_PX = 5;

interface GridSplitterProps {
  nodes: AgentNode[];
  onBuildRun: (nodeId: number, mode: 'build' | 'run' | 'terminal') => void;
  buildRunOpen: { nodeId: number; mode: 'build' | 'run' | 'terminal' } | null;
  setBuildRunOpen: (val: { nodeId: number; mode: 'build' | 'run' | 'terminal' } | null) => void;
}

type DragAxis = 'col' | 'row';

export function GridSplitter({ nodes, onBuildRun, buildRunOpen, setBuildRunOpen }: GridSplitterProps) {
  const rowCounts = getGridRows(nodes.length);
  const rows = rowCounts.length;
  const rowKey = rowCounts.join(',');

  const selectedMeshId = useMeshStore(state => state.selectedMeshId);

  const containerRef = useRef<HTMLDivElement>(null);
  const [containerSize, setContainerSize] = useState({ width: 1000, height: 600 });

  // colWidths is jagged: one width array per row, so a vertical handle only
  // resizes its own row. Heights stay one-per-row. Initialise from the per-mesh
  // store, shape-validated against the current layout (falls back to equal).
  const [colWidths, setColWidths] = useState<number[][]>(
    () => resolveLayout(useGridLayoutStore.getState().byMesh[selectedMeshId!], rowCounts).colWidths,
  );
  const [rowHeights, setRowHeights] = useState<number[]>(
    () => resolveLayout(useGridLayoutStore.getState().byMesh[selectedMeshId!], rowCounts).rowHeights,
  );

  const colWidthsRef = useRef(colWidths);
  const rowHeightsRef = useRef(rowHeights);
  const selectedMeshIdRef = useRef(selectedMeshId);
  colWidthsRef.current = colWidths;
  rowHeightsRef.current = rowHeights;
  selectedMeshIdRef.current = selectedMeshId;

  // Re-load when the active mesh OR the grid shape changes. Reading via
  // getState() (not a subscription) keeps the mouse-up persist below from
  // retriggering this effect into a loop.
  useEffect(() => {
    const l = resolveLayout(useGridLayoutStore.getState().byMesh[selectedMeshId!], getGridRows(nodes.length));
    setColWidths(l.colWidths);
    setRowHeights(l.rowHeights);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedMeshId, rowKey]);

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

  const dragRef = useRef<{
    axis: DragAxis;
    row: number;       // which row's colWidths to edit (col axis only)
    index: number;     // separator index within that array
    startPos: number;
    startSizes: number[];
    containerSize: number;
  } | null>(null);

  const handleMouseDown = useCallback((e: ReactMouseEvent, axis: DragAxis, index: number, row = 0) => {
    e.preventDefault();
    const container = containerRef.current;
    if (!container) return;
    const rect = container.getBoundingClientRect();
    const size = axis === 'col' ? rect.width : rect.height;
    const startPos = axis === 'col' ? e.clientX : e.clientY;
    const startSizes = axis === 'col' ? [...colWidthsRef.current[row]] : [...rowHeightsRef.current];
    dragRef.current = { axis, row, index, startPos, startSizes, containerSize: size };
  }, []);

  useEffect(() => {
    let rafId: number | null = null;
    let lastX = 0;
    let lastY = 0;

    const handleMouseMove = (e: MouseEvent) => {
      if (!dragRef.current) return;
      lastX = e.clientX;
      lastY = e.clientY;
      if (rafId !== null) return;
      rafId = requestAnimationFrame(() => {
        rafId = null;
        const drag = dragRef.current;
        if (!drag) return;
        const currentPos = drag.axis === 'col' ? lastX : lastY;
        const deltaPct = ((currentPos - drag.startPos) / drag.containerSize) * 100;

        const newSizes = [...drag.startSizes];
        const i = drag.index;
        const minPct = (MIN_PANE_PX / drag.containerSize) * 100;

        let newLeft = newSizes[i] + deltaPct;
        let newRight = newSizes[i + 1] - deltaPct;

        if (newLeft < minPct) { newLeft = minPct; newRight = drag.startSizes[i] + drag.startSizes[i + 1] - minPct; }
        if (newRight < minPct) { newRight = minPct; newLeft = drag.startSizes[i] + drag.startSizes[i + 1] - minPct; }

        newSizes[i] = newLeft;
        newSizes[i + 1] = newRight;

        if (drag.axis === 'col') setColWidths(prev => prev.map((r, idx) => idx === drag.row ? newSizes : r));
        else setRowHeights(newSizes);
      });
    };

    const handleMouseUp = () => {
      if (dragRef.current) {
        dragRef.current = null;
        if (rafId !== null) { cancelAnimationFrame(rafId); rafId = null; }
        const meshId = selectedMeshIdRef.current;
        if (meshId != null) {
          useGridLayoutStore.getState().setLayout(meshId, {
            colWidths: colWidthsRef.current,
            rowHeights: rowHeightsRef.current,
          });
        }
        terminalManager.fitAll();
      }
    };

    document.addEventListener('mousemove', handleMouseMove);
    document.addEventListener('mouseup', handleMouseUp);
    return () => {
      document.removeEventListener('mousemove', handleMouseMove);
      document.removeEventListener('mouseup', handleMouseUp);
      if (rafId !== null) cancelAnimationFrame(rafId);
    };
  }, []);

  const activeNodeId = useAgentNodeStore(state => state.activeNodeId);
  const setActiveNode = useAgentNodeStore(state => state.setActiveNode);

  const totalHandleHeightPct = ((rows - 1) * HANDLE_PX / containerSize.height) * 100;

  const needsScroll = rows * MIN_PANE_PX + (rows - 1) * HANDLE_PX > containerSize.height;

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
        const totalHandleWidthPct = ((rowCount - 1) * HANDLE_PX / containerSize.width) * 100;
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
                      node={node}
                      isActive={node.id === activeNodeId}
                      onActivate={setActiveNode}
                      onBuildRun={onBuildRun}
                      buildRunOpen={buildRunOpen}
                      setBuildRunOpen={setBuildRunOpen}
                    />
                    {colIdx < rowCount - 1 && (
                      <div
                        onMouseDown={(e) => handleMouseDown(e, 'col', colIdx, rowIdx)}
                        className="cursor-col-resize hover:bg-accent-cyan/30 active:bg-accent-cyan/50 transition-colors shrink-0 self-stretch rounded-sm"
                        style={{ width: HANDLE_PX }}
                      />
                    )}
                  </div>
                );
              })}
            </div>
            {rowIdx < rows - 1 && (
              <div
                onMouseDown={(e) => handleMouseDown(e, 'row', rowIdx)}
                className="cursor-row-resize hover:bg-accent-cyan/30 active:bg-accent-cyan/50 transition-colors shrink-0 rounded-sm mx-1"
                style={{ height: HANDLE_PX }}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}
