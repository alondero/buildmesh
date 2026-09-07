import { Fragment, useEffect, useMemo, useState, useRef } from 'react';
import {
  DndContext, DragOverlay, PointerSensor, useSensor, useSensors, pointerWithin,
  type DragStartEvent, type DragMoveEvent, type DragEndEvent,
} from '@dnd-kit/core';
import { useAgentNodeStore, useAllAgentNodes, type AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { terminalManager } from '../Terminal/Terminal';
import { watchAgentNode, unwatchAgentNode } from '../../lib/tauri';
import { GridSplitter } from './GridSplitter';
import { resolveSingleNode } from '../../lib/viewModes';
import { deriveVisibleNodes } from './gridFilterSort';
import { CenterDiffOverlay } from './CenterDiffOverlay';
import { CircuitEditorOverlay } from '../Circuits/CircuitEditorOverlay';
import { NodeCard } from './NodeCard';
import { activityMemberIds, activityRootId } from '../../lib/nodeActivities';
import { useNodeActivityStore } from '../../stores/nodeActivityStore';
import { DropIntentContext, NodeDragPreview, computeDropIntent, type DropIntent } from './nodeDrag';
import { equalSizes } from '../../hooks/useGridLayout';
import { useResizable, SPLITTER_HANDLE_WIDTH } from '../../hooks/useResizable';
import { CanvasEmptyStateContainer } from './CanvasEmptyStateContainer';

const MIN_PANE_PERCENT = 15;

interface ResizablePanesProps {
  nodes: AgentNode[];
  activityMembersByRoot: Readonly<Record<number, readonly number[]>>;
  // Pinned Grid mode disables card drag-reorder (wayfinder #982 / #986).
  draggable?: boolean;
}

function ResizablePanes({ nodes, activityMembersByRoot, draggable = true }: ResizablePanesProps) {
  const [widths, setWidths] = useState(() => equalSizes(nodes.length));
  // `idxRef` records which separator the user clicked, since the shared
  // `useResizable` hook fires `handleMouseDown` without knowing which divider
  // it came from. The ref is read inside the `compute` callback on every
  // mousemove. `containerRef` is the flex row's own div — used to
  // measure width on demand inside the compute, no `getElementById` lookup.
  const idxRef = useRef<number>(0);
  const containerRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    setWidths(equalSizes(nodes.length));
  }, [nodes.length]);

  // `compute` is called by the shared hook on every mousemove. The hook has
  // already snapshotted the baseline `widths` at mousedown and the signed
  // pointer delta along the col axis — we only need to apply the MIN clamp
  // and copy out a new array. The same-value short-circuit (`prev[i] ===
  // newLeft`) moves into the onChange wrapper so React still skips the
  // re-render when the divider hasn't moved a renderable amount.
  const { handleMouseDown } = useResizable<number[]>({
    value: widths,
    axis: 'col',
    throttle: 'sync',
    lockBody: true,
    compute: (baseline, deltaPx) => {
      const containerWidth = containerRef.current?.getBoundingClientRect().width;
      if (!containerWidth) return baseline;
      const deltaPercent = (deltaPx / containerWidth) * 100;
      const i = idxRef.current;
      const leftOrig = baseline[i];
      const rightOrig = baseline[i + 1];
      const clamped = Math.max(
        MIN_PANE_PERCENT - leftOrig,
        Math.min(deltaPercent, rightOrig - MIN_PANE_PERCENT),
      );
      const newLeft = leftOrig + clamped;
      const newRight = rightOrig - clamped;
      const next = [...baseline];
      next[i] = newLeft;
      next[i + 1] = newRight;
      return next;
    },
    onChange: (next: number[]) => setWidths((prev) => {
      const i = idxRef.current;
      if (prev[i] === next[i]) return prev; // identical — skip the render.
      return next;
    }),
    onEnd: () => terminalManager.fitAll(),
  });

  const onHandleMouseDown = (e: React.MouseEvent, index: number) => {
    idxRef.current = index;
    handleMouseDown(e);
  };

  const activeNodeId = useAgentNodeStore(state => state.activeNodeId);
  const setActiveNode = useAgentNodeStore(state => state.setActiveNode);
  const isMultiPane = nodes.length > 1;

  return (
    <div
      ref={containerRef}
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
                nodeId={node.id}
                memberIds={activityMembersByRoot[node.id] ?? [node.id]}
                isActive={node.id === activeNodeId}
                onActivate={setActiveNode}
                draggable={draggable}
              />
            </div>
            {isMultiPane && idx < nodes.length - 1 && (
              <div
                onMouseDown={(e) => onHandleMouseDown(e, idx)}
                className="cursor-col-resize hover:bg-accent-cyan/30 active:bg-accent-cyan/50 transition-colors shrink-0 self-stretch rounded-sm"
                style={{ width: SPLITTER_HANDLE_WIDTH }}
              />
            )}
          </Fragment>
        );
      })}
    </div>
  );
}

// The legacy `NoNodesSplash` ("Add Mesh" splash), `gridEmptyState` mode
// dispatcher (issues #986 / #1609), `FilteredEmptyState` (#1609), and
// standalone `PinnedEmptyState` (wayfinder #982) were all removed in
// issue #1536 and replaced by the context-aware `CanvasEmptyState`'s
// five branches. The dedicated `tests/unit/pinned-empty-state.test.tsx`
// was deleted alongside — `CanvasEmptyState`'s pinned-empty branch is
// covered by `tests/unit/canvas-empty-state.test.tsx`.


export function AgentNodeView() {
  const selectedMeshId = useMeshStore(state => state.selectedMeshId);
  // Granular selectors: subscribing to the whole store (useAgentNodeStore())
  // re-rendered the view on every unrelated change — including each
  // attention status flip — even though only agentNodes/activeNodeId affect
  // this view. Issue #1384 — `useAllAgentNodes` is the canonical derived
  // selector (useShallow over the nodeIds + nodesById pair). Children that
  // consume a single node (`GridNodeHeader`, `AgentTerminal`) already
  // subscribe to `state.nodesById[id]` directly, so this view's own
  // re-render no longer cascades into them when an unrelated node
  // changes.
  const agentNodes = useAllAgentNodes();
  const activeNodeId = useAgentNodeStore(state => state.activeNodeId);
  const nodesById = useAgentNodeStore(state => state.nodesById);
  const ownerships = useAgentNodeStore(state => state.circuitOwnerships);
  const activeRootId = activeNodeId === null ? null : activityRootId(activeNodeId, nodesById, ownerships);
  const activityMembersByRoot = useMemo(
    () => activityMemberIds(agentNodes, ownerships),
    [agentNodes, ownerships],
  );
  useEffect(() => {
    useNodeActivityStore.getState().prune(
      new Set(agentNodes.filter(node => node.status !== 'archived').map(node => node.id)),
    );
  }, [agentNodes]);
  const setActiveNode = useAgentNodeStore(state => state.setActiveNode);
  const reorderAgentNode = useAgentNodeStore(state => state.reorderAgentNode);
  const swapAgentNodes = useAgentNodeStore(state => state.swapAgentNodes);

  const activeNode = useMemo(
    () => (activeNodeId === null ? null : nodesById[activeNodeId] ?? null),
    [nodesById, activeNodeId],
  );

  // View Modes (wayfinder #982): 'single' solos the active node (it subsumes
  // the old maximizedNodeId); the grid modes derive their visible set from
  // the pure helpers in src/lib/viewModes.ts so keyboard traversal (#987)
  // reads the same definition.
  const viewMode = useUIStore(state => state.viewMode);
  const lastNonSingleMode = useUIStore(state => state.lastNonSingleMode);
  const exitSingleMode = useUIStore(state => state.exitSingleMode);
  const probeOpen = useUIStore(state => state.probeOpen);
  const activeDiffFile = useUIStore(state => state.activeDiffFile);
  const circuitEditorOpen = useUIStore(state => state.activeCircuitEditorId !== null);
  const gridSearchQuery = useUIStore(state => state.gridSearchQuery);
  const gridProviderFilter = useUIStore(state => state.gridProviderFilter);
  const gridStatusFilter = useUIStore(state => state.gridStatusFilter);
  const gridSortBy = useUIStore(state => state.gridSortBy);
  const gridSortDirection = useUIStore(state => state.gridSortDirection);

  // The ordered nodes the active grid mode renders. 'single' is not a grid
  // mode — it renders `singleNode` below instead, so its list stays empty.
  const visibleNodes = useMemo(
    () => deriveVisibleNodes(
      viewMode,
      agentNodes,
      selectedMeshId,
      activeNodeId,
      { gridSearchQuery, gridProviderFilter, gridStatusFilter, gridSortBy, gridSortDirection },
      ownerships,
    ),
    [
      viewMode,
      agentNodes,
      selectedMeshId,
      activeNodeId,
      gridSearchQuery,
      gridProviderFilter,
      gridStatusFilter,
      gridSortBy,
      gridSortDirection,
      ownerships,
    ],
  );

  // The node Single mode solos: the active node regardless of mesh scope
  // (explicit focus wins — cross-mesh by nature, like Pinned), else the
  // first node of the scope single was entered from, else any node. Unlike
  // the old maximize derivation there is NO visibility check against a mesh
  // filter and no auto-clear effect — ticket #983 deleted both.
  const singleNode = useMemo(
    () => {
      if (viewMode !== 'single') return null;
      const resolved = resolveSingleNode(agentNodes, activeNodeId, lastNonSingleMode, selectedMeshId, {
          gridSearchQuery,
          gridProviderFilter,
          gridStatusFilter,
        });
      return resolved ? nodesById[activityRootId(resolved.id, nodesById, ownerships)] ?? resolved : null;
    },
    // Controls shape (not each field) keeps the memo dep list stable and the
    // Filtered fallback controls-aware (`resolveSingleNode` narrows by them).
    [viewMode, agentNodes, activeNodeId, nodesById, ownerships, lastNonSingleMode, selectedMeshId, gridSearchQuery, gridProviderFilter, gridStatusFilter],
  );

  useEffect(() => {
    if (!activeNode) return;
    watchAgentNode(activeNode.id).catch(console.error);
    return () => {
      unwatchAgentNode(activeNode.id).catch(console.error);
    };
  // cli_session_id is set after spawn — re-watch so the watcher picks up the newly created worktree
  }, [activeNode?.id, activeNode?.cli_session_id]);

  // Grid-mode invariant (ticket #986): the active node is always one of the
  // visible nodes — switching mesh, or unpinning the active node in Pinned
  // mode, auto-selects the first visible node so focus/highlight/watch never
  // strand on an off-grid node. Single mode is exempt: it derives FROM the
  // active node rather than constraining it.
  useEffect(() => {
    if (viewMode === 'single') return;
    if (visibleNodes.length > 0 && activeNode && !visibleNodes.find(s => s.id === activeRootId)) {
      setActiveNode(visibleNodes[0].id);
    }
  }, [viewMode, visibleNodes, activeNode, activeRootId, setActiveNode]);

  // Fit terminal when active node changes (e.g. container might have resized)
  useEffect(() => {
    if (activeNode) {
      terminalManager.fit(activeNode.id);
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeNode?.id]);

  // Escape exits Single mode. Only bound while single is active so we don't
  // intercept Escape (e.g. agent CLIs read it) during normal grid use. While
  // the Center Diff Overlay (#379) or the Circuit Editor (#1209) is open it
  // sits on top of the solo terminal and owns Escape — without this guard,
  // Escape would close the overlay AND exit single in one press. When an
  // overlay closes, this effect re-runs and re-binds the handler.
  useEffect(() => {
    if (viewMode !== 'single' || activeDiffFile != null || circuitEditorOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') exitSingleMode();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [viewMode, activeDiffFile, circuitEditorOpen, exitSingleMode]);

  // Reflow the terminal grid on every mode transition: switching modes
  // changes which (and how many) NodeCards mount, and entering/leaving
  // Single grows/shrinks the soloed terminal. fitAll covers all directions.
  // Never dispose — the singleton survives this.
  useEffect(() => {
    terminalManager.fitAll();
  }, [viewMode, singleNode?.id]);

  // Refit when the Probe Panel expands/collapses: it shrinks/grows this view's
  // width via the App flex row, but xterm doesn't re-measure on flex reflow on
  // its own, so terminals would keep a stale column count (#374). Same
  // never-dispose contract as the mode-switch refit above.
  useEffect(() => {
    terminalManager.fitAll();
  }, [probeOpen]);

  // After a drag reorder/swap, nodes move into slots that may be a different
  // size, so refit. Keyed on the id sequence so it fires only when order
  // actually changes (status flips reuse ids → no refit). Never disposes.
  const orderKey = useMemo(() => visibleNodes.map(n => n.id).join(','), [visibleNodes]);
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
    () => (activeDragNodeId == null ? null : nodesById[activeDragNodeId] ?? null),
    [activeDragNodeId, nodesById],
  );
  const dragEnabled = viewMode !== 'pinned' && gridSortBy === 'custom';

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
    if (!dragEnabled) return;
    const data = e.active.data.current as { nodeId: number; meshId: number } | undefined;
    if (!data) return;
    activatorXRef.current = (e.activatorEvent as PointerEvent).clientX ?? 0;
    setActiveDragNodeId(data.nodeId);
    setDropIntent(null);
  };

  const handleDragMove = (e: DragMoveEvent) => {
    if (!dragEnabled) {
      setDropIntent(null);
      return;
    }
    setDropIntent(intentFromEvent(e));
  };

  const handleDragEnd = (e: DragEndEvent) => {
    const data = e.active.data.current as { nodeId: number; meshId: number } | undefined;
    const intent = intentFromEvent(e);
    setActiveDragNodeId(null);
    setDropIntent(null);
    if (!dragEnabled) return;
    if (!data || !intent) return;
    if (intent.kind === 'swap') {
      swapAgentNodes(data.nodeId, intent.targetNodeId);
      return;
    }
    // Issue #1384 — iterate `nodeIds` and dereference through `nodesById`
    // rather than reading the (now-removed) `agentNodes` array. The
    // imperative read inside the drag handler is the only place we touch
    // the ordered list directly; React state subscriptions above go
    // through the same normalized shape.
    const dragState = useAgentNodeStore.getState();
    const meshNodes: AgentNode[] = [];
    for (const id of dragState.nodeIds) {
      const n = dragState.nodesById[id];
      if (n && n.mesh_id === data.meshId) meshNodes.push(n);
    }
    meshNodes.sort((a, b) => a.position - b.position);
    const targetIdx = meshNodes.findIndex(n => n.id === intent.targetNodeId);
    if (targetIdx === -1) return;
    reorderAgentNode(data.nodeId, intent.kind === 'insert-before' ? targetIdx : targetIdx + 1);
  };

  const handleDragCancel = () => {
    setActiveDragNodeId(null);
    setDropIntent(null);
  };

  // The empty-state container owns the empty-state-only store
  // subscriptions (provider list, harness readiness, mesh count, the
  // canvas empty-state callbacks). Mounting it ONLY when the canvas
  // is actually empty keeps the active terminal view free of those
  // subscriptions — every provider-list invalidation or harness
  // change would re-render `AgentNodeView` and cascade into the
  // NodeCard / Terminal tree the user is typing into. Senior-review
  // finding. The container reads its own inputs from the store so
  // the closed-canvas render is identical to the pre-#1536 single
  // subscription count.
  const showEmptyState = viewMode === 'single'
    ? singleNode === null
    : visibleNodes.length === 0;

  return (
    <div className="relative flex-1 flex flex-col h-full bg-bg-base overflow-hidden">
      {/* Center Workspace Diff Overlay (#379) — covers the terminal grid with a
          spacious single-file diff when a changed file is clicked in the Probe.
          Absolutely positioned over this view only, so the right-hand Probe
          (a sibling in App's flex row) stays open and interactive. The
          terminals behind it keep running; "Back to Terminals" just hides it. */}
      {activeDiffFile && <CenterDiffOverlay diff={activeDiffFile} />}
      {/* Circuit canvas editor (#1209) — same overlay discipline: covers the
          workspace, never unmounts the terminals underneath. */}
      {circuitEditorOpen && <CircuitEditorOverlay />}
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
          {showEmptyState ? (
            // The container owns every empty-state-only subscription
            // (provider list, harness readiness, mesh count, the five
            // CTA callbacks). Mounting it here — rather than at the
            // `AgentNodeView` root — keeps the active terminal grid
            // free of those subscriptions; provider-list invalidations
            // and harness changes no longer cascade into NodeCard /
            // Terminal re-renders while the user is typing. Senior-
            // review finding: reactivity pollution.
            <CanvasEmptyStateContainer
              viewMode={viewMode}
              lastNonSingleMode={lastNonSingleMode}
              selectedMeshId={selectedMeshId}
              activeNodeId={activeNodeId}
              agentNodes={agentNodes}
              visibleNodesLength={visibleNodes.length}
            />
          ) : viewMode === 'single' ? (
            singleNode ? (
              <div className="flex-1 flex flex-col p-1 bg-bg-surface overflow-hidden">
                <NodeCard
                  nodeId={singleNode.id}
                  memberIds={activityMembersByRoot[singleNode.id] ?? [singleNode.id]}
                  isActive={singleNode.id === activeNodeId}
                  onActivate={setActiveNode}
                  draggable={false}
                />
              </div>
            ) : null
          ) : visibleNodes.length <= 2 ? (
            <ResizablePanes
              nodes={visibleNodes}
              activityMembersByRoot={activityMembersByRoot}
              draggable={dragEnabled}
            />
          ) : (
            <GridSplitter
              nodes={visibleNodes}
              activityMembersByRoot={activityMembersByRoot}
              draggable={dragEnabled}
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
