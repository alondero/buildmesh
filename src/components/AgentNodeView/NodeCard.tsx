import { useDraggable, useDroppable } from '@dnd-kit/core';
import type { KeyboardEvent } from 'react';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { AgentTerminal } from '../Terminal/Terminal';
import { BuildRunTerminal } from '../Terminal/BuildRunTerminal';
import { GridNodeHeader } from './GridNodeHeader';
import { NodeDropCue } from './nodeDrag';
import { SemanticTurnBanner } from './SemanticTurnBanner';

export type BuildRunState = { nodeId: number; mode: 'build' | 'run' | 'terminal' } | null;

interface NodeCardProps {
  nodeId: number;
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
///
/// Issue #1384 — `NodeCard` now subscribes to its specific node via
/// `state.nodesById[nodeId]` instead of receiving the full node as a prop.
/// The store's shallow reconciliation (see `agentNodeStore.ts`) keeps the
/// same object reference for unchanged nodes, so this selector only fires
/// a re-render when THIS specific node changes (status flip, rename, pin,
/// etc.). Other nodes' attention events no longer cascade into this card —
/// satisfying the spec's "Updating or polling node A does NOT trigger
/// re-renders in components subscribed to node B" acceptance criterion
/// directly at the card level, not just at the terminal level.
///
/// `isActive`/`onActivate` are passed in (not subscribed here) so the parent
/// keeps its single activeNodeId subscription and per-card renders stay cheap.
///
/// When `draggable`, the title bar is a dnd-kit drag handle and the whole card
/// is a drop target; collision is rect/pointer-based, so the xterm canvas in
/// the body never blocks a drop. We deliberately ignore the draggable transform
/// (a DragOverlay renders the moving preview) and just dim the source instead.
export function NodeCard({ nodeId, isActive, onActivate, onBuildRun, buildRunOpen, setBuildRunOpen, draggable = true }: NodeCardProps) {
  // Per-id subscription — see component docstring.
  const node = useAgentNodeStore((s) => s.nodesById[nodeId]);
  // All hooks below MUST run unconditionally (Rules of Hooks). The
  // `node?.id` guard handles the not-yet-loaded case; the JSX section
  // uses `if (!node) return null` once all hooks have run.
  const isClosing = useAgentNodeStore((s) => s.closingNodeIds.has(nodeId));
  // Semantic attention (Y/N/Enter keyboard shortcuts) — main added this
  // for the attention UX work; kept as a per-id subscription so unrelated
  // nodes' semantic-turn state doesn't cascade into this card.
  const semanticTurn = useAgentNodeStore((s) => s.semanticTurns[nodeId]);
  const writeToAgent = useAgentNodeStore((s) => s.writeToAgent);
  const clearAttention = useAgentNodeStore((s) => s.clearAttention);
  // dnd-kit hooks (useDraggable / useDroppable) MUST run unconditionally.
  // They internally call React hooks; placing them after the early-return
  // would skip them on the re-render where the node was just removed
  // (e.g. after `deleteAgentNode` removes the row from `nodesById`),
  // tripping React's "rendered fewer hooks than expected" assertion.
  // We always run them but only consume their `listeners`/`attributes`/
  // ref setters when the node is loaded — they're no-ops otherwise.
  const dragData = { nodeId, meshId: node?.mesh_id ?? 0 };
  const { setNodeRef: setDragRef, listeners, attributes, isDragging } = useDraggable({
    id: `node-drag-${nodeId}`,
    data: dragData,
    disabled: !draggable,
  });
  const { setNodeRef: setDropRef } = useDroppable({
    id: `node-drop-${nodeId}`,
    data: dragData,
    disabled: !draggable,
  });
  const setRefs = (el: HTMLDivElement | null) => { setDragRef(el); setDropRef(el); };

  if (!node) return null;
  const isBuildRunOpen = buildRunOpen?.nodeId === nodeId ? buildRunOpen.mode : null;

  // Y/N/Enter shortcut handler for semantic attention. The keyboard
  // listener is attached via React's `onKeyDown` so React's synthetic
  // event system handles delegation — we don't manually bind a document
  // listener. The `node` reference is safe to read here because the
  // early-return above guarantees it.
  const handleNodeKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!isActive || !semanticTurn || event.repeat || event.altKey || event.ctrlKey || event.metaKey) return;
    const target = event.target as HTMLElement;
    if (!target.classList.contains('xterm-helper-textarea')) return;
    const key = event.key.toLowerCase();
    if (semanticTurn.kind === 'turn_finished' && key === 'enter') {
      event.preventDefault();
      void clearAttention(node.id);
    } else if (semanticTurn.kind !== 'turn_finished' && (key === 'y' || key === 'n' || key === 'enter')) {
      event.preventDefault();
      void writeToAgent(node.id, key === 'n' ? 'n\r' : 'y\r');
    }
  };

  const borderClass = node.status === 'awaiting_input'
    ? 'border-status-warning animate-border-pulse'
    : isActive
      ? 'border-accent-cyan shadow-[0_0_0_2px_var(--color-accent-cyan),0_0_16px_3px_var(--color-accent-cyan-dim)]'
      : 'border-border-default hover:border-accent-cyan/50';

  return (
    <div
      ref={setRefs}
      onClick={() => { if (!isActive) onActivate(nodeId); }}
      onKeyDown={handleNodeKeyDown}
      className={`relative flex-1 flex flex-col bg-bg-card border-2 rounded-sm overflow-hidden group transition-[color,background-color,border-color,opacity] ${borderClass} ${isDragging ? 'opacity-40' : ''}`}
    >
      <GridNodeHeader
        nodeId={nodeId}
        onBuildRun={onBuildRun}
        dragHandleProps={draggable ? { ...listeners, ...attributes } : undefined}
      />
      {node.status === 'awaiting_input' && semanticTurn && (
        <SemanticTurnBanner
          turn={semanticTurn}
          isActive={isActive}
          onResolve={(data) => { void writeToAgent(node.id, data); }}
          onFinish={() => { void clearAttention(node.id); }}
        />
      )}
      <div className="flex-1 flex flex-col overflow-hidden bg-black">
        <div className={`${isBuildRunOpen ? 'flex-[2]' : 'flex-1'} overflow-hidden`}>
          <AgentTerminal nodeId={node.id} />
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
      {isClosing && (
        <div
          className="absolute inset-0 z-20 flex flex-col items-center justify-center gap-3 bg-bg-card/80 backdrop-blur-sm"
          aria-busy="true"
        >
          <span className="inline-block h-8 w-8 animate-spin rounded-full border-2 border-text-muted border-t-transparent" />
          <span className="text-text-secondary text-sm font-sans">Closing…</span>
        </div>
      )}
    </div>
  );
}
