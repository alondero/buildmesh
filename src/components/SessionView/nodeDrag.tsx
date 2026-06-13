import { createContext, useContext } from 'react';
import { type AgentNode } from '../../stores/agentNodeStore';
import { ProviderIcon } from '../Providers/ProviderIcon';

/// What dropping right now would do, relative to a specific target node.
/// `null` = no valid target under the pointer (e.g. cross-mesh, or off-grid).
export type DropIntent =
  | { kind: 'swap'; targetNodeId: number }
  | { kind: 'insert-before'; targetNodeId: number }
  | { kind: 'insert-after'; targetNodeId: number }
  | null;

/// Current drop intent, broadcast to every NodeDropCue. Kept in a context so a
/// high-frequency drag-move only re-renders the small cue overlays, never the
/// node cards (which host imperative xterm terminals we must not churn).
export const DropIntentContext = createContext<DropIntent>(null);
export const useDropIntent = () => useContext(DropIntentContext);

/// Decide insert-vs-swap from where the pointer sits across the target node:
/// the outer thirds mean "insert on that side" (drop between nodes), the middle
/// means "swap bodies". Cross-mesh hovers and self-swaps return null so the UI
/// shows no cue and the drop is ignored. Pure + side-effect-free so it can be
/// unit-tested without a DOM or dnd-kit.
export function computeDropIntent(params: {
  overNodeId: number | null;
  overMeshId: number | null;
  overRectLeft: number;
  overRectWidth: number;
  pointerX: number;
  draggedId: number;
  draggedMeshId: number;
}): DropIntent {
  const { overNodeId, overMeshId, overRectLeft, overRectWidth, pointerX, draggedId, draggedMeshId } = params;
  if (overNodeId == null || overMeshId == null) return null;
  if (overMeshId !== draggedMeshId) return null; // same-mesh reordering only
  const ratio = overRectWidth > 0 ? (pointerX - overRectLeft) / overRectWidth : 0.5;
  if (ratio < 0.3) return { kind: 'insert-before', targetNodeId: overNodeId };
  if (ratio > 0.7) return { kind: 'insert-after', targetNodeId: overNodeId };
  if (overNodeId === draggedId) return null; // a node can't swap with itself
  return { kind: 'swap', targetNodeId: overNodeId };
}

/// The drop indicator drawn over a node while it's the drag target: a glowing
/// edge line for an insert, or a ringed "⇄ Swap" badge for a swap. Decorative
/// only (pointer-events-none) — drop detection is dnd-kit's job.
export function NodeDropCue({ nodeId }: { nodeId: number }) {
  const intent = useDropIntent();
  if (!intent || intent.targetNodeId !== nodeId) return null;

  if (intent.kind === 'swap') {
    return (
      <div className="pointer-events-none absolute inset-0 z-20 flex items-center justify-center rounded-sm ring-2 ring-inset ring-accent-cyan bg-accent-cyan/10">
        <span className="px-2.5 py-1 rounded-full bg-accent-cyan text-bg-base text-[11px] font-semibold font-sans shadow-lg select-none">
          ⇄ Swap
        </span>
      </div>
    );
  }

  const side = intent.kind === 'insert-before' ? 'left-0' : 'right-0';
  return (
    <div
      className={`pointer-events-none absolute top-0 bottom-0 ${side} z-20 w-1 -mx-0.5 rounded-full bg-accent-cyan shadow-[0_0_8px_2px_var(--color-accent-cyan)]`}
    />
  );
}

/// The floating preview that follows the cursor during a drag (rendered inside
/// dnd-kit's DragOverlay). A compact echo of the node's title bar so the user
/// always sees which node they're carrying.
export function NodeDragPreview({ node }: { node: AgentNode }) {
  return (
    <div className="flex items-center gap-2 px-2.5 py-1.5 rounded-sm bg-bg-card/95 ring-1 ring-accent-cyan shadow-2xl cursor-grabbing select-none max-w-[260px]">
      <ProviderIcon providerId={node.provider} className="h-3.5 w-3.5 shrink-0" />
      <span className="text-[12px] font-semibold text-text-primary truncate font-sans">{node.name}</span>
    </div>
  );
}
