import { useRef, useState, useEffect, useLayoutEffect } from 'react';
import { createPortal } from 'react-dom';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
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
import { useAnchoredPosition } from '../../hooks/useAnchoredPosition';
import { getNodeGitPath } from '../../lib/paths';
import { getStatusConfig } from '../../lib/status';
import { canResumeSuspendedNode, hasLostConversation } from '../../lib/suspended';
import { MissingSessionIdBadge } from '../shared/MissingSessionIdBadge';
import type { SpawnOption } from '../../lib/groups';
import { getMeshColor } from '../../lib/meshColors';
import type { AutopilotRunState } from '../../types/generated/AutopilotRunStateKind';
import { ProviderIcon } from '../Providers/ProviderIcon';
import { RegenerateProviderMenu } from '../Providers/RegenerateProviderMenu';
import { InlineEditableText } from '../shared/InlineEditableText';
import { FolderOpenIcon } from '../shared/FolderOpenIcon';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import { openInFileManager } from '../../lib/tauri';
import { isMac } from '../../lib/platform';
import { AgentReviewButton } from './AgentReviewButton';
import type { ActivityStatus } from '../../lib/nodeActivities';

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

// Autopilot pill styling per pipeline state (the typed
// `AutopilotRunState` union — wires in via ts-rs from the Rust enum).
// Violet is the "automation owns this node" accent; the wrap-up / terminal
// states reuse the amber / green / red semantics the rest of the header
// already speaks.
const AUTOPILOT_PILL_STYLES: Record<AutopilotRunState, { label: string; className: string; title: string }> = {
  implementing: {
    label: 'autopilot',
    className: 'bg-accent-violet/15 text-accent-violet ring-accent-violet/40',
    title: 'Autopilot: agent is implementing the task',
  },
  finishing: {
    label: 'autopilot · wrap-up',
    className: 'bg-accent-amber/15 text-accent-amber ring-accent-amber/40',
    title: 'Autopilot: wrap-up in progress (verify, commit, push, PR)',
  },
  // Issue #993: a loop iteration's deterministic wrap-up passed and the
  // optional `loop_suffix_prompt` is now driving a second PTY turn on the
  // same node. The node is still active (it holds the mesh capacity slot
  // and the iteration row) until that suffix turn yields and the run
  // reaches `completed`. Renders with the same amber treatment as
  // `finishing` so the user can tell this is still in flight, not a wrap-
  // up verification failure.
  suffix_pending: {
    label: 'autopilot · suffix',
    className: 'bg-accent-amber/15 text-accent-amber ring-accent-amber/40',
    title: 'Autopilot: wrap-up verified, running the optional suffix prompt turn before this iteration completes',
  },
  completed: {
    label: 'autopilot · complete',
    className: 'bg-accent-green/10 text-accent-green ring-accent-green/30',
    title: 'Autopilot: wrap-up verified, hands off — the PR is ready for review; the node closes when it merges',
  },
  merged: {
    label: 'autopilot · merged',
    className: 'bg-accent-green/10 text-accent-green ring-accent-green/30',
    title: 'Autopilot: PR merged',
  },
  failed: {
    label: 'autopilot ✗',
    className: 'bg-status-error-bg text-status-error ring-status-error/40',
    title: 'Autopilot: wrap-up failed after 3 attempts — needs a human',
  },
};

export function GridNodeHeader({ nodeId, titleNodeId = nodeId, activity, attentionCount = 0, onAttention, onBuildRun, dragHandleProps }: GridNodeHeaderProps) {
  const node = useAgentNodeStore(s => s.nodesById[nodeId]);
  const titleNode = useAgentNodeStore(s => s.nodesById[titleNodeId]);
  const renameAgentNode = useAgentNodeStore(s => s.renameAgentNode);
  const setActiveNode = useAgentNodeStore(s => s.setActiveNode);
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
  if (!node || !titleNode) return null;

  const mesh = meshesById.get(titleNode.mesh_id);
  const meshColor = getMeshColor(titleNode.mesh_id, mesh?.color);
  const canResume = canResumeSuspendedNode(node);
  const toggleShortcutHint = `${isMac ? '⌘' : 'Alt'}+G`;
  const handleToggleSolo = () => {
    if (isSingleMode) exitSingleMode();
    else { setActiveNode(node.id); setViewMode('single'); }
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
  const showDetails = () => { setActiveNode(node.id); openProbeTab('properties'); };
  const showChanges = () => { setActiveNode(node.id); openProbeTab('review'); };
  const attentionTone = activity?.tone === 'error' ? 'text-status-error bg-status-error-bg' : 'text-status-warning bg-status-warning/10';

  return (
    <div {...dragHandleProps} ref={headerRef} data-testid="grid-node-header"
      onDoubleClick={handleToggleSolo}
      title={`Double-click or press ${toggleShortcutHint} to ${isSingleMode ? 'restore grid' : 'maximize'}`}
      className={`flex shrink-0 min-w-0 items-center gap-1.5 border-b border-border-default px-2 py-1 ${dragHandleProps ? 'cursor-grab active:cursor-grabbing' : ''}`}
      style={{ backgroundColor: `${meshColor.hex}14` }}>
      <div className="flex min-w-0 flex-1 items-center gap-1.5">
        <span role="status" aria-label={activity?.label ?? getStatusConfig(node.status).label}
          title={activity?.label ?? getStatusConfig(node.status).label}
          className={`h-1.5 w-1.5 shrink-0 rounded-full ${activity?.tone === 'error' ? 'bg-status-error' : activity?.tone === 'warning' ? 'bg-status-warning' : activity?.tone === 'active' ? 'bg-accent-cyan' : getStatusConfig(titleNode.status).bgColor}`} />
        {!activity && <ProviderIcon providerId={node.provider} className="h-3.5 w-3.5 shrink-0" />}
        <span onPointerDown={event => event.stopPropagation()} onDoubleClick={event => event.stopPropagation()}
          title={titleNode.name} className="min-w-0 truncate text-sm font-semibold text-text-primary">
          <InlineEditableText value={titleNode.name} onCommit={next => renameAgentNode(titleNode.id, next)}
            className="text-sm font-semibold text-text-primary" />
        </span>
        {hasLostConversation(node, !!autopilotState || !!circuitOwnership) && <MissingSessionIdBadge compact={width < 380} />}
        {node.signal_health === 'unavailable' && <span role="img" aria-label="Attention signal unavailable"
          title="Attention signal unavailable — watch this session's terminal directly."
          className="shrink-0 text-xs text-status-warning">⚠</span>}
      </div>
      {attentionCount > 0 && <button type="button" onPointerDown={event => event.stopPropagation()}
        onClick={event => { event.stopPropagation(); onAttention?.(); }}
        aria-label={`${attentionCount} ${attentionCount === 1 ? 'session needs' : 'sessions need'} attention. Show next session`}
        title={`${activity?.label ?? 'Needs attention'} · Show next session`}
        className={`flex h-7 shrink-0 items-center gap-1 rounded-sm px-1.5 text-2xs font-medium ${attentionTone}`}>
        <span aria-hidden="true">!</span><span>{attentionCount}</span>{width >= 500 && <span>needs attention</span>}
      </button>}
      <div className="flex shrink-0 items-center gap-0.5" onPointerDown={event => event.stopPropagation()}
        onDoubleClick={event => event.stopPropagation()} onClick={event => event.stopPropagation()}>
        {width >= 640 && openPr && <PrPill nodeId={node.id} gitPath={gitPath} openPr={openPr} />}
        <BuildRunDropdown node={node} onBuildRun={onBuildRun} />
        <AgentReviewButton node={node} />
        {canResume && <button type="button" onClick={handleResume} aria-label="Resume agent" title="Resume agent"
          data-testid="grid-resume-button" className="flex h-7 w-7 items-center justify-center rounded-md text-accent-violet hover:bg-accent-violet/10">↻</button>}
        <button type="button" onClick={handleToggleSolo} aria-label={isSingleMode ? 'Restore grid layout' : 'Maximize agent node'}
          title={`${isSingleMode ? 'Restore grid' : 'Maximize'} (${toggleShortcutHint})`}
          className="flex h-7 w-7 items-center justify-center rounded-md text-text-muted hover:bg-bg-base hover:text-text-primary">
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d={isSingleMode ? 'M9 3v6H3m12 12v-6h6M9 9 3 3m12 12 6 6' : 'M15 3h6v6m0-6-7 7M9 21H3v-6m0 6 7-7'} />
          </svg>
        </button>
        <KebabActions key={node.id} isSingleMode={isSingleMode} isPinned={node.is_pinned} toggleShortcutHint={toggleShortcutHint}
          onToggleSolo={event => { event.stopPropagation(); handleToggleSolo(); }} onTogglePin={handleTogglePin}
          onClose={handleClose} onOpenInExplorer={handleOpenInExplorer} canResume={canResume} onResume={handleResume}
          node={node} providerList={providerList} isRegenerateDisabled={regen.isRegenerateDisabled}
          hasRegenerateTargets={regen.hasRegenerateTargets} onPickRegenerate={regen.pickRegenerateProvider}
          onDetails={showDetails} onChanges={showChanges}
          details={<>
            <div className="truncate font-medium text-text-primary" title={node.name}>{node.name}</div>
            <div className="mt-1 text-text-muted">{mesh?.name} · #{node.id} · {node.provider}</div>
            <div className="truncate text-text-muted" title={gitPath ?? undefined}>{node.use_worktree ? 'Worktree' : 'Repository root'} · {node.branch}</div>
            {circuitOwnership && <div data-testid="circuit-run-pill" className="mt-1 text-accent-violet">{circuitOwnership.circuit_name} · #{circuitOwnership.run_id}</div>}
            {!circuitOwnership && autopilotState && <div data-testid="autopilot-pill" title={AUTOPILOT_PILL_STYLES[autopilotState].title}
              className="mt-1 text-accent-violet">{AUTOPILOT_PILL_STYLES[autopilotState].label}</div>}
            {summary && <div className="mt-1 text-text-muted">{summary.total} changed files · +{summary.added} ~{summary.modified} -{summary.deleted}</div>}
          </>} />
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
  isSingleMode: boolean;
  isPinned: boolean;
  toggleShortcutHint: string;
  onToggleSolo: (e: React.MouseEvent) => void;
  onTogglePin: (e: React.MouseEvent) => void;
  onClose: (e: React.MouseEvent) => void;
  onOpenInExplorer: (e: React.MouseEvent) => void;
  canResume: boolean;
  onResume: (e: React.MouseEvent) => void;
  node: Pick<AgentNode, 'id' | 'name' | 'provider' | 'status'>;
  details: React.ReactNode;
  onDetails: () => void;
  onChanges: () => void;
  providerList: SpawnOption[];
  isRegenerateDisabled: boolean;
  hasRegenerateTargets: boolean;
  onPickRegenerate: (providerId: string, providerLabel: string) => void;
}

const KEBAB_MIN_WIDTH = 160;

function KebabActions({ isSingleMode, isPinned, toggleShortcutHint, onToggleSolo, onTogglePin, onClose, onOpenInExplorer, canResume, onResume, node, providerList, isRegenerateDisabled, hasRegenerateTargets, onPickRegenerate, details, onDetails, onChanges }: KebabActionsProps) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const menuItemRefs = useRef<(HTMLButtonElement | null)[]>([]);
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
  // Resume adds one row; navigation wraps over the current set of commands.
  const itemCount = (canResume ? 8 : 7);
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

  // Outside click + Escape close. Same `document`-vs-`window` jsdom
  // caveat as `MeshItem` (issue #735): tests dispatch on `document`,
  // not `window`, because in jsdom the two are independent targets.
  // Submenu arrows delegate to the shared `useSubmenu` hook (same as
  // `NodeItem`); only the parent-menu traversal below is kebab-local.
  // The listener attaches once per open: submenu state lives in the
  // hook behind stable callbacks, so hovering the picker never churns
  // this subscription.
  useEffect(() => {
    if (!open) return;
    // Issue #1502 — skip disabled rows (the Regenerate trigger is
    // disabled when the mesh offers no providers). Focusing a disabled
    // button is a no-op, so a naive modulo walk would stall on the
    // disabled slot and break Arrow-wrap.
    const focusSibling = (currentIdx: number, dir: 1 | -1) => {
      for (let step = 1; step <= itemCount; step++) {
        const next = (currentIdx + dir * step + itemCount) % itemCount;
        const el = menuItemRefs.current[next];
        if (el && !el.hasAttribute('disabled')) {
          el.focus();
          return;
        }
      }
    };
    const onKeyDown = (e: KeyboardEvent) => {
      const menu = menuRef.current;
      const active = document.activeElement;
      const inMenu = menu && active instanceof Node && menu.contains(active);
      const inSubmenu = regenSubmenu.submenuContainsFocus();
      if (!inMenu && !inSubmenu) return;
      if (e.key === 'Escape') {
        e.preventDefault();
        closeAndReturnFocus();
        return;
      }
      if (e.key === 'Tab') {
        // Non-modal popover (matches MeshItem, #735): Tab leaves the
        // menu and closes it; the browser moves focus to the next
        // tabbable element naturally.
        regenSubmenu.closeSubmenu();
        setOpen(false);
        return;
      }
      if (e.key === 'ArrowRight' && inMenu && !inSubmenu) {
        if (active?.getAttribute('aria-haspopup') !== 'menu') return;
        e.preventDefault();
        regenSubmenu.openSubmenuViaKeyboard();
        return;
      }
      if (
        e.key === 'ArrowLeft' &&
        (inSubmenu || (inMenu && regenSubmenu.isSubmenuOpen()))
      ) {
        e.preventDefault();
        regenSubmenu.closeSubmenu();
        const trigger = menu?.querySelector<HTMLButtonElement>('button[aria-haspopup="menu"]');
        if (trigger) focusWithoutScroll(trigger);
        return;
      }
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        if (inSubmenu) {
          regenSubmenu.stepSubmenuFocus(1);
        } else {
          const items = menuItemRefs.current;
          const activeIdx = items.findIndex((el) => el === document.activeElement);
          if (activeIdx >= 0) focusSibling(activeIdx, 1);
        }
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        if (inSubmenu) {
          regenSubmenu.stepSubmenuFocus(-1);
        } else {
          const items = menuItemRefs.current;
          const activeIdx = items.findIndex((el) => el === document.activeElement);
          if (activeIdx >= 0) focusSibling(activeIdx, -1);
        }
        return;
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
    };
    // `itemCount` is constant while open (`canResume` can't flip
    // mid-menu); everything submenu-shaped comes from the hook's stable
    // callbacks — hence no `regenSubmenuOpen` dep and no listener churn
    // on hover.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, itemCount]);

  // Fixed-menu anchoring, viewport clamping, and scroll tracking are shared
  // with the PR pill so both title-bar menus follow the same positioning rules.
  useAnchoredPosition(triggerRef, menuRef, open, { align: 'end' });

  // Autofocus the first ENABLED menuitem on open (WAI-ARIA menu contract).
  // Issue #1502 — the first row is now Regenerate, which is disabled when
  // the mesh offers no providers (the common case in tests, where
  // `listProviders` isn't mocked and resolves to `[]`). Focusing a
  // disabled button is a no-op in browsers/jsdom, which would leave focus
  // outside the menu and break the Escape-to-close contract (the key
  // handler gates on focus-inside-menu). Skip disabled rows so Escape
  // always has a focused item to gate on.
  // Issue #1291 — `preventScroll: true` keeps Chromium from auto-scrolling
  // a flex/scroll ancestor to bring the focused item into view. The kebab
  // lives inside the GridNodeHeader row inside the splitter; without the
  // flag, opening the menu would visibly nudge the grid. Local-only —
  // `useSubmenu`'s `focusWithoutScroll` and the inline `useAriaMenu` both
  // have their own scroll behaviour (e.g. ProviderDropdown relies on the
  // scroll-into-view side effect), so we DON'T route this through them.
  useLayoutEffect(() => {
    if (!open) return;
    const firstEnabled = menuItemRefs.current.find((el) => el && !el.hasAttribute('disabled'));
    firstEnabled?.focus({ preventScroll: true });
  }, [open]);

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
          ref={menuRef}
          id={menuId}
          // Issue #814 — scoped attribute for `useClickOutside`. The
          // menu's per-instance `menuId` ensures sibling kebabs (one per
          // agent node in the grid) don't share the selector.
          data-dropdown-for={menuId}
          role="menu"
          aria-label="Agent node actions"
          className="fixed bg-bg-overlay border border-border-default rounded-md shadow-md animate-scale-in origin-top-right z-[100] py-1"
          style={{ top: 0, left: 0, minWidth: KEBAB_MIN_WIDTH, width: 240, maxWidth: 'calc(100vw - 16px)' }}
        >
          <div role="presentation" className="mb-1 border-b border-border-subtle px-3 py-2 text-xs">{details}</div>
          {/* Issue #1502 — Regenerate row (first, mirrors the sidebar
              context-menu order). Hover or ArrowRight/click opens the
              provider picker submenu pinned with `Current (<label>)` on
              top for in-place kick-start. The submenu opens to the LEFT
              (`right-full`) because the kebab itself hugs the header's
              right edge — opening to the right would overflow the
              viewport. Same `data-dropdown-for` scoping as the parent so
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
              ref={(el) => { menuItemRefs.current[0] = el; }}
              role="menuitem"
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
              className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:bg-transparent"
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
            ref={(el) => { menuItemRefs.current[1] = el; }}
            role="menuitem"
            onClick={(e) => { closeAndReturnFocus(); onOpenInExplorer(e); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <FolderOpenIcon className="w-3 h-3" />
            Open in file explorer
          </button>
          <button
            ref={(el) => { menuItemRefs.current[2] = el; }}
            role="menuitem"
            aria-pressed={isPinned}
            onClick={(e) => { closeAndReturnFocus(); onTogglePin(e); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <svg width="12" height="12" viewBox="0 0 24 24" fill={isPinned ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <path d="M12 17v5" fill="none" />
              <path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z" />
            </svg>
            {isPinned ? 'Unpin node' : 'Pin node'}
          </button>
          <button
            ref={(el) => { menuItemRefs.current[3] = el; }}
            role="menuitem"
            onClick={(e) => { closeAndReturnFocus(); onToggleSolo(e); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              {isSingleMode ? (
                <path d="M9 9H4m0 0V4m0 5 6-6m5 16v-5m0 0h5m-5 0 6 6M9 15H4m0 0v5m0-5 6 6m5-16V4m0 0h5m-5 0 6 6" />
              ) : (
                <path d="M15 3h6m0 0v6m0-6-7 7M9 21H3m0 0v-6m0 6 7-7" />
              )}
            </svg>
            {isSingleMode ? `Restore grid (${toggleShortcutHint})` : `Maximize (${toggleShortcutHint})`}
          </button>
          <button
            ref={(el) => { menuItemRefs.current[4] = el; }}
            role="menuitem"
            aria-label={`Close session · ${node.name}`}
            onClick={(e) => { closeAndReturnFocus(); onClose(e); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <span className="text-text-muted" aria-hidden="true">×</span>
            Close session
          </button>
          {canResume && (
            <button
              ref={(el) => { menuItemRefs.current[5] = el; }}
              role="menuitem"
              onClick={(e) => { closeAndReturnFocus(); onResume(e); }}
              data-testid="grid-resume-button"
              className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
            >
              <span className="text-accent-violet" aria-hidden="true">↻</span>
              Resume agent
            </button>
          )}
          <button type="button" role="menuitem" ref={el => { menuItemRefs.current[canResume ? 6 : 5] = el; }}
            onClick={() => { closeAndReturnFocus(); onDetails(); }}
            className="w-full border-t border-border-subtle px-3 py-1.5 text-left text-xs text-text-secondary hover:bg-bg-card">Session details</button>
          <button type="button" role="menuitem" ref={el => { menuItemRefs.current[canResume ? 7 : 6] = el; }}
            onClick={() => { closeAndReturnFocus(); onChanges(); }}
            className="w-full px-3 py-1.5 text-left text-xs text-text-secondary hover:bg-bg-card">View changes</button>
        </div>,
        document.body,
      )}
    </>
  );
}
