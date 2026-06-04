import { useMemo } from 'react';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { BuildRunDropdown } from '../BuildRun/BuildRunDropdown';
import { useGitSummary } from '../../hooks/useGitSummary';
import { getNodeGitPath } from '../../lib/paths';
import { getStatusConfig } from '../../lib/status';
import { getMeshColor } from '../../lib/meshColors';
import { ProviderIcon } from '../Providers/ProviderIcon';
import { InlineEditableText } from '../shared/InlineEditableText';

interface GridNodeHeaderProps {
  node: AgentNode;
  onBuildRun: (nodeId: number, mode: 'build' | 'run' | 'terminal') => void;
}

export function GridNodeHeader({ node, onBuildRun }: GridNodeHeaderProps) {
  const toggleFileExplorer = useUIStore(state => state.toggleFileExplorer);
  const fileExplorerContext = useUIStore(state => state.fileExplorerContext);
  const deleteAgentNode = useAgentNodeStore(state => state.deleteAgentNode);
  const renameAgentNode = useAgentNodeStore(state => state.renameAgentNode);
  const meshesById = useMeshStore(state => state.meshesById);
  const meshColor = getMeshColor(node.mesh_id);

  const handleClose = async (e: React.MouseEvent) => {
    e.stopPropagation();
    await deleteAgentNode(node.id);
  };

  const gitPath = getNodeGitPath(node);
  const { summary } = useGitSummary(gitPath || null);

  const isPanelNode =
    fileExplorerContext?.type === 'agent' && fileExplorerContext.nodeId === node.id;

  const meshLabel = useMemo(() => {
    const m = meshesById.get(node.mesh_id);
    return m ? `[${m.name} #${node.id}]` : `[#${node.id}]`;
  }, [meshesById, node.mesh_id, node.id]);

  return (
    <div
      className="flex items-center justify-between px-2.5 py-1.5 border-b border-border-default"
      style={{ backgroundColor: `${meshColor.hex}40` }}
    >
      <div className="flex items-center gap-2 overflow-hidden">
        <span className={`w-1.5 h-1.5 rounded-full ${getStatusConfig(node.status).bgColor}`} />
        <ProviderIcon providerId={node.provider} className="h-3.5 w-3.5 drop-shadow-sm" />
        <span className="text-[12px] font-semibold text-text-primary truncate font-sans drop-shadow-sm">
          <InlineEditableText
            value={node.name}
            onCommit={(next) => renameAgentNode(node.id, next)}
            className="text-[12px] font-semibold text-text-primary font-sans drop-shadow-sm"
          /> <span className="text-text-secondary font-normal">{meshLabel}</span>
        </span>
        {summary && (
          <span
            onClick={(e) => { e.stopPropagation(); toggleFileExplorer({ type: 'agent', nodeId: node.id, path: gitPath }); }}
            className="text-[11px] font-mono font-semibold cursor-pointer flex items-center gap-1.5 drop-shadow-sm hover:brightness-125"
            title="Click to see changes"
          >
            {/* Each count carries its own semantic colour so added / modified / deleted
                read at a glance against the mesh-tinted header. Zero counts stay muted
                so the eye lands on the changes that exist. When this node owns the
                agent file-explorer panel the whole chip flips to cyan as a selection
                cue, matching the panel border. */}
            <span className={isPanelNode ? 'text-accent-cyan' : summary.added ? 'text-green-400' : 'text-text-muted'}>
              +{summary.added}
            </span>
            <span className={isPanelNode ? 'text-accent-cyan' : summary.modified ? 'text-amber-400' : 'text-text-muted'}>
              ~{summary.modified}
            </span>
            <span className={isPanelNode ? 'text-accent-cyan' : summary.deleted ? 'text-red-400' : 'text-text-muted'}>
              -{summary.deleted}
            </span>
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
