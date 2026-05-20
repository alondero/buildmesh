import { useEffect, useMemo, useState, useRef, type MouseEvent as ReactMouseEvent } from 'react';
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

function clampWidths(widths: number[], min = MIN_PANE_PERCENT): number[] {
  let excess = widths.reduce((s, w) => s + w, 0) - 100;
  if (excess <= 0) {
    const ok = widths.every(w => w >= min);
    if (ok) return widths;
  }
  const capped = widths.map(w => Math.max(min, w));
  const total = capped.reduce((s, w) => s + w, 0);
  return capped.map(w => (w / total) * 100);
}

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

  useEffect(() => {
    setWidths(equalSizes(nodes.length));
  }, [nodes.length]);

  const handleResizeMouseDown = (e: ReactMouseEvent, index: number) => {
    e.preventDefault();
    resizingRef.current = true;
    dragIndexRef.current = index;
    startXRef.current = e.clientX;
    startWidthsRef.current = [...widths];
  };

  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      if (!resizingRef.current) return;
      const container = document.getElementById('grid-panes-container');
      if (!container) return;
      const containerWidth = container.getBoundingClientRect().width;
      const deltaPercent = ((e.clientX - startXRef.current) / containerWidth) * 100;

      const newWidths = [...startWidthsRef.current];
      const i = dragIndexRef.current;
      const leftDelta = Math.min(deltaPercent, newWidths[i] - MIN_PANE_PERCENT);
      const rightDelta = Math.min(-deltaPercent, newWidths[i + 1] - MIN_PANE_PERCENT);
      const actualDelta = Math.max(leftDelta, -rightDelta);

      newWidths[i] += actualDelta;
      newWidths[i + 1] -= actualDelta;
      setWidths(clampWidths(newWidths));
    };

    const handleMouseUp = () => {
      if (resizingRef.current) {
        resizingRef.current = false;
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

  if (nodes.length === 1) {
    const node = nodes[0];
    const isBuildRunOpen = buildRunOpen?.nodeId === node.id ? buildRunOpen.mode : null;
    const isActive = node.id === activeNodeId;
    const borderClass = node.status === 'awaiting_input'
      ? 'border-status-warning animate-border-pulse'
      : isActive ? 'border-accent-cyan/60' : 'border-border-default hover:border-accent-cyan/50';
    return (
      <div className="flex-1 flex flex-col overflow-hidden">
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
    );
  }

  return (
    <div id="grid-panes-container" className="flex-1 flex gap-0.5 p-1 bg-bg-surface overflow-hidden">
      {nodes.map((node, idx) => {
        const isBuildRunOpen = buildRunOpen?.nodeId === node.id ? buildRunOpen.mode : null;
        const isActive = node.id === activeNodeId;
        const borderClass = node.status === 'awaiting_input'
          ? 'border-status-warning animate-border-pulse'
          : isActive ? 'border-accent-cyan/60' : 'border-border-default hover:border-accent-cyan/50';
        return (
          <div key={node.id} className="flex flex-col overflow-hidden" style={{ width: `${widths[idx]}%`, flex: '0 0 auto' }}>
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
            {idx < nodes.length - 1 && (
              <div
                onMouseDown={(e) => handleResizeMouseDown(e, idx)}
                className="w-1 cursor-col-resize hover:bg-accent-cyan/30 active:bg-accent-cyan/50 transition-colors shrink-0 my-1.5 rounded-sm"
                style={{ minWidth: RESIZE_HANDLE_WIDTH }}
              />
            )}
          </div>
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
              <div className="text-center">
                <p className="text-lg mb-2 text-text-secondary">Buildmesh Orchestrator</p>
                <p className="text-sm">Select a node to start managing your agents</p>
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