import { useDraggable, useDroppable } from '@dnd-kit/core';
import { Suspense, lazy, useEffect, type KeyboardEvent } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { activityRootId, activityStatus } from '../../lib/nodeActivities';
import { useNodeActivityStore } from '../../stores/nodeActivityStore';
import { AgentTerminal } from '../Terminal/Terminal';
import { GridNodeHeader } from './GridNodeHeader';
import { NodeDropCue } from './nodeDrag';
import { SemanticTurnBanner } from './SemanticTurnBanner';

// Keep the utility terminal's second xterm registry out of the initial bundle.
const BuildRunTerminal = lazy(() => import('../Terminal/BuildRunTerminal').then((m) => ({ default: m.BuildRunTerminal })));

interface NodeCardProps {
  nodeId: number;
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
export function NodeCard({ nodeId, isActive, onActivate, draggable = true }: NodeCardProps) {
  const members = useAgentNodeStore(useShallow(s => {
    const all = s.nodeIds.map(id => s.nodesById[id]).filter(Boolean);
    return all.filter(n => n.status !== 'archived' && activityRootId(n.id, all, s.circuitOwnerships) === nodeId);
  }));
  const activeNodeId = useAgentNodeStore(s => s.activeNodeId);
  const selection = useNodeActivityStore(s => s.selections[nodeId]);
  const utilities = useNodeActivityStore(s => s.utilities);
  const select = useNodeActivityStore(s => s.select);
  const openUtility = useNodeActivityStore(s => s.openUtility);
  const closeUtility = useNodeActivityStore(s => s.closeUtility);
  const activeMember = members.find(n => n.id === activeNodeId);
  const selectedId = activeMember?.id ?? members.find(n => n.id === selection?.nodeId)?.id ?? nodeId;
  const utilityMode = utilities[selectedId];
  const showingUtility = selection?.nodeId === selectedId && !!selection?.utility && !!utilityMode;
  const cardActive = isActive || !!activeMember;
  useEffect(() => {
    if (activeMember && selection?.nodeId !== activeMember.id) select(nodeId, activeMember.id);
  }, [activeMember?.id, selection?.nodeId, nodeId, select]);
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
  const hasTabs = members.length > 1 || members.some(n => utilities[n.id]);
  const choose = (id: number, utility = false) => {
    onActivate(id);
    select(nodeId, id, utility);
  };
  const tabs = members.flatMap(member => {
    const label = member.id === nodeId ? (members.length > 1 ? 'Implementation' : 'Agent') : 'Review';
    const agent = { key: `agent-${member.id}`, nodeId: member.id, utility: false, label, status: member.status };
    const mode = utilities[member.id];
    return mode ? [agent, { key: `utility-${member.id}`, nodeId: member.id, utility: true,
      label: `${mode[0].toUpperCase()}${mode.slice(1)}${member.id === nodeId ? '' : ' · Review'}`, status: '' }] : [agent];
  });

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
      ? 'border-accent-cyan shadow-[0_0_0_2px_var(--color-accent-cyan),0_0_16px_3px_var(--color-accent-cyan-dim)]'
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
      {members.length > 1 && (
        <div className="flex min-w-0 items-center justify-between gap-2 px-2 py-1 text-xs bg-bg-base">
          <span className="truncate" title={root.name}>{root.name}</span>
          <span role="status" className="shrink-0 text-accent-cyan">{activityStatus(root, members)}</span>
        </div>
      )}
      <GridNodeHeader
        nodeId={selectedId}
        onBuildRun={(id, mode) => { onActivate(id); openUtility(nodeId, id, mode); }}
        dragHandleProps={draggable ? { ...listeners, ...attributes } : undefined}
      />
      {hasTabs && (
        <div role="tablist" aria-label="Node activities" className="flex shrink-0 overflow-x-auto border-b border-border-default bg-bg-base">
          {tabs.map((tab, index) => {
            const selected = tab.nodeId === selectedId && tab.utility === showingUtility;
            return <button key={tab.key} type="button" role="tab"
              id={`activity-${nodeId}-${tab.key}`} aria-controls={`activity-panel-${nodeId}`}
              aria-label={`${tab.label}${tab.status ? ` ${tab.status.replace(/_/g, ' ')}` : ''}`}
              aria-selected={selected} tabIndex={selected ? 0 : -1}
              title={`${tab.label}${tab.status ? `: ${tab.status.replace(/_/g, ' ')}` : ''} · ${members.find(n => n.id === tab.nodeId)?.name}`}
              onClick={event => { event.stopPropagation(); choose(tab.nodeId, tab.utility); }}
              onKeyDown={event => {
                const target = event.key === 'ArrowRight' ? (index + 1) % tabs.length
                  : event.key === 'ArrowLeft' ? (index + tabs.length - 1) % tabs.length
                  : event.key === 'Home' ? 0 : event.key === 'End' ? tabs.length - 1 : null;
                if (target === null) return;
                event.preventDefault();
                choose(tabs[target].nodeId, tabs[target].utility);
                document.getElementById(`activity-${nodeId}-${tabs[target].key}`)?.focus();
              }}
              className={`shrink-0 px-3 py-2 text-xs border-b-2 ${selected ? 'border-accent-cyan text-accent-cyan' : 'border-transparent text-text-secondary hover:text-text-primary'}`}>
              {tab.label}{tab.status && <span className="ml-2 text-2xs">{' '}{tab.status.replace(/_/g, ' ')}</span>}
            </button>;
          })}
        </div>
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
        {!showingUtility && <AgentTerminal key={node.id} nodeId={node.id} />}
        {showingUtility && utilityMode && (
          <Suspense fallback={<span className="p-2 text-text-muted">Loading terminal…</span>}>
            <BuildRunTerminal
              key={`${node.id}-${utilityMode}`}
              sessionId={node.id}
              mode={utilityMode}
              useWorktree={node.use_worktree}
              onClose={() => closeUtility(nodeId, node.id)}
            />
          </Suspense>
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
