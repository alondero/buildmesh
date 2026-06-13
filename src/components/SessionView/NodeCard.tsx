import { useDraggable, useDroppable } from '@dnd-kit/core';
import { type AgentNode } from '../../stores/agentNodeStore';
import { AgentTerminal } from '../Terminal/Terminal';
import { BuildRunTerminal } from '../Terminal/BuildRunTerminal';
import { GridNodeHeader } from './GridNodeHeader';
import { NodeDropCue } from './nodeDrag';

export type BuildRunState = { nodeId: number; mode: 'build' | 'run' | 'terminal' } | null;

interface NodeCardProps {
  node: AgentNode;
  isActive: boolean;
  onActivate: (nodeId: number) => void;
  onBuildRun: (nodeId: number, mode: 'build' | 'run' | 'terminal') => void;
  buildRunOpen: BuildRunState;
  setBuildRunOpen: (val: BuildRunState) => void;
  /// When false (e.g. the maximized solo view), the card is not a drag target
  /// or handle — there's nothing to reorder against.
  draggable?: boolean;
}

/// The single source of truth for an agent node's card: tinted header + agent
/// terminal + optional build/run pane. Shared by the 1–2 node pane view, the
/// 3+ node grid, and the maximized/solo view (#65) so all three stay in lockstep.
/// `isActive`/`onActivate` are passed in (not subscribed here) so the parent
/// keeps its single activeNodeId subscription and per-card renders stay cheap.
///
/// When `draggable`, the title bar is a dnd-kit drag handle and the whole card
/// is a drop target; collision is rect/pointer-based, so the xterm canvas in
/// the body never blocks a drop. We deliberately ignore the draggable transform
/// (a DragOverlay renders the moving preview) and just dim the source instead.
export function NodeCard({ node, isActive, onActivate, onBuildRun, buildRunOpen, setBuildRunOpen, draggable = true }: NodeCardProps) {
  const isBuildRunOpen = buildRunOpen?.nodeId === node.id ? buildRunOpen.mode : null;

  const dragData = { nodeId: node.id, meshId: node.mesh_id };
  const { setNodeRef: setDragRef, listeners, attributes, isDragging } = useDraggable({
    id: `node-drag-${node.id}`,
    data: dragData,
    disabled: !draggable,
  });
  const { setNodeRef: setDropRef } = useDroppable({
    id: `node-drop-${node.id}`,
    data: dragData,
    disabled: !draggable,
  });
  const setRefs = (el: HTMLDivElement | null) => { setDragRef(el); setDropRef(el); };

  const borderClass = node.status === 'awaiting_input'
    ? 'border-status-warning animate-border-pulse'
    : isActive
      ? 'border-accent-cyan shadow-[0_0_0_2px_var(--color-accent-cyan),0_0_16px_3px_var(--color-accent-cyan-dim)]'
      : 'border-border-default hover:border-accent-cyan/50';

  return (
    <div
      ref={setRefs}
      onClick={() => { if (!isActive) onActivate(node.id); }}
      className={`relative flex-1 flex flex-col bg-bg-card border-2 rounded-sm overflow-hidden group transition-colors ${borderClass} ${isDragging ? 'opacity-40' : ''}`}
    >
      <GridNodeHeader
        node={node}
        onBuildRun={onBuildRun}
        dragHandleProps={draggable ? { ...listeners, ...attributes } : undefined}
      />
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
      {draggable && <NodeDropCue nodeId={node.id} />}
    </div>
  );
}
