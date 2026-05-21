import { Fragment, useEffect, useMemo, useState, useRef, type MouseEvent as ReactMouseEvent } from 'react';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { AgentTerminal, terminalManager } from '../Terminal/Terminal';
import { BuildRunTerminal } from '../Terminal/BuildRunTerminal';
import { FileExplorerPanel } from '../FileTree/FileExplorerPanel';
import { watchSession, unwatchSession } from '../../lib/tauri';
import { GridNodeHeader } from './GridNodeHeader';
import { GridSplitter } from './GridSplitter';
import { equalSizes } from '../../hooks/useGridLayout';

const MIN_PANE_PERCENT = 15;
const RESIZE_HANDLE_WIDTH = 4;

interface ResizablePanesProps {
  nodes: AgentNode[];
  onBuildRun: (nodeId: number, mode: 'build' | 'run') => void;
  buildRunOpen: { nodeId: number; mode: 'build' | 'run' } | null;
  setBuildRunOpen: (val: { nodeId: number; mode: 'build' | 'run' } | null) => void;
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
        const isBuildRunOpen = buildRunOpen?.nodeId === node.id ? buildRunOpen.mode : null;
        const isActive = node.id === activeNodeId;
        const borderClass = node.status === 'awaiting_input'
          ? 'border-status-warning animate-border-pulse'
          : isActive ? 'border-accent-cyan/60' : 'border-border-default hover:border-accent-cyan/50';
        return (
          <Fragment key={node.id}>
            <div
              className="flex flex-col overflow-hidden"
              style={isMultiPane ? { width: `${widths[idx]}%`, flex: '0 0 auto' } : { flex: '1 1 0%' }}
            >
              <div
                onClick={() => { if (!isActive) setActiveNode(node.id); }}
                className={`flex-1 flex flex-col bg-bg-card border rounded-sm overflow-hidden group transition-colors ${borderClass}`}
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
  const meshesById = useMeshStore(state => state.meshesById);
  const {
    agentNodes,
    getActiveNode,
    setActiveNode,
  } = useAgentNodeStore();

  const activeNode = getActiveNode();

  const fileExplorerContext = useUIStore(state => state.fileExplorerContext);
  const closeFileExplorer = useUIStore(state => state.closeFileExplorer);
  const [fileExplorerWidth, setFileExplorerWidth] = useState(360);
  const [openBuildRun, setOpenBuildRun] = useState<{ nodeId: number; mode: 'build' | 'run' } | null>(null);

  const filteredNodes = useMemo(() => {
    if (selectedMeshId === null) {
      return agentNodes;
    }
    return agentNodes.filter(s => s.mesh_id === selectedMeshId);
  }, [agentNodes, selectedMeshId]);

  // Get node for file explorer context
  const fileExplorerNode = useMemo(() => {
    if (!fileExplorerContext || fileExplorerContext.type !== 'agent') return null;
    return agentNodes.find(n => n.id === fileExplorerContext.nodeId) ?? null;
  }, [fileExplorerContext, agentNodes]);

  // Get mesh name for file explorer mesh context
  const fileExplorerMeshName = useMemo(() => {
    if (!fileExplorerContext || fileExplorerContext.type !== 'mesh') return null;
    return meshesById.get(fileExplorerContext.meshId)?.name ?? null;
  }, [fileExplorerContext, meshesById]);

  useEffect(() => {
    if (!activeNode) return;
    watchSession(activeNode.id).catch(console.error);
    return () => {
      unwatchSession(activeNode.id).catch(console.error);
    };
  }, [activeNode?.id]);

  useEffect(() => {
    if (selectedMeshId === null) return;
    const ctx = fileExplorerContext;
    const shouldClose = ctx && (
      (ctx.type === 'mesh' && ctx.meshId !== selectedMeshId) ||
      ctx.type === 'agent'
    );
    if (shouldClose) closeFileExplorer();
  }, [selectedMeshId, fileExplorerContext, closeFileExplorer]);

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

  return (
    <div className="flex-1 flex flex-col h-full bg-bg-base overflow-hidden">
      <div className="flex-1 flex overflow-hidden">
        {fileExplorerContext && (
          <FileExplorerPanel
            context={fileExplorerContext}
            width={fileExplorerWidth}
            onWidthChange={setFileExplorerWidth}
            onClose={closeFileExplorer}
            nodeName={fileExplorerNode?.name}
            meshName={fileExplorerMeshName ?? undefined}
          />
        )}

        <div className="flex-1 flex overflow-hidden">
          {filteredNodes.length === 0 ? (
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
      </div>
    </div>
  );
}