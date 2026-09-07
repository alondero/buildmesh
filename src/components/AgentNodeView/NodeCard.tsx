import { useDraggable, useDroppable } from '@dnd-kit/core';
import { memo, Suspense, lazy, useMemo, useState, type KeyboardEvent } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { activityStatus } from '../../lib/nodeActivities';
import { useNodeActivityStore } from '../../stores/nodeActivityStore';
import { AgentTerminal, terminalManager } from '../Terminal/Terminal';
import { GridNodeHeader } from './GridNodeHeader';
import { NodeDropCue } from './nodeDrag';
import { SemanticTurnBanner } from './SemanticTurnBanner';
import { NodeActivityTabs } from './NodeActivityTabs';
import { buildRunTerminalManager } from '../Terminal/BuildRunTerminalRegistry';

// Keep the utility terminal's second xterm registry out of the initial bundle.
const BuildRunTerminal = lazy(() => import('../Terminal/BuildRunTerminal').then((m) => ({ default: m.BuildRunTerminal })));

interface NodeCardProps {
  nodeId: number;
  memberIds?: readonly number[];
  isActive: boolean;
  onActivate: (nodeId: number) => void;
  /// When false (e.g. the maximized solo view), the card is not a drag target
  /// or handle — there's nothing to reorder against.
  draggable?: boolean;
}

/// The single source of truth for an agent node's card: tinted header + agent
/// terminal and activity tabs. Shared by the 1–2 node pane view, the
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
/// Related agents use a shallow member selector; the active agent determines
/// which member's terminal and controls the card presents.
///
/// When `draggable`, the title bar is a dnd-kit drag handle and the whole card
/// is a drop target; collision is rect/pointer-based, so the xterm canvas in
/// the body never blocks a drop. We deliberately ignore the draggable transform
/// (a DragOverlay renders the moving preview) and just dim the source instead.
function NodeCardView({ nodeId, memberIds: memberIdsProp, isActive, onActivate, draggable = true }: NodeCardProps) {
  const [keyboardSelection, setKeyboardSelection] = useState<{ nodeId: number; utility: boolean } | null>(null);
  const memberIds = memberIdsProp ?? [nodeId];
  const memberIdKey = memberIds.join(',');
  const stableMemberIds = useMemo(() => memberIds, [memberIdKey]);
  const members = useAgentNodeStore(useShallow(s => stableMemberIds
    .map(id => s.nodesById[id])
    .filter((node): node is NonNullable<typeof node> => !!node && node.status !== 'archived')));
  const activeNodeId = useAgentNodeStore(s => s.activeNodeId);
  const selection = useNodeActivityStore(s => s.selections[nodeId]);
  const utilityModes = useNodeActivityStore(useShallow(s => stableMemberIds.map(id => s.utilities[id])));
  const utilities = useMemo(
    () => new Map(stableMemberIds.map((id, index) => [id, utilityModes[index]] as const)),
    [stableMemberIds, utilityModes],
  );
  const select = useNodeActivityStore(s => s.select);
  const openUtility = useNodeActivityStore(s => s.openUtility);
  const closeUtility = useNodeActivityStore(s => s.closeUtility);
  const activeMember = members.find(n => n.id === activeNodeId);
  const selectedId = members.find(n => n.id === selection?.nodeId)?.id ?? activeMember?.id ?? nodeId;
  const selectedUtilityMode = utilities.get(selectedId);
  const showingUtility = selection?.nodeId === selectedId && !!selection?.utility && !!selectedUtilityMode;
  const focusOnAttach = keyboardSelection?.nodeId !== selectedId || keyboardSelection.utility !== showingUtility;
  const cardActive = isActive || !!activeMember;
  // Per-id subscription — see component docstring.
  const root = useAgentNodeStore((s) => s.nodesById[nodeId]);
  const node = useAgentNodeStore((s) => s.nodesById[selectedId]);
  // All hooks below MUST run unconditionally (Rules of Hooks). The
  // `node?.id` guard handles the not-yet-loaded case; the JSX section
  // uses `if (!node) return null` once all hooks have run.
  const isClosing = useAgentNodeStore((s) => s.closingNodeIds.has(selectedId));
  // Semantic attention (Y/N/Enter keyboard shortcuts) — main added this
  // for the attention UX work; kept as a per-id subscription so unrelated
  // nodes' semantic-turn state doesn't cascade into this card.
  const semanticTurn = useAgentNodeStore((s) => s.semanticTurns[selectedId]);
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

  if (!node || !root) return null;
  const hasTabs = members.length > 1 || members.some(n => utilities.get(n.id));
  const choose = (id: number, utility = false, focusTerminal = true) => {
    setKeyboardSelection(focusTerminal ? null : { nodeId: id, utility });
    select(nodeId, id, utility);
    onActivate(id);
    if (focusTerminal) {
      requestAnimationFrame(() => {
        if (utility) {
          const member = members.find(candidate => candidate.id === id);
          const mode = utilities.get(id);
          if (member && mode) buildRunTerminalManager.getInstance(id, mode, member.use_worktree)?.term.focus();
        } else terminalManager.getInstance(id)?.term.focus();
      });
    }
  };
  const status = activityStatus(root, members);
  const attention = members.filter(member => member.status === 'awaiting_input' || member.status === 'error');
  const revealAttention = () => {
    const index = attention.findIndex(member => member.id === selectedId);
    const target = showingUtility && index >= 0 ? attention[index] : attention[(index + 1) % attention.length];
    if (target) choose(target.id);
  };
  const closeTab = (id: number) => {
    const member = members.find(candidate => candidate.id === id);
    const mode = utilities.get(id);
    if (!member || !mode) return;
    void buildRunTerminalManager.dispose(id, mode, member.use_worktree); // allow-dispose — explicit utility-tab close, never a session switch
    closeUtility(nodeId, id);
  };

  // Y/N/Enter shortcut handler for semantic attention. The keyboard
  // listener is attached via React's `onKeyDown` so React's synthetic
  // event system handles delegation — we don't manually bind a document
  // listener. The `node` reference is safe to read here because the
  // early-return above guarantees it.
  const handleNodeKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!cardActive || showingUtility || !semanticTurn || event.repeat || event.altKey || event.ctrlKey || event.metaKey) return;
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

  const borderClass = members.some(n => n.status === 'awaiting_input')
    ? 'border-status-warning animate-border-pulse'
    : cardActive
      ? 'border-accent-cyan/70'
      : 'border-border-default hover:border-accent-cyan/50';

  return (
    <div
      ref={setRefs}
      onClick={() => {
        if (cardActive) return;
        const current = useNodeActivityStore.getState().selections[nodeId];
        if (current && members.some(n => n.id === current.nodeId)) choose(current.nodeId, current.utility);
        else choose(selectedId, showingUtility);
      }}
      onKeyDown={handleNodeKeyDown}
      className={`relative flex-1 flex flex-col bg-bg-card border-2 rounded-sm overflow-hidden group transition-[color,background-color,border-color,opacity] ${borderClass} ${isDragging ? 'opacity-40' : ''}`}
    >
      <GridNodeHeader
        nodeId={selectedId}
        titleNodeId={nodeId}
        activity={hasTabs ? status : undefined}
        attentionCount={attention.length}
        onAttention={revealAttention}
        onBuildRun={(id, mode) => { setKeyboardSelection(null); openUtility(nodeId, id, mode); onActivate(id); }}
        dragHandleProps={draggable ? { ...listeners, ...attributes } : undefined}
      />
      {hasTabs && (
        <NodeActivityTabs rootId={nodeId} members={members} utilities={utilities}
          selectedId={selectedId} showingUtility={showingUtility} onSelect={choose} onClose={closeTab} />
      )}
      {!showingUtility && node.status === 'awaiting_input' && semanticTurn && (
        <SemanticTurnBanner
          turn={semanticTurn}
          isActive={cardActive}
          onResolve={(data) => { void writeToAgent(node.id, data); }}
          onFinish={() => { void clearAttention(node.id); }}
        />
      )}
      <div role={hasTabs ? 'tabpanel' : undefined} id={`activity-panel-${nodeId}`}
        aria-labelledby={hasTabs ? `activity-${nodeId}-${showingUtility ? 'utility' : 'agent'}-${selectedId}` : undefined}
        className="min-h-0 flex-1 flex flex-col overflow-hidden bg-black">
        {showingUtility && selectedUtilityMode ? (
          <Suspense fallback={<span className="p-2 text-text-muted">Loading terminal…</span>}>
            <BuildRunTerminal key={`${node.id}-${selectedUtilityMode}`} sessionId={node.id}
              mode={selectedUtilityMode} useWorktree={node.use_worktree} focusOnAttach={focusOnAttach} />
          </Suspense>
        ) : (
          <AgentTerminal key={node.id} nodeId={node.id} focusOnAttach={focusOnAttach} />
        )}
      </div>
      {draggable && <NodeDropCue nodeId={nodeId} />}
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

function sameMemberIds(left: readonly number[], right: readonly number[]): boolean {
  return left.length === right.length && left.every((id, index) => id === right[index]);
}

export const NodeCard = memo(NodeCardView, (previous, next) =>
  previous.nodeId === next.nodeId
  && previous.isActive === next.isActive
  && previous.draggable === next.draggable
  && previous.onActivate === next.onActivate
  && sameMemberIds(previous.memberIds ?? [previous.nodeId], next.memberIds ?? [next.nodeId]));
