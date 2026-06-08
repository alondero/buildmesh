import { useEffect, useRef, useState, useCallback, type MouseEvent as ReactMouseEvent } from 'react';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
import { AgentTerminal, terminalManager } from '../Terminal/Terminal';
import { BuildRunTerminal } from '../Terminal/BuildRunTerminal';
import { GridNodeHeader } from './GridNodeHeader';
import { getGridLayout, equalSizes } from '../../hooks/useGridLayout';

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
  const { cols, rows } = getGridLayout(nodes.length);
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerSize, setContainerSize] = useState({ width: 1000, height: 600 });

  const [colWidths, setColWidths] = useState<number[]>(() => equalSizes(cols));
  const [rowHeights, setRowHeights] = useState<number[]>(() => equalSizes(rows));

  const colWidthsRef = useRef(colWidths);
  const rowHeightsRef = useRef(rowHeights);
  colWidthsRef.current = colWidths;
  rowHeightsRef.current = rowHeights;

  useEffect(() => { setColWidths(equalSizes(cols)); }, [cols]);
  useEffect(() => { setRowHeights(equalSizes(rows)); }, [rows]);

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
    index: number;
    startPos: number;
    startSizes: number[];
    containerSize: number;
  } | null>(null);

  const handleMouseDown = useCallback((e: ReactMouseEvent, axis: DragAxis, index: number) => {
    e.preventDefault();
    const container = containerRef.current;
    if (!container) return;
    const rect = container.getBoundingClientRect();
    const size = axis === 'col' ? rect.width : rect.height;
    const startPos = axis === 'col' ? e.clientX : e.clientY;
    const startSizes = axis === 'col' ? [...colWidthsRef.current] : [...rowHeightsRef.current];
    dragRef.current = { axis, index, startPos, startSizes, containerSize: size };
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

        if (drag.axis === 'col') setColWidths(newSizes);
        else setRowHeights(newSizes);
      });
    };

    const handleMouseUp = () => {
      if (dragRef.current) {
        dragRef.current = null;
        if (rafId !== null) { cancelAnimationFrame(rafId); rafId = null; }
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

  const totalHandleWidthPct = ((cols - 1) * HANDLE_PX / containerSize.width) * 100;
  const totalHandleHeightPct = ((rows - 1) * HANDLE_PX / containerSize.height) * 100;

  const needsScroll = rows * MIN_PANE_PX + (rows - 1) * HANDLE_PX > containerSize.height;

  return (
    <div
      ref={containerRef}
      className="flex-1 flex flex-col p-1 bg-bg-surface overflow-x-hidden"
      style={{ overflowY: needsScroll ? 'auto' : 'hidden' }}
    >
      {Array.from({ length: rows }, (_, rowIdx) => {
        const rowHeight = needsScroll
          ? `${MIN_PANE_PX}px`
          : `calc(${rowHeights[rowIdx]}% - ${totalHandleHeightPct / rows}%)`;

        return (
          <div key={`row-${rowIdx}`} className="flex flex-col" style={{ height: rowHeight, flexShrink: needsScroll ? 0 : undefined }}>
            <div className="flex flex-1 overflow-hidden">
              {Array.from({ length: cols }, (_, colIdx) => {
                const nodeIdx = rowIdx * cols + colIdx;
                if (nodeIdx >= nodes.length) return <div key={`empty-${colIdx}`} className="flex-1" />;
                const node = nodes[nodeIdx];
                const isBuildRunOpen = buildRunOpen?.nodeId === node.id ? buildRunOpen.mode : null;
                const isActive = node.id === activeNodeId;
                const borderClass = node.status === 'awaiting_input'
                  ? 'border-status-warning animate-border-pulse'
                  : isActive
                    ? 'border-accent-cyan shadow-[0_0_0_2px_var(--color-accent-cyan),0_0_16px_3px_var(--color-accent-cyan-dim)]'
                    : 'border-border-default hover:border-accent-cyan/50';

                const colStyle: React.CSSProperties = {
                  width: `calc(${colWidths[colIdx]}% - ${totalHandleWidthPct / cols}%)`,
                  flexShrink: 0,
                };

                return (
                  <div key={node.id} className="flex" style={colStyle}>
                    <div
                      onClick={() => { if (!isActive) setActiveNode(node.id); }}
                      className={`flex-1 flex flex-col bg-bg-card border-2 rounded-sm overflow-hidden group transition-colors ${borderClass}`}
                    >
                      <GridNodeHeader node={node} onBuildRun={onBuildRun} />
                      <div className="flex-1 flex flex-col overflow-hidden bg-black">
                        <div className={`${isBuildRunOpen ? 'flex-[2]' : 'flex-1'} overflow-hidden`}>
                          <AgentTerminal sessionId={node.id} />
                        </div>
                        {isBuildRunOpen && (
                          <BuildRunTerminal
                            sessionId={node.id}
                            mode={isBuildRunOpen}
                            useWorktree={node.use_worktree}
                            onClose={() => setBuildRunOpen(null)}
                          />
                        )}
                      </div>
                    </div>
                    {colIdx < cols - 1 && nodeIdx < nodes.length - 1 && (
                      <div
                        onMouseDown={(e) => handleMouseDown(e, 'col', colIdx)}
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
