import { useMemo } from 'react';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { BuildRunDropdown } from '../BuildRun/BuildRunDropdown';
import { useGitSummary } from '../../hooks/useGitSummary';
import { useOpenPr } from '../../hooks/useOpenPr';
import { getNodeGitPath } from '../../lib/paths';
import { getStatusConfig } from '../../lib/status';
import { getMeshColor } from '../../lib/meshColors';
import { ProviderIcon } from '../Providers/ProviderIcon';
import { InlineEditableText } from '../shared/InlineEditableText';
import { openUrl } from '@tauri-apps/plugin-opener';

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
  const { pr: openPr } = useOpenPr(node.id, gitPath || null);

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
        <span
          title={node.use_worktree
            ? 'Agent runs in a git worktree'
            : 'Agent runs in the repository root'}
          className={`text-[10px] font-mono px-1.5 py-0.5 rounded-full leading-none font-medium select-none whitespace-nowrap drop-shadow-sm ${
            node.use_worktree
              ? 'bg-bg-overlay/70 text-text-muted ring-1 ring-inset ring-border-subtle'
              : 'bg-accent-cyan/15 text-accent-cyan ring-1 ring-inset ring-accent-cyan/40 font-semibold'
          }`}
        >
          {node.use_worktree ? 'worktree' : 'root'}
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
        {/* Open PR chip — surfaces a clickable link to the PR for the branch
            this node is working on. Hidden when no PR is open (useOpenPr
            returns null for the common cases: no auth, no PR, non-GitHub
            origin, unborn branch). Tooltip carries the PR title; if the PR
            is a draft, the tooltip is suffixed so the user knows. */}
        {openPr && (
          <span
            onClick={(e) => {
              e.stopPropagation();
              openUrl(openPr.url).catch(console.error);
            }}
            title={openPr.draft ? `Draft · ${openPr.title}` : openPr.title}
            className="text-[10px] font-mono px-1.5 py-0.5 rounded-full leading-none font-medium select-none cursor-pointer whitespace-nowrap bg-green-400/10 text-green-400 ring-1 ring-inset ring-green-400/30 drop-shadow-sm hover:brightness-125 transition-colors"
          >
            PR #{openPr.number}
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
