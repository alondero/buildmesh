import type { AgentNode } from '../../stores/agentNodeStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { getStatusConfig } from '../../lib/status';
import { getMeshColor } from '../../lib/meshColors';
import { ProviderIcon } from '../Providers/ProviderIcon';
import { InlineEditableText } from '../shared/InlineEditableText';

interface NodeItemProps {
  node: AgentNode;
  meshColor: ReturnType<typeof getMeshColor>;
  isActive: boolean;
  onSelect: () => void;
  onDelete: (e: React.MouseEvent) => void;
}

export function NodeItem({ node, meshColor, isActive, onSelect, onDelete }: NodeItemProps) {
  const config = getStatusConfig(node.status);
  const renameAgentNode = useAgentNodeStore((s) => s.renameAgentNode);
  const spawnAgent = useAgentNodeStore((s) => s.spawnAgent);
  // Closing a node first runs a worktree safety check that can take seconds on
  // a large repo; until it resolves the row stays on screen, so show a spinner
  // (and stop reacting to clicks) rather than letting the click look ignored.
  const isClosing = useAgentNodeStore((s) => s.closingNodeIds.has(node.id));
  // 'error' is the false-positive status the app-exit / post-pump race
  // leaves behind (see agent/spawn.rs:419-438 vs lib.rs:247-253) — the
  // user never got a chance to actually use the node, so the
  // meaningful action is "retry the spawn", not "delete". The store's
  // spawnAgent passes `cli_session_id` as the resume argument, so a
  // click re-attempts the same --resume the failed auto-resume tried.
  const showRestart = node.status === 'error';
  return (
    <div
      data-session-item
      data-session-id={node.id}
      onClick={isClosing ? undefined : onSelect}
      aria-busy={isClosing}
      style={{ backgroundColor: isActive ? undefined : `${meshColor.hex}40` }}
      className={`
        pl-3 pr-1 py-1.5 rounded text-sm mb-0.5 flex items-center gap-2 group/node
        ${isClosing ? 'opacity-50 pointer-events-none cursor-default' : 'cursor-pointer'}
        ${isActive ? 'border border-accent-cyan/50' : 'hover:brightness-125 border border-transparent'}
      `}
    >
      <span
        className={`${config.color} inline-flex h-3 w-3 shrink-0 items-center justify-center text-xs leading-none`}
        title={config.label}
      >
        {config.dot}
      </span>
      <ProviderIcon providerId={node.provider} className="h-3 w-3 opacity-90" />
      <InlineEditableText
        value={node.name}
        onCommit={(next) => renameAgentNode(node.id, next)}
        className="flex-1 truncate text-text-primary font-sans text-left text-sm"
      />
      {showRestart && (
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            spawnAgent(node.id, node.provider).catch((err) => {
              console.error('[NodeItem] Restart failed:', err);
            });
          }}
          className="text-text-muted hover:text-status-warning text-xs px-1 transition-colors opacity-0 group-hover/node:opacity-100 group-focus-within/node:opacity-100 focus-visible:opacity-100"
          title="Restart agent"
          aria-label={`Restart ${node.name}`}
        >
          ↻
        </button>
      )}
      {isClosing ? (
        <span
          className="text-text-muted text-xs px-1 flex items-center"
          title="Closing…"
          aria-label="Closing"
        >
          <span className="inline-block h-3 w-3 animate-spin rounded-full border border-current border-t-transparent" />
        </span>
      ) : (
        <button
          type="button"
          onClick={onDelete}
          className="text-text-muted hover:text-status-error text-xs px-1 transition-colors opacity-0 group-hover/node:opacity-100 group-focus-within/node:opacity-100 focus-visible:opacity-100"
          title="Delete node"
          aria-label={`Delete ${node.name}`}
        >
          ×
        </button>
      )}
    </div>
  );
}
