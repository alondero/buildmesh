import { Fragment, useEffect, useMemo, useState, useRef, type MouseEvent as ReactMouseEvent } from 'react';
import {
  DndContext, DragOverlay, PointerSensor, useSensor, useSensors, pointerWithin,
  type DragStartEvent, type DragMoveEvent, type DragEndEvent,
} from '@dnd-kit/core';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { terminalManager } from '../Terminal/Terminal';
import { watchSession, unwatchSession } from '../../lib/tauri';
import { GridSplitter } from './GridSplitter';
import { CenterDiffOverlay } from './CenterDiffOverlay';
import { NodeCard, type BuildRunState } from './NodeCard';
import { DropIntentContext, NodeDragPreview, computeDropIntent, type DropIntent } from './nodeDrag';
import { equalSizes } from '../../hooks/useGridLayout';

const MIN_PANE_PERCENT = 15;
const RESIZE_HANDLE_WIDTH = 4;

interface ResizablePanesProps {
  nodes: AgentNode[];
  onBuildRun: (nodeId: number, mode: 'build' | 'run' | 'terminal') => void;
  buildRunOpen: { nodeId: number; mode: 'build' | 'run' | 'terminal' } | null;
  setBuildRunOpen: (val: { nodeId: number; mode: 'build' | 'run' | 'terminal' } | null) => void;
}

function ResizablePanes({ nodes, onBuildRun, buildRunOpen, setBuildRunOpen }: ResizablePanesProps) {
  const [widths, setWidths] = useState(() => equalSizes(nodes.length));
  const resizingRef = useRef(false);
  const startXRef = useRef(0);
  const startWidthsRef = useRef<number[]>([]);
  const dragIndexRef = useRef<number>(0);
  const containerRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    setWidths(equalSizes(nodes.length));
  }, [nodes.length]);

  const handleResizeMouseDown = (e: ReactMouseEvent, index: number) => {
    e.preventDefault();
    resizingRef.current = true;
    dragIndexRef.current = index;
    startXRef.current = e.clientX;
    startWidthsRef.current = [...widths];
    containerRef.current = document.getElementById('grid-panes-container');
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
  };

  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      if (!resizingRef.current) return;
      const container = containerRef.current;
      if (!container) return;
      const containerWidth = container.getBoundingClientRect().width;
      const deltaPercent = ((e.clientX - startXRef.current) / containerWidth) * 100;

      const i = dragIndexRef.current;
      const leftOrig = startWidthsRef.current[i];
      const rightOrig = startWidthsRef.current[i + 1];
      const clamped = Math.max(MIN_PANE_PERCENT - leftOrig, Math.min(deltaPercent, rightOrig - MIN_PANE_PERCENT));

      const newLeft = leftOrig + clamped;
      const newRight = rightOrig - clamped;
      setWidths(prev => {
        if (prev[i] === newLeft) return prev;
        const next = [...startWidthsRef.current];
        next[i] = newLeft;
        next[i + 1] = newRight;
        return next;
      });
    };

    const handleMouseUp = () => {
      if (resizingRef.current) {
        resizingRef.current = false;
        containerRef.current = null;
        document.body.style.cursor = '';
        document.body.style.userSelect = '';
        terminalManager.fitAll();
      }
    };

    document.addEventListener('mousemove', handleMouseMove);
    document.addEventListener('mouseup', handleMouseUp);
    return () => {
      document.removeEventListener('mousemove', handleMouseMove);
      document.removeEventListener('mouseup', handleMouseUp);
    };
  }, []);

  const activeNodeId = useAgentNodeStore(state => state.activeNodeId);
  const setActiveNode = useAgentNodeStore(state => state.setActiveNode);
  const isMultiPane = nodes.length > 1;

  return (
    <div
      id="grid-panes-container"
      className={`flex-1 flex overflow-hidden ${isMultiPane ? 'p-1 bg-bg-surface' : 'flex-col'}`}
    >
      {nodes.map((node, idx) => {
        return (
          <Fragment key={node.id}>
            <div
              className="flex flex-col overflow-hidden"
              style={isMultiPane ? { width: `${widths[idx]}%`, flex: '0 0 auto' } : { flex: '1 1 0%' }}
            >
              <NodeCard
                node={node}
                isActive={node.id === activeNodeId}
                onActivate={setActiveNode}
                onBuildRun={onBuildRun}
                buildRunOpen={buildRunOpen}
                setBuildRunOpen={setBuildRunOpen}
              />
            </div>
            {isMultiPane && idx < nodes.length - 1 && (
              <div
                onMouseDown={(e) => handleResizeMouseDown(e, idx)}
                className="cursor-col-resize hover:bg-accent-cyan/30 active:bg-accent-cyan/50 transition-colors shrink-0 self-stretch rounded-sm"
                style={{ width: RESIZE_HANDLE_WIDTH }}
              />
            )}
          </Fragment>
        );
      })}
    </div>
  );
}

export function SessionView() {
  const selectedMeshId = useMeshStore(state => state.selectedMeshId);
  // Granular selectors: subscribing to the whole store (useAgentNodeStore())
  // re-rendered SessionView on every unrelated change — including each
  // attention status flip — even though only agentNodes/activeNodeId affect
  // this view.
  const agentNodes = useAgentNodeStore(state => state.agentNodes);
  const activeNodeId = useAgentNodeStore(state => state.activeNodeId);
  const setActiveNode = useAgentNodeStore(state => state.setActiveNode);
  const reorderAgentNode = useAgentNodeStore(state => state.reorderAgentNode);
  const swapAgentNodes = useAgentNodeStore(state => state.swapAgentNodes);

  const activeNode = useMemo(
    () => agentNodes.find(s => s.id === activeNodeId) ?? null,
    [agentNodes, activeNodeId],
  );

  const maximizedNodeId = useUIStore(state => state.maximizedNodeId);
  const clearMaximizedNode = useUIStore(state => state.clearMaximizedNode);
  const probeOpen = useUIStore(state => state.probeOpen);
  const activeDiffFile = useUIStore(state => state.activeDiffFile);
  const [openBuildRun, setOpenBuildRun] = useState<BuildRunState>(null);

  const filteredNodes = useMemo(() => {
    if (selectedMeshId === null) {
      return agentNodes;
    }
    return agentNodes.filter(s => s.mesh_id === selectedMeshId);
  }, [agentNodes, selectedMeshId]);

  // The node to solo, if maximize is active AND that node is visible in the
  // current mesh filter. Deriving it (rather than trusting maximizedNodeId
  // blindly) means a node that's closed or filtered out simply falls back to
  // the grid — the auto-clear effect below then tidies the stale id.
  const maximizedNode = useMemo(
    () => (maximizedNodeId == null ? null : filteredNodes.find(n => n.id === maximizedNodeId) ?? null),
    [maximizedNodeId, filteredNodes],
  );

  useEffect(() => {
    if (!activeNode) return;
    watchSession(activeNode.id).catch(console.error);
    return () => {
      unwatchSession(activeNode.id).catch(console.error);
    };
  // cli_session_id is set after spawn — re-watch so the watcher picks up the newly created worktree
  }, [activeNode?.id, activeNode?.cli_session_id]);

  // Auto-select first node when switching to a mesh that doesn't include the active node
  useEffect(() => {
    if (filteredNodes.length > 0 && activeNode && !filteredNodes.find(s => s.id === activeNode.id)) {
      setActiveNode(filteredNodes[0].id);
    }
  }, [selectedMeshId, filteredNodes, activeNode, setActiveNode]);

  // Fit terminal when active node changes (e.g. container might have resized)
  useEffect(() => {
    if (activeNode) {
      terminalManager.fit(activeNode.id);
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeNode?.id]);

  // Escape exits maximize. Only bound while something is maximized so we don't
  // intercept Escape (e.g. agent CLIs read it) during normal grid use. While
  // the Center Diff Overlay (#379) is open it sits on top of the maximized
  // terminal and owns Escape — without this guard, Escape would close the
  // overlay AND un-maximize in one press. When the overlay closes, this effect
  // re-runs (activeDiffFile dep) and re-binds the maximize handler.
  useEffect(() => {
    if (maximizedNode == null || activeDiffFile != null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') clearMaximizedNode();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [maximizedNode, activeDiffFile, clearMaximizedNode]);

  // If the maximized node disappears (closed, or filtered out by a mesh
  // switch), drop the stale id so it doesn't suppress the grid on return.
  useEffect(() => {
    if (maximizedNodeId != null && maximizedNode == null) {
      clearMaximizedNode();
    }
  }, [maximizedNodeId, maximizedNode, clearMaximizedNode]);

  // Reflow the terminal grid whenever we enter or leave maximize: the soloed
  // terminal grows to fill the area, and on restore every pane shrinks back.
  // fitAll covers both directions. Never dispose — the singleton survives this.
  useEffect(() => {
    terminalManager.fitAll();
  }, [maximizedNode?.id]);

  // Refit when the Probe Panel expands/collapses: it shrinks/grows this view's
  // width via the App flex row, but xterm doesn't re-measure on flex reflow on
  // its own, so terminals would keep a stale column count (#374). Same
  // never-dispose contract as the maximize refit above.
  useEffect(() => {
    terminalManager.fitAll();
  }, [probeOpen]);

  // After a drag reorder/swap, nodes move into slots that may be a different
  // size, so refit. Keyed on the id sequence so it fires only when order
  // actually changes (status flips reuse ids → no refit). Never disposes.
  const orderKey = useMemo(() => filteredNodes.map(n => n.id).join(','), [filteredNodes]);
  useEffect(() => {
    terminalManager.fitAll();
  }, [orderKey]);

  // --- Drag-to-reorder / swap -------------------------------------------------
  // The title bar is the drag handle; collision is pointer-based (pointerWithin)
  // so the xterm canvas in a node body never blocks a drop. Drop intent (insert
  // vs swap) is decided from where the pointer sits across the target node.
  const [activeDragNodeId, setActiveDragNodeId] = useState<number | null>(null);
  const [dropIntent, setDropIntent] = useState<DropIntent>(null);
  const activatorXRef = useRef(0);
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 6 } }),
  );

  const activeDragNode = useMemo(
    () => (activeDragNodeId == null ? null : agentNodes.find(n => n.id === activeDragNodeId) ?? null),
    [activeDragNodeId, agentNodes],
  );

  const intentFromEvent = (e: DragMoveEvent | DragEndEvent): DropIntent => {
    const data = e.active.data.current as { nodeId: number; meshId: number } | undefined;
    if (!data) return null;
    const overData = e.over?.data.current as { nodeId: number; meshId: number } | undefined;
    return computeDropIntent({
      overNodeId: overData?.nodeId ?? null,
      overMeshId: overData?.meshId ?? null,
      overRectLeft: e.over?.rect.left ?? 0,
      overRectWidth: e.over?.rect.width ?? 0,
      pointerX: activatorXRef.current + e.delta.x,
      draggedId: data.nodeId,
      draggedMeshId: data.meshId,
    });
  };

  const handleDragStart = (e: DragStartEvent) => {
    const data = e.active.data.current as { nodeId: number; meshId: number } | undefined;
    if (!data) return;
    activatorXRef.current = (e.activatorEvent as PointerEvent).clientX ?? 0;
    setActiveDragNodeId(data.nodeId);
    setDropIntent(null);
  };

  const handleDragMove = (e: DragMoveEvent) => {
    setDropIntent(intentFromEvent(e));
  };

  const handleDragEnd = (e: DragEndEvent) => {
    const data = e.active.data.current as { nodeId: number; meshId: number } | undefined;
    const intent = intentFromEvent(e);
    setActiveDragNodeId(null);
    setDropIntent(null);
    if (!data || !intent) return;
    if (intent.kind === 'swap') {
      swapAgentNodes(data.nodeId, intent.targetNodeId);
      return;
    }
    const meshNodes = useAgentNodeStore.getState().agentNodes
      .filter(n => n.mesh_id === data.meshId)
      .sort((a, b) => a.position - b.position);
    const targetIdx = meshNodes.findIndex(n => n.id === intent.targetNodeId);
    if (targetIdx === -1) return;
    reorderAgentNode(data.nodeId, intent.kind === 'insert-before' ? targetIdx : targetIdx + 1);
  };

  const handleDragCancel = () => {
    setActiveDragNodeId(null);
    setDropIntent(null);
  };

  return (
    <div className="relative flex-1 flex flex-col h-full bg-bg-base overflow-hidden">
      {/* Center Workspace Diff Overlay (#379) — covers the terminal grid with a
          spacious single-file diff when a changed file is clicked in the Probe.
          Absolutely positioned over this view only, so the right-hand Probe
          (a sibling in App's flex row) stays open and interactive. The
          terminals behind it keep running; "Back to Terminals" just hides it. */}
      {activeDiffFile && <CenterDiffOverlay diff={activeDiffFile} />}
      <div className="flex-1 flex overflow-hidden">
        <DndContext
          sensors={sensors}
          collisionDetection={pointerWithin}
          onDragStart={handleDragStart}
          onDragMove={handleDragMove}
          onDragEnd={handleDragEnd}
          onDragCancel={handleDragCancel}
        >
        <DropIntentContext.Provider value={dropIntent}>
        <div className="flex-1 flex overflow-hidden">
          {maximizedNode ? (
            <div className="flex-1 flex flex-col p-1 bg-bg-surface overflow-hidden">
              <NodeCard
                node={maximizedNode}
                isActive={maximizedNode.id === activeNodeId}
                onActivate={setActiveNode}
                onBuildRun={(nodeId, mode) => setOpenBuildRun({ nodeId, mode })}
                buildRunOpen={openBuildRun}
                setBuildRunOpen={setOpenBuildRun}
                draggable={false}
              />
            </div>
          ) : filteredNodes.length === 0 ? (
            <div className="flex-1 flex items-center justify-center text-text-muted">
              <div className="text-center max-w-sm">
                <p className="text-xl mb-2 text-text-primary font-sans font-semibold">Buildmesh</p>
                <p className="text-sm text-text-secondary mb-6 font-sans">Orchestrate AI agents across your projects. Add a mesh to point at a project folder, then spawn agents to work in parallel.</p>
                <button
                  onClick={() => useMeshStore.getState().addMesh()}
                  className="inline-flex items-center gap-2 px-4 py-2 rounded-md bg-accent-cyan/10 text-accent-cyan font-sans font-medium text-sm hover:bg-accent-cyan/20 transition-colors border border-accent-cyan/20"
                >
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/>
                  </svg>
                  Add Mesh
                </button>
                <div className="mt-8 text-[11px] text-text-muted font-mono space-y-1">
                  <p><kbd className="px-1 py-0.5 rounded bg-bg-card border border-border-default">Cmd+T</kbd> New agent node</p>
                  <p><kbd className="px-1 py-0.5 rounded bg-bg-card border border-border-default">Cmd+1-9</kbd> Switch nodes</p>
                </div>
              </div>
            </div>
          ) : filteredNodes.length <= 2 ? (
            <ResizablePanes
              nodes={filteredNodes}
              onBuildRun={(nodeId, mode) => setOpenBuildRun({ nodeId, mode })}
              buildRunOpen={openBuildRun}
              setBuildRunOpen={setOpenBuildRun}
            />
          ) : (
            <GridSplitter
              nodes={filteredNodes}
              onBuildRun={(nodeId, mode) => setOpenBuildRun({ nodeId, mode })}
              buildRunOpen={openBuildRun}
              setBuildRunOpen={setOpenBuildRun}
            />
          )}
        </div>
        </DropIntentContext.Provider>
        <DragOverlay dropAnimation={null}>
          {activeDragNode ? <NodeDragPreview node={activeDragNode} /> : null}
        </DragOverlay>
        </DndContext>
      </div>
    </div>
  );
}