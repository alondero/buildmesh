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
  return (
    <div
      data-session-item
      data-session-id={node.id}
      onClick={onSelect}
      style={{ backgroundColor: isActive ? undefined : `${meshColor.hex}40` }}
      className={`
        pl-3 pr-1 py-1.5 rounded cursor-pointer text-[12px] mb-0.5 flex items-center gap-2 group/node
        ${isActive ? 'border border-accent-cyan/50' : 'hover:brightness-125 border border-transparent'}
      `}
    >
      <span className="text-text-muted cursor-grab active:cursor-grabbing text-[10px] opacity-0 group-hover/node:opacity-100 transition-opacity">⋮⋮</span>
      <span className={config.color} title={config.label}>{config.dot}</span>
      <ProviderIcon providerId={node.provider} className="h-3 w-3 opacity-90" />
      <InlineEditableText
        value={node.name}
        onCommit={(next) => renameAgentNode(node.id, next)}
        className="flex-1 truncate text-text-secondary font-sans text-left"
      />
      <button
        onClick={onDelete}
        className="text-text-muted hover:text-status-error text-xs px-1 transition-colors opacity-0 group-hover/node:opacity-100"
        title="Delete node"
      >
        ×
      </button>
    </div>
  );
}
