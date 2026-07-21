import { Fragment, useEffect, useMemo, useState, useRef } from 'react';
import {
  DndContext, DragOverlay, PointerSensor, useSensor, useSensors, pointerWithin,
  type DragStartEvent, type DragMoveEvent, type DragEndEvent,
} from '@dnd-kit/core';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { terminalManager } from '../Terminal/Terminal';
import { SHORTCUT_CATALOG, shortcutLabel } from '../../lib/shortcutCatalog';
import { watchAgentNode, unwatchAgentNode } from '../../lib/tauri';
import { GridSplitter } from './GridSplitter';
import { scopeNodesForMode, resolveSingleNode } from '../../lib/viewModes';
import { CenterDiffOverlay } from './CenterDiffOverlay';
import { NodeCard, type BuildRunState } from './NodeCard';
import { DropIntentContext, NodeDragPreview, computeDropIntent, type DropIntent } from './nodeDrag';
import { equalSizes } from '../../hooks/useGridLayout';
import { useResizable, SPLITTER_HANDLE_WIDTH } from '../../hooks/useResizable';

const MIN_PANE_PERCENT = 15;

interface ResizablePanesProps {
  nodes: AgentNode[];
  onBuildRun: (nodeId: number, mode: 'build' | 'run' | 'terminal') => void;
  buildRunOpen: { nodeId: number; mode: 'build' | 'run' | 'terminal' } | null;
  setBuildRunOpen: (val: { nodeId: number; mode: 'build' | 'run' | 'terminal' } | null) => void;
  // Pinned Grid mode disables card drag-reorder (wayfinder #982 / #986).
  draggable?: boolean;
}

function ResizablePanes({ nodes, onBuildRun, buildRunOpen, setBuildRunOpen, draggable = true }: ResizablePanesProps) {
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
                node={node}
                isActive={node.id === activeNodeId}
                onActivate={setActiveNode}
                onBuildRun={onBuildRun}
                buildRunOpen={buildRunOpen}
                setBuildRunOpen={setBuildRunOpen}
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

/// Empty state for Mesh Grid / All Nodes / Single when the scope has no
/// nodes — the original "Add Mesh" splash. The shortcut rows source from
/// SHORTCUT_CATALOG (issue #748) so catalog edits propagate here.
function NoNodesSplash() {
  return (
    <div className="flex-1 flex items-center justify-center text-text-muted">
      <div className="text-center max-w-sm">
        <p className="text-xl mb-2 text-text-primary font-sans font-semibold">Buildmesh</p>
        <p className="text-sm text-text-secondary mb-6 font-sans">Orchestrate AI agents across your meshes. Add a mesh pointing at a Git repository, then spawn agents to work in parallel.</p>
        <button
          onClick={() => useMeshStore.getState().addMesh()}
          className="inline-flex items-center gap-2 px-4 py-2 rounded-md bg-accent-cyan/10 text-accent-cyan font-sans font-medium text-sm hover:bg-accent-cyan/20 transition-colors border border-accent-cyan/20"
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/>
          </svg>
          Add Mesh
        </button>
        <div className="mt-8 text-xs text-text-muted font-mono space-y-1">
          {/* Issue #748: the splash now sources its rows from
              SHORTCUT_CATALOG (entries flagged `splash: true`) instead
              of hand-coding five inline strings. A future catalog edit
              (e.g. renaming `?` to `Ctrl+/`) now propagates here
              automatically — the previous hand-coded version would
              silently drift.

              Modifier prefix follows the platform convention used
              elsewhere (Terminal.tsx context menu, README): ⌘ on macOS,
              Ctrl on Windows/Linux. The arrow glyphs (←/→/↑/↓) read
              identically across platforms and match the key names
              bound by Tauri's global-shortcut plugin.

              Issue #668 — Alt+G (Win/Linux) / ⌘+G (macOS) is the new
              maximize/restore toggle. Listed here so users discover it
              before they ever open a mesh. */}
          {SHORTCUT_CATALOG.filter(e => e.splash).map(entry => (
            <p key={entry.action}>
              <kbd className="px-1 py-0.5 rounded-md bg-bg-card border border-border-default">
                {shortcutLabel(entry)}
              </kbd>
              {' '}{entry.description}
            </p>
          ))}
        </div>
      </div>
    </div>
  );
}

/// Empty state for Pinned Grid mode with 0 pinned nodes (wayfinder #982 /
/// ticket #986). Mirrors the splash's structure (centered, max-w-sm,
/// heading + body + accent-cyan CTA) but the call to action is "View All
/// Nodes" — the natural next step when nothing is pinned yet. Pin afford-
/// ances live in the node header and the sidebar node context menu (#985).
export function PinnedEmptyState() {
  const setViewMode = useUIStore(state => state.setViewMode);
  return (
    <div className="flex-1 flex items-center justify-center text-text-muted">
      <div className="text-center max-w-sm">
        <svg
          className="mx-auto mb-4 w-8 h-8 text-text-muted"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.75"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden
        >
          <path d="M12 17v5" />
          <path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z" />
        </svg>
        <p className="text-xl mb-2 text-text-primary font-sans font-semibold">No pinned nodes</p>
        <p className="text-sm text-text-secondary mb-6 font-sans">
          Pin agents from any mesh to keep them in reach here. Use the pin button in a node's header, or right-click a node in the sidebar.
        </p>
        <button
          onClick={() => {
            // All Nodes ⟺ no mesh selected (one filter, two controls) —
            // clearing the selection flips the mode via the uiStore
            // mesh-subscription. With nothing selected, set it directly.
            const { selectedMeshId, selectMesh } = useMeshStore.getState();
            if (selectedMeshId !== null) {
              selectMesh(null);
            } else {
              setViewMode('all');
            }
          }}
          className="inline-flex items-center gap-2 px-4 py-2 rounded-md bg-accent-cyan/10 text-accent-cyan font-sans font-medium text-sm hover:bg-accent-cyan/20 transition-colors border border-accent-cyan/20"
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <rect width="7" height="9" x="3" y="3" rx="1" />
            <rect width="7" height="5" x="14" y="3" rx="1" />
            <rect width="7" height="9" x="14" y="12" rx="1" />
            <rect width="7" height="5" x="3" y="16" rx="1" />
          </svg>
          View All Nodes
        </button>
      </div>
    </div>
  );
}

export function AgentNodeView() {
  const selectedMeshId = useMeshStore(state => state.selectedMeshId);
  // Granular selectors: subscribing to the whole store (useAgentNodeStore())
  // re-rendered the view on every unrelated change — including each
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

  // View Modes (wayfinder #982): 'single' solos the active node (it subsumes
  // the old maximizedNodeId); the grid modes derive their visible set from
  // the pure helpers in src/lib/viewModes.ts so keyboard traversal (#987)
  // reads the same definition.
  const viewMode = useUIStore(state => state.viewMode);
  const lastNonSingleMode = useUIStore(state => state.lastNonSingleMode);
  const exitSingleMode = useUIStore(state => state.exitSingleMode);
  const probeOpen = useUIStore(state => state.probeOpen);
  const activeDiffFile = useUIStore(state => state.activeDiffFile);
  const [openBuildRun, setOpenBuildRun] = useState<BuildRunState>(null);

  // The ordered nodes the active grid mode renders. 'single' is not a grid
  // mode — it renders `singleNode` below instead, so its list stays empty.
  const visibleNodes = useMemo(
    () => (viewMode === 'single'
      ? []
      : scopeNodesForMode(viewMode, agentNodes, selectedMeshId, activeNodeId)),
    [viewMode, agentNodes, selectedMeshId, activeNodeId],
  );

  // The node Single mode solos: the active node regardless of mesh scope
  // (explicit focus wins — cross-mesh by nature, like Pinned), else the
  // first node of the scope single was entered from, else any node. Unlike
  // the old maximize derivation there is NO visibility check against a mesh
  // filter and no auto-clear effect — ticket #983 deleted both.
  const singleNode = useMemo(
    () => (viewMode !== 'single'
      ? null
      : resolveSingleNode(agentNodes, activeNodeId, lastNonSingleMode, selectedMeshId)),
    [viewMode, agentNodes, activeNodeId, lastNonSingleMode, selectedMeshId],
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
    if (visibleNodes.length > 0 && activeNode && !visibleNodes.find(s => s.id === activeNode.id)) {
      setActiveNode(visibleNodes[0].id);
    }
  }, [viewMode, visibleNodes, activeNode, setActiveNode]);

  // Fit terminal when active node changes (e.g. container might have resized)
  useEffect(() => {
    if (activeNode) {
      terminalManager.fit(activeNode.id);
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeNode?.id]);

  // Escape exits Single mode. Only bound while single is active so we don't
  // intercept Escape (e.g. agent CLIs read it) during normal grid use. While
  // the Center Diff Overlay (#379) is open it sits on top of the solo
  // terminal and owns Escape — without this guard, Escape would close the
  // overlay AND exit single in one press. When the overlay closes, this
  // effect re-runs (activeDiffFile dep) and re-binds the handler.
  useEffect(() => {
    if (viewMode !== 'single' || activeDiffFile != null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') exitSingleMode();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [viewMode, activeDiffFile, exitSingleMode]);

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
          {viewMode === 'single' ? (
            // Single solos one node; with no nodes at all (singleNode null)
            // the shared splash is the only sensible empty state.
            singleNode ? (
              <div className="flex-1 flex flex-col p-1 bg-bg-surface overflow-hidden">
                <NodeCard
                  node={singleNode}
                  isActive={singleNode.id === activeNodeId}
                  onActivate={setActiveNode}
                  onBuildRun={(nodeId, mode) => setOpenBuildRun({ nodeId, mode })}
                  buildRunOpen={openBuildRun}
                  setBuildRunOpen={setOpenBuildRun}
                  draggable={false}
                />
              </div>
            ) : (
              <NoNodesSplash />
            )
          ) : visibleNodes.length === 0 ? (
            // Empty states are mode-aware (ticket #986): Pinned explains
            // pinning and offers All Nodes; mesh/all keep the splash.
            viewMode === 'pinned' ? <PinnedEmptyState /> : <NoNodesSplash />
          ) : visibleNodes.length <= 2 ? (
            <ResizablePanes
              nodes={visibleNodes}
              onBuildRun={(nodeId, mode) => setOpenBuildRun({ nodeId, mode })}
              buildRunOpen={openBuildRun}
              setBuildRunOpen={setOpenBuildRun}
              draggable={viewMode !== 'pinned'}
            />
          ) : (
            <GridSplitter
              nodes={visibleNodes}
              onBuildRun={(nodeId, mode) => setOpenBuildRun({ nodeId, mode })}
              buildRunOpen={openBuildRun}
              setBuildRunOpen={setOpenBuildRun}
              draggable={viewMode !== 'pinned'}
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