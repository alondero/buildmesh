import { useEffect, useMemo, useState } from 'react';
import { useAgentNodeStore, AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { AgentTerminal } from '../Terminal/Terminal';
import { BuildRunTerminal } from '../Terminal/BuildRunTerminal';
import { SlashCommandBar } from '../SlashCommands/SlashCommandBar';
import { ChangedFilesPanel } from '../ChangedFilesPanel/ChangedFilesPanel';
import { BuildRunDropdown } from '../BuildRun/BuildRunDropdown';
import { listen, emit } from '@tauri-apps/api/event';
import { watchSession, unwatchSession, getGitSummary, type GitSummary } from '../../lib/tauri';
import { FILE_CHANGE, FIT_TERMINALS } from '../../lib/events';
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

  // Get node path for git status — from the node whose changes are shown in panel
  const changedFilesNode = changedFilesNodeId
    ? agentNodes.find(n => n.id === changedFilesNodeId)
    : activeNode;
  const nodePath = changedFilesNode?.path || '';

  useEffect(() => {
    if (!activeNode) return;
    watchSession(activeNode.id).catch(console.error);
    const unlisten = listen<{ session_id: number; change: { path: string; kind: string } }>(FILE_CHANGE, () => {
      // File tree refresh handled by parent if needed
    });
    return () => {
      unwatchSession(activeNode.id).catch(console.error);
      unlisten.then(fn => fn());
    };
  }, [activeNode?.id]);

  // Auto-select first node when switching to a mesh that doesn't include the active node
  useEffect(() => {
    if (filteredNodes.length > 0 && activeNode && !filteredNodes.find(s => s.id === activeNode.id)) {
      setActiveNode(filteredNodes[0].id);
    }
  }, [selectedMeshId, filteredNodes, activeNode, setActiveNode]);

  // Dispatch fit-terminals event when active node changes
  useEffect(() => {
    if (activeNode) {
      console.log(`[DEBUG SessionView] Emitting FIT_TERMINALS for node ${activeNode.id}`);
      emit(FIT_TERMINALS, { sessionId: activeNode.id }).catch(console.error);
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeNode?.id]);

  const handleSlashCommand = (nodeId: number, cmd: string) => {
    sendToAgent(nodeId, cmd);
  };

  const handleFileSelect = (file: GitStatus, diff: DiffResult) => {
    setSelectedDiff({ file, diff });
  };

  const closeDiff = () => {
    setSelectedDiff(null);
  };

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
      {/* Main content area with grid and optional panel */}
      <div className="flex-1 flex overflow-hidden">
        {/* Grid of filtered nodes */}
        <GridLayout nodes={filteredNodes} onSlashCommand={handleSlashCommand} changedFilesNodeId={changedFilesNodeId} onBuildRun={(nodeId, mode) => setOpenBuildRun({ nodeId, mode })} buildRunOpen={openBuildRun} setBuildRunOpen={setOpenBuildRun} />

        {/* Changed Files Panel */}
        <ChangedFilesPanel
          projectPath={nodePath}
          isOpen={changedFilesOpen}
          onFileSelect={handleFileSelect}
        />
      </div>

      {/* Diff Viewer Modal */}
      {selectedDiff && (
        <DiffViewerModal
          file={selectedDiff.file}
          diff={selectedDiff.diff}
          onClose={closeDiff}
        />
      )}
    </div>
  );
}

// Module-level cache for git summaries (keyed by path)
const gridSummaryCache = new Map<string, GitSummary>();
const gridPendingFetches = new Map<string, Promise<GitSummary | null>>();

function GridNodeHeader({ node, changedFilesNodeId, onBuildRun }: { node: AgentNode; changedFilesNodeId: number | null; onBuildRun: (nodeId: number, mode: 'build' | 'run') => void }) {
  const toggleChangedFiles = useUIStore(state => state.toggleChangedFiles);
  const deleteAgentNode = useAgentNodeStore(state => state.deleteAgentNode);
  const [summary, setSummary] = useState<GitSummary | null>(null);

  const handleClose = async (e: React.MouseEvent) => {
    e.stopPropagation();
    await deleteAgentNode(node.id);
  };

  useEffect(() => {
    if (!node.path) return;

    const cached = gridSummaryCache.get(node.path);
    if (cached) {
      setSummary(cached.total > 0 ? cached : null);
      return;
    }

    if (gridPendingFetches.has(node.path)) return;

    const fetchSummary = async (): Promise<GitSummary | null> => {
      try {
        const result = await getGitSummary(node.path);
        gridSummaryCache.set(node.path, result);
        gridPendingFetches.delete(node.path);
        setSummary(result.total > 0 ? result : null);
        return result;
      } catch {
        gridPendingFetches.delete(node.path);
        return null;
      }
    };

    const p = fetchSummary();
    gridPendingFetches.set(node.path, p);
  }, [node.path]);

  const isPanelNode = changedFilesNodeId === node.id;

  return (
    <div className="flex items-center justify-between px-2.5 py-1.5 bg-bg-overlay border-b border-border-default">
      <div className="flex items-center gap-2 overflow-hidden">
        <span className={`w-1.5 h-1.5 rounded-full ${
          node.status === 'running' ? 'bg-accent-cyan' :
          node.status === 'awaiting_input' ? 'bg-status-warning animate-pulse' :
          'bg-text-muted'
        }`} />
        <span className="text-[11px] font-bold text-text-secondary truncate">{node.name}</span>
        {node.status === 'awaiting_input' && (
          <span className="text-[9px] text-status-warning font-bold ml-1">ATTN</span>
        )}
        {summary && (
          <span
            onClick={() => toggleChangedFiles(node.id)}
            className={`text-[10px] font-mono cursor-pointer hover:text-accent-cyan ${isPanelNode ? 'text-accent-cyan' : 'text-text-muted'}`}
            title="Click to see changes"
          >
            +{summary.added} ~{summary.modified} -{summary.deleted}
          </span>
        )}
      </div>
      <div className="flex items-center gap-1.5">
        <span className="text-[9px] text-text-muted font-mono px-1 rounded bg-bg-base">{node.env.toUpperCase()}</span>
        <BuildRunDropdown node={node} onBuildRun={onBuildRun} />
        <button
          onClick={handleClose}
          className="w-4 h-4 flex items-center justify-center rounded text-text-muted hover:text-accent-cyan hover:bg-bg-base transition-colors text-[10px]"
          title="Close agent node"
        >
          ×
        </button>
      </div>
    </div>
  );
}

function GridLayout({ nodes, onSlashCommand, changedFilesNodeId, onBuildRun, buildRunOpen, setBuildRunOpen }: { nodes: AgentNode[]; onSlashCommand: (nodeId: number, cmd: string) => void; changedFilesNodeId: number | null; onBuildRun: (nodeId: number, mode: 'build' | 'run') => void; buildRunOpen: { nodeId: number; mode: 'build' | 'run' } | null; setBuildRunOpen: (val: { nodeId: number; mode: 'build' | 'run' } | null) => void }) {
  const count = nodes.length;
  let gridCols = "grid-cols-1";
  let gridRows = "grid-rows-1";

  if (count === 2) {
    gridCols = "grid-cols-2";
  } else if (count === 3) {
    gridCols = "grid-cols-2";
    gridRows = "grid-rows-2";
  } else if (count === 4) {
    gridCols = "grid-cols-2";
    gridRows = "grid-rows-2";
  } else if (count > 4) {
    gridCols = "grid-cols-3";
    gridRows = "grid-rows-2";
  }

  return (
    <div className={`flex-1 grid gap-1.5 p-1.5 bg-bg-surface ${gridCols} ${gridRows}`}>
      {nodes.map((node) => {
        const isBuildRunOpen = buildRunOpen?.nodeId === node.id ? buildRunOpen.mode : null;
        return (
          <div key={node.id} className="flex flex-col bg-bg-card border border-border-default rounded-sm overflow-hidden group hover:border-accent-cyan/50 transition-colors">
            <GridNodeHeader node={node} changedFilesNodeId={changedFilesNodeId} onBuildRun={onBuildRun} />
            <div className="flex-1 overflow-hidden bg-black relative">
              <AgentTerminal sessionId={node.id} />
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

// Simple diff viewer modal for changed files
function DiffViewerModal({ file, diff, onClose }: { file: GitStatus; diff: DiffResult; onClose: () => void }) {
  return (
    <div className="fixed inset-0 bg-black/80 flex items-center justify-center z-50">
      <div className="bg-[#1a1a1a] border border-[#2a2a2a] rounded-lg w-[95vw] h-[85vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-[#2a2a2a]">
          <div className="flex items-center gap-2">
            <span className={`w-2 h-2 rounded-full ${
              file.status === 'added' ? 'bg-green-400' :
              file.status === 'modified' ? 'bg-amber-400' :
              file.status === 'deleted' ? 'bg-red-400' :
              'bg-gray-400'
            }`} />
            <h2 className="text-sm font-semibold">{file.path}</h2>
          </div>
          <button
            onClick={onClose}
            className="text-[#888] hover:text-white text-lg"
          >
            ×
          </button>
        </div>

        {/* Diff content */}
        <div className="flex-1 overflow-auto">
          {diff.files.length === 0 ? (
            <div className="flex items-center justify-center h-full text-[#666]">
              No changes
            </div>
          ) : (
            <div className="space-y-4 p-4">
              {diff.files.map((file) => (
                <div key={file.path} className="border border-[#2a2a2a] rounded overflow-hidden">
                  <div className="px-3 py-2 bg-[#111] text-xs text-[#888] border-b border-[#2a2a2a] font-mono">
                    {file.path}
                  </div>

                  {/* Unified diff */}
                  <div className="font-mono text-xs">
                    {file.hunks.map((hunk, hi) => (
                      <div key={hi} className="py-1">
                        {hunk.lines.map((line, li) => (
                          <div
                            key={li}
                            className={`
                              px-3 py-0.5 flex
                              ${line.line_type === 'add' ? 'bg-[#22c55e20] text-[#22c55e]' : ''}
                              ${line.line_type === 'remove' ? 'bg-[#ef444420] text-[#ef4444]' : ''}
                              ${line.line_type === 'context' ? 'text-[#e0e0e0]' : ''}
                            `}
                          >
                            <span className="w-8 text-[#666] select-none text-right mr-2">
                              {line.old_num || ''}
                            </span>
                            <span className="w-8 text-[#666] select-none text-right mr-2">
                              {line.new_num || ''}
                            </span>
                            <span className="w-4 text-[#666] select-none">
                              {line.line_type === 'add' ? '+' : line.line_type === 'remove' ? '-' : ' '}
                            </span>
                            <span>{line.content}</span>
                          </div>
                        ))}
                      </div>
                    ))}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}