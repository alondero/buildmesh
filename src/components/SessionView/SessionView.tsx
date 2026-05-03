import { useEffect, useMemo, useState } from 'react';
import { useAgentNodeStore, AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { AgentTerminal } from '../Terminal/Terminal';
import { SlashCommandBar } from '../SlashCommands/SlashCommandBar';
import { ChangedFilesPanel } from '../ChangedFilesPanel/ChangedFilesPanel';
import { listen, emit } from '@tauri-apps/api/event';
import { watchSession, unwatchSession } from '../../lib/tauri';
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

  const [changedFilesOpen, setChangedFilesOpen] = useState(false);
  const [selectedDiff, setSelectedDiff] = useState<{ file: GitStatus; diff: DiffResult } | null>(null);

  const filteredNodes = useMemo(() => {
    if (selectedMeshId === null) {
      return agentNodes;
    }
    return agentNodes.filter(s => s.mesh_id === selectedMeshId);
  }, [agentNodes, selectedMeshId]);

  // Get node path for git status
  const nodePath = activeNode?.path || '';

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
      {/* Changed Files Toggle */}
      <div className="flex items-center justify-end px-3 py-2 border-b border-border-subtle">
        <button
          onClick={() => setChangedFilesOpen(!changedFilesOpen)}
          className={`
            flex items-center gap-1.5 px-2.5 h-8 rounded text-xs transition-colors
            ${changedFilesOpen
              ? 'bg-accent-cyan/20 text-accent-cyan border border-accent-cyan/50'
              : 'text-text-muted hover:bg-bg-card hover:text-text-secondary border border-transparent'
            }
          `}
          title="Toggle Changed Files"
        >
          <svg className="w-3.5 h-3.5" viewBox="0 0 16 16" fill="currentColor">
            <path d="M2 2.5A1.5 1.5 0 013.5 1h9A1.5 1.5 0 0114 2.5v11a1.5 1.5 0 01-1.5 1.5h-9A1.5 1.5 0 012 13.5v-11zM3.5 2a.5.5 0 00-.5.5v11a.5.5 0 00.5.5h9a.5.5 0 00.5-.5v-11a.5.5 0 00-.5-.5h-9z"/>
            <path d="M2 5a.5.5 0 01.5-.5h2a.5.5 0 010 1h-2a.5.5 0 01-.5-.5zM2 8a.5.5 0 01.5-.5h5a.5.5 0 010 1h-5a.5.5 0 01-.5-.5zM2 11a.5.5 0 01.5-.5h8a.5.5 0 010 1h-8a.5.5 0 01-.5-.5z"/>
          </svg>
          <span>Changed</span>
        </button>
      </div>

      {/* Main content area with grid and optional panel */}
      <div className="flex-1 flex overflow-hidden">
        {/* Grid of filtered nodes */}
        <GridLayout nodes={filteredNodes} onSlashCommand={handleSlashCommand} />

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

function GridLayout({ nodes, onSlashCommand }: { nodes: AgentNode[]; onSlashCommand: (nodeId: number, cmd: string) => void }) {
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
        return (
          <div key={node.id} className="flex flex-col bg-bg-card border border-border-default rounded-sm overflow-hidden group hover:border-accent-cyan/50 transition-colors">
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
              </div>
              <span className="text-[9px] text-text-muted font-mono px-1 rounded bg-bg-base">{node.env.toUpperCase()}</span>
            </div>
            <div className="flex-1 overflow-hidden bg-black">
              <AgentTerminal sessionId={node.id} />
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