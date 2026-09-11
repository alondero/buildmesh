import { useRef, useState, useEffect } from 'react';
import { createPortal } from 'react-dom';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
import { useNodeActivityStore } from '../../stores/nodeActivityStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { BuildRunDropdown } from '../BuildRun/BuildRunDropdown';
import { PrPill } from './PrPill';
import { useGitSummary } from '../../hooks/useGitSummary';
import { useOpenPr } from '../../hooks/useOpenPr';
import { useResizeWidth } from '../../hooks/useResizeWidth';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useProviderList } from '../../hooks/useProviderList';
import { useRegenerateAction } from '../../hooks/useRegenerateAction';
import { useSubmenu, focusWithoutScroll } from '../../hooks/useSubmenu';
import { useAriaMenu } from '../../hooks/useAriaMenu';
import { useAnchoredPosition } from '../../hooks/useAnchoredPosition';
import { getNodeGitPath } from '../../lib/paths';
import { getStatusConfig } from '../../lib/status';
import { canResumeSuspendedNode, hasLostConversation } from '../../lib/suspended';
import { MissingSessionIdBadge } from '../shared/MissingSessionIdBadge';
import { SignalHealthBadge } from '../shared/SignalHealthBadge';
import type { SpawnOption } from '../../lib/groups';
import { getMeshColor } from '../../lib/meshColors';
import type { AutopilotRunState } from '../../types/generated/AutopilotRunStateKind';
import type { CircuitAgentOwnership } from '../../types/generated/CircuitAgentOwnership';
import { ProviderIcon } from '../Providers/ProviderIcon';
import { RegenerateProviderMenu } from '../Providers/RegenerateProviderMenu';
import { InlineEditableText } from '../shared/InlineEditableText';
import { FolderOpenIcon } from '../shared/FolderOpenIcon';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import { openInFileManager } from '../../lib/tauri';
import { isMac } from '../../lib/platform';
import { AgentReviewButton } from './AgentReviewButton';
import type { ActivityStatus } from '../../lib/nodeActivities';
import { getAutopilotNodePresentation, getAutopilotRunDetails, hasActiveAutopilotOwnership, type AutopilotIndicatorTone } from '../../lib/autopilotNodePresentation';
import { AutopilotNodeIndicatorCell } from '../shared/AutopilotNodeIndicator';
import { useMuseSessionTelemetry } from '../../hooks/useMuseSessionTelemetry';
import { ObservedSessionTelemetry } from './ObservedSessionTelemetry';

interface GridNodeHeaderProps {
  /// Issue #1384 — pass the id only; the header subscribes to
  /// `state.nodesById[nodeId]` directly. Resolving the node here means
  /// the parent doesn't have to ship a fresh object reference on every
  /// fetch, and unrelated attention events on other nodes no longer
  /// re-render this header.
  nodeId: number;
  titleNodeId?: number;
  activity?: ActivityStatus;
  attentionCount?: number;
  onAttention?: () => void;
  onBuildRun: (nodeId: number, mode: 'build' | 'run' | 'terminal') => void;
  /// dnd-kit drag listeners/attributes that turn the whole title bar into the
  /// reorder/swap drag handle. Undefined when dragging is disabled (e.g. the
  /// maximized solo view, or in isolation tests).
  dragHandleProps?: Record<string, unknown>;
}

const AUTOPILOT_PILL_CLASSES: Record<AutopilotIndicatorTone, string> = {
  automation: 'bg-accent-violet/15 text-accent-violet ring-accent-violet/40',
  warning: 'bg-accent-amber/15 text-accent-amber ring-accent-amber/40',
  success: 'bg-accent-green/10 text-accent-green ring-accent-green/30',
  error: 'bg-status-error-bg text-status-error ring-status-error/40',
};

function getAutopilotPillDetails(node: AgentNode, state: AutopilotRunState) {
  const presentation = getAutopilotNodePresentation(node, state);
  const copy = getAutopilotRunDetails(state);
  return { ...copy, className: AUTOPILOT_PILL_CLASSES[presentation?.tone ?? 'automation'] };
}

function getCircuitPillDetails(node: AgentNode, ownership: CircuitAgentOwnership) {
  const presentation = getAutopilotNodePresentation(node, undefined, ownership);
  const tone: AutopilotIndicatorTone = presentation?.tone
    ?? (ownership.state === 'cancelled' ? 'warning' : 'error');
  const stateLabel = ownership.state.replace(/_/g, ' ');
  return {
    label: `${ownership.circuit_name} · #${ownership.run_id}`,
    title: `Circuit run #${ownership.run_id} (${stateLabel}): ${presentation?.detail ?? 'historical ownership retained for inspection.'}`,
    className: AUTOPILOT_PILL_CLASSES[tone],
  };
}

/** Width contracts for the compact header. Keep layout decisions named so a
 * pane resize cannot quietly grow a collection of unrelated magic numbers. */
export const HEADER_TIER_BREAKPOINTS = {
  compact: 380,
  attentionLabel: 500,
  menuWidth: 240,
} as const;

export function GridNodeHeader({ nodeId, titleNodeId = nodeId, activity, attentionCount = 0, onAttention, onBuildRun, dragHandleProps }: GridNodeHeaderProps) {
  const node = useAgentNodeStore(s => s.nodesById[nodeId]);
  const titleNode = useAgentNodeStore(s => s.nodesById[titleNodeId]);
  const renameAgentNode = useAgentNodeStore(s => s.renameAgentNode);
  const activateNode = useNodeActivityStore(s => s.activateNode);
  const deleteAgentNode = useAgentNodeStore(s => s.deleteAgentNode);
  const toggleNodePinned = useAgentNodeStore(s => s.toggleNodePinned);
  const spawnAgent = useAgentNodeStore(s => s.spawnAgent);
  const autopilotState = useAgentNodeStore(s => s.autopilotStates[nodeId]);
  const circuitOwnership = useAgentNodeStore(s => s.circuitOwnerships[nodeId]);
  const meshesById = useMeshStore(s => s.meshesById);
  const isSingleMode = useUIStore(s => s.viewMode === 'single');
  const setViewMode = useUIStore(s => s.setViewMode);
  const exitSingleMode = useUIStore(s => s.exitSingleMode);
  const openProbeTab = useUIStore(s => s.openProbeTab);
  const headerRef = useRef<HTMLDivElement>(null);
  const width = useResizeWidth(headerRef);
  const providerList = useProviderList();
  const regen = useRegenerateAction(node, providerList);
  const gitPath = node ? getNodeGitPath(node) : null;
  const { summary } = useGitSummary(gitPath);
  const { pr: openPr } = useOpenPr(nodeId, gitPath);
  const museTelemetry = useMuseSessionTelemetry(nodeId, node?.provider ?? '');
  if (!node || !titleNode) return null;

  const mesh = meshesById.get(titleNode.mesh_id);
  const meshColor = getMeshColor(titleNode.mesh_id, mesh?.color);
  const canResume = canResumeSuspendedNode(node);
  const lostConversation = hasLostConversation(node, hasActiveAutopilotOwnership(autopilotState, circuitOwnership));
  const autopilotPresentation = getAutopilotNodePresentation(node, autopilotState, circuitOwnership);
  const autopilotPill = autopilotState ? getAutopilotPillDetails(node, autopilotState) : null;
  const circuitPill = circuitOwnership ? getCircuitPillDetails(node, circuitOwnership) : null;
  const signalUnavailable = node.signal_health === 'unavailable';
  const compactHeader = width < HEADER_TIER_BREAKPOINTS.compact;
  const toggleShortcutHint = `${isMac ? '⌘' : 'Alt'}+G`;
  const handleToggleSolo = () => {
    if (isSingleMode) exitSingleMode();
    else { activateNode(node.id); setViewMode('single'); }
  };
  const handleClose = async (event: React.MouseEvent) => {
    event.stopPropagation();
    await deleteAgentNode(node.id);
  };
  const handleTogglePin = (event: React.MouseEvent) => {
    event.stopPropagation();
    void toggleNodePinned(node.id).catch(() => {});
  };
  const handleResume = (event: React.MouseEvent) => {
    event.stopPropagation();
    void spawnAgent(node.id, node.provider).catch(() => {});
  };
  const handleOpenInExplorer = async (event: React.MouseEvent) => {
    event.stopPropagation();
    if (!gitPath) return;
    try { await openInFileManager(gitPath); }
    catch (error) { console.error('Failed to open folder in file manager:', error); }
  };
  const showDetails = () => { activateNode(node.id); openProbeTab('properties'); };
  const showChanges = () => { activateNode(node.id); openProbeTab('review'); };
  const attentionTone = activity?.tone === 'error' ? 'text-status-error bg-status-error-bg' : 'text-status-warning bg-status-warning/10';

  return (
    <div {...dragHandleProps} ref={headerRef} data-testid="grid-node-header" data-node-id={node.id}
      onDoubleClick={handleToggleSolo}
      title={`Double-click or press ${toggleShortcutHint} to ${isSingleMode ? 'restore grid' : 'maximize'}`}
      className={`flex shrink-0 min-w-0 overflow-hidden items-center gap-1.5 border-b border-border-default px-2 py-1 ${dragHandleProps ? 'cursor-grab active:cursor-grabbing' : ''}`}
      style={{ backgroundColor: `${meshColor.hex}14` }}>
      <div className="flex min-w-0 flex-1 items-center gap-1.5">
        <span role="status" aria-label={activity?.label ?? getStatusConfig(node.status).label}
          title={activity?.label ?? getStatusConfig(node.status).label}
          className={`h-1.5 w-1.5 shrink-0 rounded-full ${activity?.tone === 'error' ? 'bg-status-error' : activity?.tone === 'warning' ? 'bg-status-warning' : activity?.tone === 'active' ? 'bg-accent-cyan' : getStatusConfig(titleNode.status).bgColor}`} />
        <AutopilotNodeIndicatorCell presentation={autopilotPresentation} />
        {!activity && <ProviderIcon providerId={node.provider} className="h-3.5 w-3.5 shrink-0" />}
        <span onPointerDown={event => event.stopPropagation()} onDoubleClick={event => event.stopPropagation()}
          title={titleNode.name} className="min-w-0 truncate text-sm font-semibold text-text-primary">
          <InlineEditableText value={titleNode.name} onCommit={next => renameAgentNode(titleNode.id, next)}
            className="text-sm font-semibold text-text-primary" />
        </span>
        {lostConversation && <MissingSessionIdBadge compact={compactHeader} />}
        {signalUnavailable && <SignalHealthBadge compact={compactHeader} />}
      </div>
        {attentionCount > 0 && <button type="button" onPointerDown={event => event.stopPropagation()}
        onClick={event => { event.stopPropagation(); onAttention?.(); }}
        aria-label={`${attentionCount} ${attentionCount === 1 ? 'session needs' : 'sessions need'} attention. Show next session`}
        title={`${activity?.label ?? 'Needs attention'} · Show next session`}
         className={`flex h-7 shrink-0 items-center gap-1 rounded-sm px-1.5 text-2xs font-medium ${attentionTone}`}>
        <span aria-hidden="true">!</span><span>{attentionCount}</span>{width >= HEADER_TIER_BREAKPOINTS.attentionLabel && <span>needs attention</span>}
      </button>}
      <div className="flex shrink-0 items-center gap-0.5" onPointerDown={event => event.stopPropagation()}
        onDoubleClick={event => event.stopPropagation()} onClick={event => event.stopPropagation()}>
        {openPr && <PrPill nodeId={node.id} gitPath={gitPath} openPr={openPr} compact={compactHeader} />}
        <BuildRunDropdown node={node} onBuildRun={onBuildRun} />
        <AgentReviewButton node={node} providerList={providerList} />
        {canResume && <button type="button" onClick={handleResume} aria-label="Resume agent" title="Resume agent"
          data-testid="grid-resume-button" className="flex h-7 w-7 items-center justify-center rounded-md text-accent-violet hover:bg-accent-violet/10">↻</button>}
        <KebabActions key={node.id} isPinned={node.is_pinned} onTogglePin={handleTogglePin}
          onOpenInExplorer={handleOpenInExplorer} node={node} providerList={providerList}
          isRegenerateDisabled={regen.isRegenerateDisabled} hasRegenerateTargets={regen.hasRegenerateTargets}
          onPickRegenerate={regen.pickRegenerateProvider} onDetails={showDetails} onChanges={showChanges}
          details={<>
            <div className="truncate font-medium text-text-primary" title={node.name}>{node.name}</div>
            <div className="mt-1 text-text-muted">{mesh?.name} · #{node.id} · {node.provider}</div>
            <div className="truncate text-text-muted" title={gitPath ?? undefined}>{node.use_worktree ? 'Worktree' : 'Repository root'} · {node.branch}</div>
            {circuitPill && <div data-testid="circuit-run-pill" title={circuitPill.title}
              className={`mt-1 inline-flex rounded-sm px-1.5 py-0.5 text-2xs ring-1 ${circuitPill.className}`}>{circuitPill.label}</div>}
            {!circuitOwnership && autopilotPill && <div data-testid="autopilot-pill" title={autopilotPill.title}
              className={`mt-1 inline-flex rounded-sm px-1.5 py-0.5 text-2xs ring-1 ${autopilotPill.className}`}>{autopilotPill.label}</div>}
            {summary && <div data-testid="git-summary-details" className="mt-1 text-text-muted">
              <span>{summary.total} changed files · </span>
              <span className={summary.added ? 'text-accent-green' : 'text-text-muted'}>+{summary.added}</span>{' '}
              <span className={summary.modified ? 'text-accent-amber' : 'text-text-muted'}>~{summary.modified}</span>{' '}
              <span className={summary.deleted ? 'text-accent-red' : 'text-text-muted'}>-{summary.deleted}</span>
            </div>}
            {museTelemetry && <ObservedSessionTelemetry telemetry={museTelemetry} />}
          </>} />
        <button type="button" onClick={handleToggleSolo} aria-label={isSingleMode ? 'Restore grid layout' : 'Maximize agent node'}
          title={`${isSingleMode ? 'Restore grid' : 'Maximize'} (${toggleShortcutHint})`}
          className="flex h-7 w-7 items-center justify-center rounded-md text-text-muted hover:bg-bg-base hover:text-text-primary">
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d={isSingleMode ? 'M9 3v6H3m12 12v-6h6M9 9 3 3m12 12 6 6' : 'M15 3h6v6m0-6-7 7M9 21H3v-6m0 6 7-7'} />
          </svg>
        </button>
        <button type="button" onClick={handleClose} aria-label="Close agent node" title="Close agent node"
          className="flex h-7 w-7 items-center justify-center rounded-md text-text-muted hover:bg-status-error-bg hover:text-status-error transition-colors">
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M18 6 6 18" />
            <path d="m6 6 12 12" />
          </svg>
        </button>
      </div>
      {regen.pendingRegenerate && <ConfirmDialog title="Regenerate this node?"
        message={`Agent is currently working. Regenerate with ${regen.pendingRegenerate.providerLabel}?`}
        confirmLabel="Regenerate" onConfirm={regen.confirmRegenerate} onCancel={regen.cancelRegenerate} />}
    </div>
  );
}

/**
 * Compact kebab menu for the right-side header actions. Single-use
 * component — the agent-node header is the only call site, so we
 * inline it here instead of elevating a generic KebabMenu primitive to
 * `src/components/shared/`. If a second consumer ever wants the same
 * shape, lift it then; pattern-match first.
 *
 * Closes on Escape, outside click, or item activation. Reuses the same
 * WAI-ARIA conventions the sidebar's MeshItem menu applies (issue #735):
 * role=menu / role=menuitem / aria-labelledby on the trigger, focus
 * returns to the trigger on close.
 *
 * Why `fixed` positioning rather than `absolute`? The kebab lives
 * inside `<NodeCard>` whose `overflow-hidden` would clip a popover
 * that escapes the card (e.g. when the menu needs to drop *below*
 * the bottom edge of the row). Fixed coordinates are viewport-scoped
 * and unaffected by ancestor overflow. Anchor to the trigger by
 * snapshotting its `getBoundingClientRect` on the toggle click.
 *
 * Issue #1291 — the menu is portaled to `document.body` for the same
 * reason the sidebar's NodeItem/MeshItem menus were in #1290: the
 * GridNodeHeader row nests inside a flex container that can carry a
 * CSS `filter` (the inactive-row brightness hover state on adjacent
 * rows) and inside the GridSplitter, both of which become containing
 * blocks for `position:fixed`. Portaling keeps `top`/`left` anchored
 * to the viewport so the menu renders where the trigger rect says it
 * should, even when the row above is hovered. The `useAnchoredPosition`
 * math above stays the same — viewport pixels are still the right
 * unit because the menu now lives at body level.
 */
interface KebabActionsProps {
  isPinned: boolean;
  onTogglePin: (e: React.MouseEvent) => void;
  onOpenInExplorer: (e: React.MouseEvent) => void;
  node: Pick<AgentNode, 'provider'>;
  details: React.ReactNode;
  onDetails: () => void;
  onChanges: () => void;
  providerList: SpawnOption[];
  isRegenerateDisabled: boolean;
  hasRegenerateTargets: boolean;
  onPickRegenerate: (providerId: string, providerLabel: string) => void;
}

const KEBAB_MIN_WIDTH = 160;

function KebabActions({ isPinned, onTogglePin, onOpenInExplorer, node, providerList, isRegenerateDisabled, hasRegenerateTargets, onPickRegenerate, details, onDetails, onChanges }: KebabActionsProps) {
  const [open, setOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  // Issue #1502 — Regenerate picker submenu via the shared `useSubmenu`
  // hook (same hook drives the sidebar `NodeItem` picker): hover/click
  // opens, ArrowRight opens-and-focuses, ArrowLeft closes, ArrowDown/Up
  // wraps. No local submenu state, refs, or modulo loops.
  const regenSubmenu = useSubmenu({
    disabled: isRegenerateDisabled,
    itemCount: (providerList ?? []).length,
  });
  const regenDisabled = isRegenerateDisabled || !hasRegenerateTargets;
  // Stable id linking the trigger to the menu for the WAI-ARIA
  // disclosure pattern (aria-controls). Each header instance owns one
  // kebab, so a module-scoped counter is enough.
  const menuIdRef = useRef(`grid-node-kebab-menu-${Math.random().toString(36).slice(2, 9)}`);
  const menuId = menuIdRef.current;
  const closeAndReturnFocus = () => {
    const trigger = triggerRef.current;
    regenSubmenu.closeSubmenu();
    setOpen(false);
    requestAnimationFrame(() => trigger?.focus());
  };

  const handleToggle = (e: React.MouseEvent) => {
    e.stopPropagation();
    regenSubmenu.closeSubmenu();
    setOpen((o) => !o);
  };

  const handleRegenPick = (providerId: string, providerLabel: string) => {
    regenSubmenu.closeSubmenu();
    closeAndReturnFocus();
    onPickRegenerate(providerId, providerLabel);
  };

  // Issue #814 — converged on the shared `useClickOutside` hook
  // (#492) for the outside-mousedown close path. The hook scopes by
  // `[data-dropdown-for="<menuId>"]` and `menuId` is per-instance
  // (one kebab per agent node), so two open kebabs on different
  // nodes don't interfere. Place `data-dropdown-for={menuId}` on the
  // menu root AND the regenerate submenu so clicks inside either
  // subtree count as "inside" (mirrors `NodeItem`'s parent+submenu
  // scoping, issue #814).
  useClickOutside<string>(open ? menuId : null, () => {
    regenSubmenu.closeSubmenu();
    setOpen(false);
  });

  useAriaMenu({
    rootRef: menuRef,
    activeIndex,
    setActiveIndex,
    onClose: closeAndReturnFocus,
    enabled: open,
    itemSelector: '[data-aria-menu-item]',
    skipDisabled: true,
  });

  // The shared menu hook owns Escape, Tab, and roving Arrow/Home/End focus.
  // This listener only handles the regenerate submenu's horizontal arrows.
  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      const menu = menuRef.current;
      const active = document.activeElement;
      const inMenu = menu && active instanceof Node && menu.contains(active);
      const inSubmenu = regenSubmenu.submenuContainsFocus();
      if (!inMenu && !inSubmenu) return;
      if (e.key === 'ArrowRight' && inMenu && !inSubmenu) {
        if (active?.getAttribute('aria-haspopup') !== 'menu') return;
        e.preventDefault();
        regenSubmenu.openSubmenuViaKeyboard();
        return;
      }
      if (e.key === 'ArrowLeft' && (inSubmenu || (inMenu && regenSubmenu.isSubmenuOpen()))) {
        e.preventDefault();
        regenSubmenu.closeSubmenu();
        const trigger = menu?.querySelector<HTMLButtonElement>('button[aria-haspopup="menu"]');
        if (trigger) focusWithoutScroll(trigger);
        return;
      }
      if (inSubmenu && e.key === 'ArrowDown') { e.preventDefault(); regenSubmenu.stepSubmenuFocus(1); return; }
      if (inSubmenu && e.key === 'ArrowUp') { e.preventDefault(); regenSubmenu.stepSubmenuFocus(-1); return; }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [open, regenSubmenu]);

  // Fixed-menu anchoring, viewport clamping, and scroll tracking are shared
  // with the PR pill so both title-bar menus follow the same positioning rules.
  useAnchoredPosition(triggerRef, popoverRef, open, { align: 'end' });


  return (
    <>
      <button
        ref={triggerRef}
        data-dropdown-for={menuId}
        type="button"
        onClick={handleToggle}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        aria-label="Agent node actions"
        title="Agent node actions"
        className="w-7 h-7 flex items-center justify-center rounded-md text-text-muted hover:text-text-primary hover:bg-bg-base transition-[color,background-color]"
      >
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <circle cx="12" cy="5" r="1.2" fill="currentColor" />
          <circle cx="12" cy="12" r="1.2" fill="currentColor" />
          <circle cx="12" cy="19" r="1.2" fill="currentColor" />
        </svg>
      </button>
      {open && createPortal(
        <div
          ref={popoverRef}
          id={menuId}
          // Issue #814 — scoped attribute for `useClickOutside`. The
          // menu's per-instance `menuId` ensures sibling kebabs (one per
          // agent node in the grid) don't share the selector.
          data-dropdown-for={menuId}
          className="fixed bg-bg-overlay border border-border-default rounded-md shadow-md animate-scale-in origin-top-right z-[100] py-1"
          style={{ top: 0, left: 0, minWidth: KEBAB_MIN_WIDTH, width: HEADER_TIER_BREAKPOINTS.menuWidth, maxWidth: 'calc(100vw - 16px)' }}
        >
          <div data-testid="grid-node-details" className="mb-1 border-b border-border-subtle px-3 py-2 text-xs">{details}</div>
          <div ref={menuRef} role="menu" aria-label="Agent node actions">
          {/* Issue #1502 — Regenerate row (first, mirrors the sidebar
              context-menu order). Hover or ArrowRight/click opens the
              provider picker submenu pinned with `Current (<label>)` on
              top for in-place kick-start. The submenu opens to the LEFT
              (`right-full`) so it does not cover Maximize/Close on the
              trailing edge. Same `data-dropdown-for` scoping as the parent so
              `useClickOutside` treats both as "inside". */}
          <div
            role="presentation"
            className="relative"
            onMouseEnter={() => {
              if (!regenDisabled) regenSubmenu.setSubmenuOpen(true);
            }}
            onMouseLeave={() => regenSubmenu.closeSubmenu()}
          >
            <button
              role="menuitem" data-aria-menu-item
              aria-haspopup="menu"
              aria-expanded={regenSubmenu.submenuOpen}
              disabled={regenDisabled}
              onClick={() => {
                if (regenDisabled) return;
                regenSubmenu.setSubmenuOpen(true);
              }}
              title={
                isRegenerateDisabled
                  ? 'Regenerate unavailable while node is in this state'
                  : !hasRegenerateTargets
                    ? 'No providers are available on this mesh'
                    : 'Pick a Model Provider for this node (including current to kick-start)'
              }
              data-testid="grid-regenerate-trigger"
              className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card-hover flex items-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:bg-transparent"
            >
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8" />
                <path d="M21 3v5h-5" />
                <path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16" />
                <path d="M3 21v-5h5" />
              </svg>
              Regenerate
              <span aria-hidden="true" className="ml-auto">▸</span>
            </button>
            {regenSubmenu.submenuOpen && (
              <div
                ref={regenSubmenu.submenuRef}
                role="menu"
                aria-label="Pick target provider"
                data-testid="grid-regenerate-submenu"
                data-dropdown-for={menuId}
                className="absolute right-full top-0 mr-1 min-w-[200px] bg-bg-overlay border border-border-default rounded-md shadow-md py-1 z-[101]"
                onMouseDown={(e) => e.stopPropagation()}
              >
                <RegenerateProviderMenu
                  providers={providerList}
                  currentProviderId={node.provider}
                  onPick={handleRegenPick}
                  submenuTestId="grid-regenerate-submenu"
                />
              </div>
            )}
          </div>
          <button
            role="menuitem" data-aria-menu-item
            onClick={(e) => { closeAndReturnFocus(); onOpenInExplorer(e); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card-hover flex items-center gap-2"
          >
            <FolderOpenIcon className="w-3 h-3" />
            Open in file explorer
          </button>
          <button
            role="menuitem" data-aria-menu-item
            aria-pressed={isPinned}
            onClick={(e) => { closeAndReturnFocus(); onTogglePin(e); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card-hover flex items-center gap-2"
          >
            <svg width="12" height="12" viewBox="0 0 24 24" fill={isPinned ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <path d="M12 17v5" fill="none" />
              <path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z" />
            </svg>
            {isPinned ? 'Unpin node' : 'Pin node'}
          </button>
          <button type="button" role="menuitem" data-aria-menu-item
            onClick={() => { closeAndReturnFocus(); onDetails(); }}
            className="w-full border-t border-border-subtle px-3 py-1.5 text-left text-xs text-text-secondary hover:bg-bg-card-hover">Agent node details</button>
          <button type="button" role="menuitem" data-aria-menu-item
            onClick={() => { closeAndReturnFocus(); onChanges(); }}
            className="w-full px-3 py-1.5 text-left text-xs text-text-secondary hover:bg-bg-card-hover">View changes</button>
          </div>
        </div>,
        document.body,
      )}
    </>
  );
}
