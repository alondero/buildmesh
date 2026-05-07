import { useEffect, useMemo, useState } from 'react';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { AgentTerminal } from '../Terminal/Terminal';
import { BuildRunTerminal } from '../Terminal/BuildRunTerminal';
import { SlashCommandBar } from '../SlashCommands/SlashCommandBar';
import { ChangedFilesPanel } from '../ChangedFilesPanel/ChangedFilesPanel';
import { emit } from '@tauri-apps/api/event';
import { watchSession, unwatchSession } from '../../lib/tauri';
import { FIT_TERMINALS } from '../../lib/events';
import { getNodeGitPath } from '../../lib/paths';
import { useGridLayout } from '../../hooks/useGridLayout';
import { DiffViewerModal } from './DiffViewerModal';
import { GridNodeHeader } from './GridNodeHeader';
import type { GitStatus, DiffResult } from '../../lib/tauri';

export function SessionView() {
  const selectedMeshId = useMeshStore(state => state.selectedMeshId);
  const {
    agentNodes,
    getActiveNode,
    setActiveNode,
    sendToAgent,
  } = useAgentNodeStore();

  const activeNode = getActiveNode();

  const changedFilesOpen = useUIStore(state => state.changedFilesOpen);
  const changedFilesNodeId = useUIStore(state => state.changedFilesNodeId);
  const [selectedDiff, setSelectedDiff] = useState<{ file: GitStatus; diff: DiffResult } | null>(null);
  const [openBuildRun, setOpenBuildRun] = useState<{ nodeId: number; mode: 'build' | 'run' } | null>(null);


  const filteredNodes = useMemo(() => {
    if (selectedMeshId === null) {
      return agentNodes;
    }
    return agentNodes.filter(s => s.mesh_id === selectedMeshId);
  }, [agentNodes, selectedMeshId]);

  // Get node path for git status -- from the node whose changes are shown in panel
  const changedFilesNode = changedFilesNodeId
    ? agentNodes.find(n => n.id === changedFilesNodeId)
    : activeNode;
  const nodePath = changedFilesNode ? getNodeGitPath(changedFilesNode) : '';

  const closeChangedFiles = useUIStore(state => state.closeChangedFiles);

  useEffect(() => {
    if (!activeNode) return;
    watchSession(activeNode.id).catch(console.error);
    return () => {
      unwatchSession(activeNode.id).catch(console.error);
    };
  }, [activeNode?.id]);

  useEffect(() => {
    closeChangedFiles();
  }, [selectedMeshId, closeChangedFiles]);

  useEffect(() => {
    if (activeNode && changedFilesOpen && changedFilesNodeId !== activeNode.id) {
      useUIStore.getState().setChangedFilesNodeId(activeNode.id);
    }
  }, [activeNode?.id, changedFilesOpen, changedFilesNodeId]);

  // Auto-select first node when switching to a mesh that doesn't include the active node
  useEffect(() => {
    if (filteredNodes.length > 0 && activeNode && !filteredNodes.find(s => s.id === activeNode.id)) {
      setActiveNode(filteredNodes[0].id);
    }
  }, [selectedMeshId, filteredNodes, activeNode, setActiveNode]);

  // Fit terminal when active node changes (e.g. container might have resized)
  useEffect(() => {
    if (activeNode) {
      emit(FIT_TERMINALS, { sessionId: activeNode.id }).catch(console.error);
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
          onSlashCommand={(nodeId, cmd) => sendToAgent(nodeId, cmd)}
          changedFilesNodeId={changedFilesNodeId}
          onBuildRun={(nodeId, mode) => setOpenBuildRun({ nodeId, mode })}
          buildRunOpen={openBuildRun}
          setBuildRunOpen={setOpenBuildRun}
        />

        <ChangedFilesPanel
          projectPath={nodePath}
          isOpen={changedFilesOpen}
          onFileSelect={(file, diff) => setSelectedDiff({ file, diff })}
        />
      </div>

      {selectedDiff && (
        <DiffViewerModal
          file={selectedDiff.file}
          diff={selectedDiff.diff}
          onClose={() => setSelectedDiff(null)}
        />
      )}
    </div>
  );
}

function GridLayout({ nodes, onSlashCommand, changedFilesNodeId, onBuildRun, buildRunOpen, setBuildRunOpen }: {
  nodes: AgentNode[];
  onSlashCommand: (nodeId: number, cmd: string) => void;
  changedFilesNodeId: number | null;
  onBuildRun: (nodeId: number, mode: 'build' | 'run') => void;
  buildRunOpen: { nodeId: number; mode: 'build' | 'run' } | null;
  setBuildRunOpen: (val: { nodeId: number; mode: 'build' | 'run' } | null) => void;
}) {
  const { columns, rows } = useGridLayout(nodes.length);

  return (
    <div className={`flex-1 grid gap-1.5 p-1.5 bg-bg-surface ${columns} ${rows}`}>
      {nodes.map((node) => {
        const isBuildRunOpen = buildRunOpen?.nodeId === node.id ? buildRunOpen.mode : null;
        return (
          <div key={node.id} className="flex flex-col bg-bg-card border border-border-default rounded-sm overflow-hidden group hover:border-accent-cyan/50 transition-colors">
            <GridNodeHeader node={node} changedFilesNodeId={changedFilesNodeId} onBuildRun={onBuildRun} />
            <div className="flex-1 flex flex-col overflow-hidden bg-black">
              <div className={`${isBuildRunOpen ? 'flex-[2]' : 'flex-1'} overflow-hidden`}>
                <AgentTerminal sessionId={node.id} />
              </div>
              {isBuildRunOpen && (
                <BuildRunTerminal
                  sessionId={node.id}
                  mode={isBuildRunOpen}
                  onClose={() => setBuildRunOpen(null)}
                />
              )}
            </div>
            <div className="opacity-40 group-hover:opacity-100 transition-opacity">
              <SlashCommandBar onCommand={(cmd) => onSlashCommand(node.id, cmd)} />
            </div>
          </div>
        );
      })}
    </div>
  );
}
