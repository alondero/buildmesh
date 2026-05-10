import { useMemo } from 'react';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { BuildRunDropdown } from '../BuildRun/BuildRunDropdown';
import { useGitSummary } from '../../hooks/useGitSummary';
import { getNodeGitPath } from '../../lib/paths';
import { getStatusConfig } from '../../lib/status';

interface GridNodeHeaderProps {
  node: AgentNode;
  changedFilesNodeId: number | null;
  onBuildRun: (nodeId: number, mode: 'build' | 'run') => void;
}

export function GridNodeHeader({ node, changedFilesNodeId, onBuildRun }: GridNodeHeaderProps) {
  const toggleChangedFiles = useUIStore(state => state.toggleChangedFiles);
  const deleteAgentNode = useAgentNodeStore(state => state.deleteAgentNode);
  const meshesById = useMeshStore(state => state.meshesById);

  const handleClose = async (e: React.MouseEvent) => {
    e.stopPropagation();
    await deleteAgentNode(node.id);
  };

  const gitPath = getNodeGitPath(node);
  const { summary } = useGitSummary(gitPath || null);

  const isPanelNode = changedFilesNodeId === node.id;

  const meshLabel = useMemo(() => {
    const m = meshesById.get(node.mesh_id);
    return m ? `[${m.name} #${node.id}]` : `[#${node.id}]`;
  }, [meshesById, node.mesh_id, node.id]);

  return (
    <div className="flex items-center justify-between px-2.5 py-1.5 bg-bg-overlay border-b border-border-default">
      <div className="flex items-center gap-2 overflow-hidden">
        <span className={`w-1.5 h-1.5 rounded-full ${getStatusConfig(node.status).bgColor}`} />
        <span className="text-[11px] font-bold text-text-secondary truncate">
          {node.name} {meshLabel}
        </span>
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
