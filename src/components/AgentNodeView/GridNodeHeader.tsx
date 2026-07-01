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
import { isMac } from '../../lib/platform';

interface GridNodeHeaderProps {
  node: AgentNode;
  onBuildRun: (nodeId: number, mode: 'build' | 'run' | 'terminal') => void;
  /// dnd-kit drag listeners/attributes that turn the whole title bar into the
  /// reorder/swap drag handle. Undefined when dragging is disabled (e.g. the
  /// maximized solo view, or in isolation tests).
  dragHandleProps?: Record<string, unknown>;
}

export function GridNodeHeader({ node, onBuildRun, dragHandleProps }: GridNodeHeaderProps) {
  // Issue #376: the chip now opens the unified Probe Panel on the 🔍
  // (Agent Changes) tab for this node, rather than toggling the legacy
  // FileExplorerPanel in the SessionView left pane (deleted in #380; the
  // `AgentChangesTab` review surface is the only one now).
  const openProbeTab = useUIStore(state => state.openProbeTab);
  const probeOpen = useUIStore(state => state.probeOpen);
  const probeTab = useUIStore(state => state.probeTab);
  // Boolean selector (not the raw id) so only the two headers whose maximized
  // status actually flips re-render on a toggle — not every header in the grid.
  const isMaximized = useUIStore(state => state.maximizedNodeId === node.id);
  const toggleMaximizedNode = useUIStore(state => state.toggleMaximizedNode);
  const deleteAgentNode = useAgentNodeStore(state => state.deleteAgentNode);
  const renameAgentNode = useAgentNodeStore(state => state.renameAgentNode);
  // The chip's click focuses the node before opening the probe — the
  // `AgentChangesTab` reads `useProbeContext().activeNodeId` to pick
  // which node's review to render, so without this the user could land
  // on a different terminal's review if a different node was already
  // focused. (The pre-#376 left-pane `FileExplorerPanel` accepted an
  // explicit `nodeId` per click; the new probe context derivation
  // makes "focus" the natural way to express "review THIS node".)
  const setActiveNode = useAgentNodeStore((state) => state.setActiveNode);
  // The cyan chip highlight (post-#376) signals "the probe is showing
  // this node's review right now". `AgentChangesTab` reads `activeNodeId`
  // from this store to pick which node's review to render, so we compare
  // the same value to keep the highlight and the body in sync.
  const isReviewingThisNode = useAgentNodeStore((s) => s.activeNodeId === node.id);
  const meshesById = useMeshStore(state => state.meshesById);
  const meshColor = getMeshColor(node.mesh_id);

  const handleClose = async (e: React.MouseEvent) => {
    e.stopPropagation();
    await deleteAgentNode(node.id);
  };

  const gitPath = getNodeGitPath(node);
  const { summary } = useGitSummary(gitPath || null);
  const { pr: openPr } = useOpenPr(node.id, gitPath || null);

  const isPanelNode = probeOpen && probeTab === 'review' && isReviewingThisNode;

  const meshLabel = useMemo(() => {
    const m = meshesById.get(node.mesh_id);
    return m ? `[${m.name} #${node.id}]` : `[#${node.id}]`;
  }, [meshesById, node.mesh_id, node.id]);

  // Issue #668 — advertise the Alt+G / Cmd+G shortcut in the title tooltip
  // alongside the existing double-click affordance, so discoverability
  // doesn't depend on the empty-state splash being on screen.
  const toggleShortcutHint = `${isMac ? '⌘' : 'Alt'}+G`;

  return (
    <div
      {...dragHandleProps}
      onDoubleClick={() => toggleMaximizedNode(node.id)}
      title={isMaximized
        ? `Double-click or press ${toggleShortcutHint} to restore grid`
        : `Double-click or press ${toggleShortcutHint} to maximize`}
      className={`flex items-center justify-between px-2.5 py-1.5 border-b border-border-default ${dragHandleProps ? 'cursor-grab active:cursor-grabbing' : ''}`}
      style={{ backgroundColor: `${meshColor.hex}40` }}
    >
      <div className="flex items-center gap-2 overflow-hidden">
        {dragHandleProps && (
          <span
            aria-hidden="true"
            title="Drag to reorder, or onto another node to swap"
            className="text-text-muted text-[11px] leading-none opacity-0 group-hover:opacity-60 transition-opacity select-none"
          >
            ⠿
          </span>
        )}
        <span className={`w-1.5 h-1.5 rounded-full ${getStatusConfig(node.status).bgColor}`} />
        <ProviderIcon providerId={node.provider} className="h-3.5 w-3.5 drop-shadow-sm" />
        <span
          onPointerDown={(e) => e.stopPropagation()}
          className="text-[12px] font-semibold text-text-primary truncate font-sans drop-shadow-sm"
        >
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
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => { e.stopPropagation(); setActiveNode(node.id); openProbeTab('review'); }}
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
            onPointerDown={(e) => e.stopPropagation()}
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
      <div className="flex items-center gap-1.5" onPointerDown={(e) => e.stopPropagation()}>
        <BuildRunDropdown node={node} onBuildRun={onBuildRun} />
        <button
          onClick={(e) => { e.stopPropagation(); toggleMaximizedNode(node.id); }}
          className="w-4 h-4 flex items-center justify-center rounded-md text-text-muted hover:text-accent-cyan hover:bg-bg-base transition-colors opacity-0 group-hover:opacity-100 focus:opacity-100"
          // Issue #668 — surface the Alt+G / ⌘+G shortcut in the button
          // tooltip so discoverability isn't gated on the header double-click
          // or the empty-state splash.
          title={isMaximized ? `Restore grid (or ${toggleShortcutHint})` : `Maximize (or ${toggleShortcutHint})`}
          aria-label={isMaximized ? 'Restore grid layout' : 'Maximize agent node'}
        >
          {isMaximized ? (
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <path d="M9 9H4m0 0V4m0 5 6-6m5 16v-5m0 0h5m-5 0 6 6M9 15H4m0 0v5m0-5 6 6m5-16V4m0 0h5m-5 0 6 6" />
            </svg>
          ) : (
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <path d="M15 3h6m0 0v6m0-6-7 7M9 21H3m0 0v-6m0 6 7-7" />
            </svg>
          )}
        </button>
        <button
          onClick={handleClose}
          className="w-4 h-4 flex items-center justify-center rounded-md text-text-muted hover:text-accent-cyan hover:bg-bg-base transition-colors text-[10px]"
          title="Close agent node"
        >
          ×
        </button>
      </div>
    </div>
  );
}
