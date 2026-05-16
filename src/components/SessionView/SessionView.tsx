import { useEffect, useMemo, useState } from 'react';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { AgentTerminal } from '../Terminal/Terminal';
import { BuildRunTerminal } from '../Terminal/BuildRunTerminal';
import { FileExplorerPanel } from '../FileTree/FileExplorerPanel';
import { watchSession, unwatchSession } from '../../lib/tauri';
import { terminalManager } from '../Terminal/Terminal';
import { useGridLayout } from '../../hooks/useGridLayout';
import { GridNodeHeader } from './GridNodeHeader';

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
    if (selectedMeshId !== null) {
      closeFileExplorer();
    }
  }, [selectedMeshId, closeFileExplorer]);

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

  if (filteredNodes.length === 0) {
    return (
      <div className="flex-1 flex flex-col bg-bg-base">
        <div className="flex-1 flex items-center justify-center text-text-muted">
          <div className="text-center">
            <p className="text-lg mb-2 text-text-secondary">Buildmesh Orchestrator</p>
            <p className="text-sm">Select a node to start managing your agents</p>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="flex-1 flex flex-col h-full bg-bg-base overflow-hidden">
      <div className="flex-1 flex overflow-hidden">
        <GridLayout
          nodes={filteredNodes}
          onBuildRun={(nodeId, mode) => setOpenBuildRun({ nodeId, mode })}
          buildRunOpen={openBuildRun}
          setBuildRunOpen={setOpenBuildRun}
        />

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
      </div>
    </div>
  );
}

function GridLayout({ nodes, onBuildRun, buildRunOpen, setBuildRunOpen }: {
  nodes: AgentNode[];
  onBuildRun: (nodeId: number, mode: 'build' | 'run') => void;
  buildRunOpen: { nodeId: number; mode: 'build' | 'run' } | null;
  setBuildRunOpen: (val: { nodeId: number; mode: 'build' | 'run' } | null) => void;
}) {
  const { columns, rows } = useGridLayout(nodes.length);
  const activeNodeId = useAgentNodeStore(state => state.activeNodeId);
  const setActiveNode = useAgentNodeStore(state => state.setActiveNode);

  return (
    <div className={`flex-1 grid gap-1.5 p-1.5 bg-bg-surface ${columns} ${rows}`}>
      {nodes.map((node) => {
        const isBuildRunOpen = buildRunOpen?.nodeId === node.id ? buildRunOpen.mode : null;
        const isActive = node.id === activeNodeId;
        const borderClass = node.status === 'awaiting_input'
          ? 'border-status-warning animate-border-pulse'
          : isActive ? 'border-accent-cyan/60' : 'border-border-default hover:border-accent-cyan/50';
        return (
          <div key={node.id} onClick={() => { if (!isActive) setActiveNode(node.id); }} className={`flex flex-col bg-bg-card border rounded-sm overflow-hidden group transition-colors ${borderClass}`}>
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
        );
      })}
    </div>
  );
}
