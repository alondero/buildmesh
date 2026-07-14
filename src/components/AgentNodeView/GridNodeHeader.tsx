import { useMemo, useRef, useState, useEffect, useLayoutEffect } from 'react';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { BuildRunDropdown } from '../BuildRun/BuildRunDropdown';
import { useGitSummary } from '../../hooks/useGitSummary';
import { useOpenPr } from '../../hooks/useOpenPr';
import { useResizeWidth } from '../../hooks/useResizeWidth';
import { getNodeGitPath } from '../../lib/paths';
import { getStatusConfig } from '../../lib/status';
import { getMeshColor } from '../../lib/meshColors';
import { ProviderIcon } from '../Providers/ProviderIcon';
import { InlineEditableText } from '../shared/InlineEditableText';
import { FolderOpenIcon } from '../shared/FolderOpenIcon';
import { openUrl } from '@tauri-apps/plugin-opener';
import { openInFileManager } from '../../lib/tauri';
import { isMac } from '../../lib/platform';

interface GridNodeHeaderProps {
  node: AgentNode;
  onBuildRun: (nodeId: number, mode: 'build' | 'run' | 'terminal') => void;
  /// dnd-kit drag listeners/attributes that turn the whole title bar into the
  /// reorder/swap drag handle. Undefined when dragging is disabled (e.g. the
  /// maximized solo view, or in isolation tests).
  dragHandleProps?: Record<string, unknown>;
}

// Responsive tier breakpoints (issue #736). Ordered highest → lowest so the
// first match wins; the `$` last clause keeps the type narrow in JS so
// downstream switches don't need a default branch.
//
// Why these specific values rather than the Tailwind v4 container defaults
// (`xs 320 / sm 384 / md 448 / lg 512 / xl 576`)? The agent-node header sits
// inside a pane set by the GridSplitter / ResizablePanes and can run as narrow
// as ~150 px. A 320 px floor would force every node in a 2-pane layout to start
// in `compact`, so the breakpoints step down to match the actual minimum
// layout (Build ▼ + kebab fits in ~155 px) and stagger in chips once per
// hierarchy level.
export type HeaderTier = 'xl' | 'wide' | 'medium' | 'slim' | 'compact';

export const HEADER_TIER_BREAKPOINTS: Record<Exclude<HeaderTier, 'compact'>, number> = {
  xl: 640,    // diff summary joins the row
  wide: 500,  // PR chip joins the row
  medium: 380, // worktree/root pill joins the row; inline close+max replace kebab
  slim: 280,  // mesh label joins the row; kebab stays
};
// `compact` is the implicit floor (`< HEADER_TIER_BREAKPOINTS.slim`).

export function getHeaderTier(width: number): HeaderTier {
  if (width >= HEADER_TIER_BREAKPOINTS.xl) return 'xl';
  if (width >= HEADER_TIER_BREAKPOINTS.wide) return 'wide';
  if (width >= HEADER_TIER_BREAKPOINTS.medium) return 'medium';
  if (width >= HEADER_TIER_BREAKPOINTS.slim) return 'slim';
  return 'compact';
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
  const meshColor = getMeshColor(node.mesh_id, meshesById.get(node.mesh_id)?.color);

  // Issue #736 — measure the rendered header width and bucket it into a tier
  // that decides which chips render and whether the close/max buttons live
  // inline or inside a kebab menu. The hook starts at `Infinity` so the
  // initial render shows everything until the ResizeObserver reports; on
  // shrunken panes the worst case is one flash of "wide" before the tier
  // drops on the next frame.
  const headerRef = useRef<HTMLDivElement>(null);
  const width = useResizeWidth(headerRef);
  const tier = getHeaderTier(width);
  // Convenience booleans for readability — avoids noisy `tier === 'xl' || tier === 'wide'`
  // chains at every chip.
  const showSummary = tier === 'xl';
  const showPr = tier === 'xl' || tier === 'wide';
  const showWorktree = tier === 'xl' || tier === 'wide' || tier === 'medium';
  const showMeshLabel = tier !== 'compact';
  // The worktree pill and the inline close+max buttons appear at the same
  // tiers because both compete for the same horizontal real estate — any
  // future tier shift on one should land on the other. Aliasing here makes
  // that coupling explicit instead of inviting two booleans to drift.
  const showInlineActions = showWorktree;

  const handleClose = async (e: React.MouseEvent) => {
    e.stopPropagation();
    await deleteAgentNode(node.id);
  };

  const gitPath = getNodeGitPath(node);

  // Silent on failure — worktree rows can go stale between renders and a
  // toast storm on every click is worse UX than a quiet console line.
  // Same precedent as `WorktreeManagerTab.openInExplorer`.
  const handleOpenInExplorer = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (!gitPath) return;
    try {
      await openInFileManager(gitPath);
    } catch (err) {
      console.error('Failed to open folder in file manager:', err);
    }
  };
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
      // Issue #736 — `data-tier` is the test seam: jsdom can't observe the
      // actual visibility of a container-query CSS rule, but the JS state
      // driving the render branches sets `data-tier` on the root, so tests
      // can pin which chip is present at each breakpoint by reading the
      // attribute. Production can also use it as a CSS hook if needed.
      data-tier={tier}
      data-testid="grid-node-header"
      ref={headerRef}
      className={`flex items-center justify-between px-2.5 py-1.5 border-b border-border-default gap-2 ${dragHandleProps ? 'cursor-grab active:cursor-grabbing' : ''}`}
      style={{ backgroundColor: `${meshColor.hex}40` }}
    >
      <div className="flex items-center gap-2 overflow-hidden flex-1 min-w-0">
        {dragHandleProps && (
          <span
            aria-hidden="true"
            title="Drag to reorder, or onto another node to swap"
            className="text-text-muted text-xs leading-none opacity-0 group-hover:opacity-60 transition-opacity select-none"
          >
            ⠿
          </span>
        )}
        <span className={`w-1.5 h-1.5 rounded-full flex-shrink-0 ${getStatusConfig(node.status).bgColor}`} />
        <ProviderIcon providerId={node.provider} className="h-3.5 w-3.5 drop-shadow-sm flex-shrink-0" />
        <span
          onPointerDown={(e) => e.stopPropagation()}
          className="text-sm font-semibold text-text-primary truncate font-sans drop-shadow-sm min-w-0"
        >
          <InlineEditableText
            value={node.name}
            onCommit={(next) => renameAgentNode(node.id, next)}
            className="text-sm font-semibold text-text-primary font-sans drop-shadow-sm"
          />
          {/* Mesh label is the lowest-priority ID-only second string — the
              title must remain legible at every tier, so it's the first
              thing dropped under compaction. Conditional render shifts the
              title horizontally on tier change; a future iteration could
              place it on a second line under the title if the jitter
              becomes user-visible. */}
          {showMeshLabel && (
            <span className="text-xs text-text-secondary font-normal"> {meshLabel}</span>
          )}
        </span>
        {showWorktree && (
          <span
            title={node.use_worktree
              ? 'Agent runs in a git worktree'
              : 'Agent runs in the repository root'}
            className={`text-2xs font-mono px-1.5 py-0.5 rounded-full leading-none font-medium select-none whitespace-nowrap drop-shadow-sm flex-shrink-0 ${
              node.use_worktree
                ? 'bg-bg-overlay/70 text-text-muted ring-1 ring-inset ring-border-subtle'
                : 'bg-accent-cyan/15 text-accent-cyan ring-1 ring-inset ring-accent-cyan/40 font-semibold'
            }`}
          >
            {node.use_worktree ? 'worktree' : 'root'}
          </span>
        )}
        {showSummary && summary && (
          <span
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => { e.stopPropagation(); setActiveNode(node.id); openProbeTab('review'); }}
            className="text-xs font-mono font-semibold cursor-pointer flex items-center gap-1.5 drop-shadow-sm hover:brightness-125 flex-shrink-0"
            title="Click to see changes"
          >
            {/* Each count carries its own semantic colour so added / modified / deleted
                read at a glance against the mesh-tinted header. Zero counts stay muted
                so the eye lands on the changes that exist. When this node owns the
                agent file-explorer panel the whole chip flips to cyan as a selection
                cue, matching the panel border. */}
            <span className={isPanelNode ? 'text-accent-cyan' : summary.added ? 'text-accent-green' : 'text-text-muted'}>
              +{summary.added}
            </span>
            <span className={isPanelNode ? 'text-accent-cyan' : summary.modified ? 'text-accent-amber' : 'text-text-muted'}>
              ~{summary.modified}
            </span>
            <span className={isPanelNode ? 'text-accent-cyan' : summary.deleted ? 'text-accent-red' : 'text-text-muted'}>
              -{summary.deleted}
            </span>
          </span>
        )}
        {/* Open PR chip — surfaces a clickable link to the PR for the branch
            this node is working on. Hidden when no PR is open (useOpenPr
            returns null for the common cases: no auth, no PR, non-GitHub
            origin, unborn branch). Tooltip carries the PR title; if the PR
            is a draft, the tooltip is suffixed so the user knows. */}
        {showPr && openPr && (
          <span
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation();
              openUrl(openPr.url).catch(console.error);
            }}
            title={openPr.draft ? `Draft · ${openPr.title}` : openPr.title}
            className="text-2xs font-mono px-1.5 py-0.5 rounded-full leading-none font-medium select-none cursor-pointer whitespace-nowrap bg-accent-green/10 text-accent-green ring-1 ring-inset ring-accent-green/30 drop-shadow-sm hover:brightness-125 transition-colors flex-shrink-0"
          >
            PR #{openPr.number}
          </span>
        )}
      </div>
      <div className="flex items-center gap-1.5 flex-shrink-0" onPointerDown={(e) => e.stopPropagation()}>
        <BuildRunDropdown node={node} onBuildRun={onBuildRun} />
        {showInlineActions ? (
          <>
            <button
              type="button"
              onClick={handleOpenInExplorer}
              // Surface matches maximise/close (`bg-bg-base/60` + border)
              // so the trio reads as one control group against the mesh
              // tint. Cyan hover deliberately reuses `PathHeader`'s
              // accent — one verb, one accent, throughout the app.
              // Tooltip includes the resolved path because worktree
              // nodes open into a non-obvious `.claude/worktrees/<name>`
              // subdir, not the mesh root.
              className="w-7 h-7 flex items-center justify-center rounded-md bg-bg-base/60 border border-border-default text-text-primary hover:text-accent-cyan hover:bg-accent-cyan/15 hover:border-accent-cyan/60 transition-colors"
              title={gitPath ? `Open in file explorer (${gitPath})` : 'Open in file explorer'}
              aria-label="Open in file explorer"
            >
              <FolderOpenIcon className="w-3.5 h-3.5" />
            </button>
            <button
              onClick={(e) => { e.stopPropagation(); toggleMaximizedNode(node.id); }}
              // Always-visible button surface (was opacity-0 until hover — invisible
              // against the mesh tint). Same `h-7` + `bg-bg-base/60 + border` as
              // BuildRunDropdown so the trio reads as one control group; hover
              // tint flips to cyan, matching the maximise semantic.
              className="w-7 h-7 flex items-center justify-center rounded-md bg-bg-base/60 border border-border-default text-text-primary hover:text-accent-cyan hover:bg-accent-cyan/15 hover:border-accent-cyan/60 transition-colors"
              // Issue #668 — surface the Alt+G / ⌘+G shortcut in the button
              // tooltip so discoverability isn't gated on the header double-click
              // or the empty-state splash.
              title={isMaximized ? `Restore grid (or ${toggleShortcutHint})` : `Maximize (or ${toggleShortcutHint})`}
              aria-label={isMaximized ? 'Restore grid layout' : 'Maximize agent node'}
            >
              {isMaximized ? (
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M9 9H4m0 0V4m0 5 6-6m5 16v-5m0 0h5m-5 0 6 6M9 15H4m0 0v5m0-5 6 6m5-16V4m0 0h5m-5 0 6 6" />
                </svg>
              ) : (
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M15 3h6m0 0v6m0-6-7 7M9 21H3m0 0v-6m0 6 7-7" />
                </svg>
              )}
            </button>
            <button
              type="button"
              onClick={handleClose}
              // Same surface treatment as the maximise button so the close icon
              // reads at rest, not just on hover. Hover flips to the error
              // semantic so the destructive intent is unmistakable.
              className="w-7 h-7 flex items-center justify-center rounded-md bg-bg-base/60 border border-border-default text-text-primary hover:text-status-error hover:bg-status-error-bg hover:border-status-error/60 transition-colors text-base leading-none"
              title="Close agent node"
              aria-label="Close agent node"
            >
              ×
            </button>
          </>
        ) : (
          // Kebab menu — replaces the inline trio at `slim` and `compact`
          // widths where all three would crowd the title out. Same three
          // actions (Reveal in Explorer + Maximize/Restore + Close) with
          // identical tooltips/aria-labels so semantics are width-agnostic.
          // NOTE: <KebabActions> still uses the pre-#756 `text-text-muted
          // opacity-0` pattern at narrow widths — the always-visible surface
          // treatment introduced in this PR is intentionally scoped to the
          // inline trio so we don't bloat scope. Tracking the analogous
          // kebab-trigger fix separately (issue TBD).
          <KebabActions
            isMaximized={isMaximized}
            toggleShortcutHint={toggleShortcutHint}
            onToggleMaximized={(e) => { e.stopPropagation(); toggleMaximizedNode(node.id); }}
            onClose={handleClose}
            onOpenInExplorer={handleOpenInExplorer}
          />
        )}
      </div>
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
 */
interface KebabActionsProps {
  isMaximized: boolean;
  toggleShortcutHint: string;
  onToggleMaximized: (e: React.MouseEvent) => void;
  onClose: (e: React.MouseEvent) => void;
  onOpenInExplorer: (e: React.MouseEvent) => void;
}

const KEBAB_MIN_WIDTH = 160;
const KEBAB_GAP = 4;

function KebabActions({ isMaximized, toggleShortcutHint, onToggleMaximized, onClose, onOpenInExplorer }: KebabActionsProps) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const menuItemRefs = useRef<(HTMLButtonElement | null)[]>([]);
  // Stable id linking the trigger to the menu for the WAI-ARIA
  // disclosure pattern (aria-controls). Each header instance owns one
  // kebab, so a module-scoped counter is enough.
  const menuIdRef = useRef(`grid-node-kebab-menu-${Math.random().toString(36).slice(2, 9)}`);
  const menuId = menuIdRef.current;
  // Reveal-in-explorer joined the existing maximize/close pair (#736);
  // bump count so arrow navigation wraps at three.
  const itemCount = 3;
  const closeAndReturnFocus = () => {
    const trigger = triggerRef.current;
    setOpen(false);
    requestAnimationFrame(() => trigger?.focus());
  };

  const handleToggle = (e: React.MouseEvent) => {
    e.stopPropagation();
    setOpen((o) => !o);
  };

  // Outside click + Escape close. Same `document`-vs-`window` jsdom
  // caveat as `MeshItem` (issue #735): tests dispatch on `document`,
  // not `window`, because in jsdom the two are independent targets.
  useEffect(() => {
    if (!open) return;
    const onDocClick = (e: MouseEvent) => {
      const menu = menuRef.current;
      const trigger = triggerRef.current;
      if (menu && !menu.contains(e.target as Node) && trigger && !trigger.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    const focusSibling = (currentIdx: number, dir: 1 | -1) => {
      const next = (currentIdx + dir + itemCount) % itemCount;
      menuItemRefs.current[next]?.focus();
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        closeAndReturnFocus();
        return;
      }
      if (e.key === 'Tab') {
        // Non-modal popover (matches MeshItem, #735): Tab leaves the
        // menu and closes it; the browser moves focus to the next
        // tabbable element naturally.
        setOpen(false);
        return;
      }
      // ArrowDown/ArrowUp traverse menuitems with wrap-around, mirroring
      // the MeshItem WAI-ARIA pattern (#735). Wrap matters even at two
      // items so the contract holds if a third is added.
      const items = menuItemRefs.current;
      const activeIdx = items.findIndex((el) => el === document.activeElement);
      if (e.key === 'ArrowDown' && activeIdx >= 0) {
        e.preventDefault();
        focusSibling(activeIdx, 1);
        return;
      }
      if (e.key === 'ArrowUp' && activeIdx >= 0) {
        e.preventDefault();
        focusSibling(activeIdx, -1);
        return;
      }
    };
    document.addEventListener('mousedown', onDocClick);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('mousedown', onDocClick);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [open]);

  // Anchor + viewport-clamp in a single layout pass: read the trigger
  // rect, place the menu's right edge under the trigger's right edge,
  // then nudge back inside the viewport if it overflows. No `pos`
  // state — every layout effect re-reads the live rect, which keeps a
  // re-render (e.g. node name edit) from briefly flashing the menu at
  // a stale anchored position.
  useLayoutEffect(() => {
    if (!open) return;
    const menu = menuRef.current;
    const trigger = triggerRef.current;
    if (!menu || !trigger) return;
    const r = trigger.getBoundingClientRect();
    const top = r.bottom + KEBAB_GAP;
    const left = r.right - KEBAB_MIN_WIDTH;
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const MARGIN = 4;
    const clampedLeft = Math.max(MARGIN, Math.min(left, vw - KEBAB_MIN_WIDTH - MARGIN));
    const clampedTop = Math.max(MARGIN, top);
    menu.style.top = `${clampedTop}px`;
    menu.style.left = `${clampedLeft}px`;
    // If the menu's rendered height would push it off the bottom, raise
    // it above the trigger instead. `vh - clampedTop` is the room left
    // below; if the menu's `offsetHeight` exceeds that, flip up.
    const roomBelow = vh - clampedTop - MARGIN;
    if (menu.offsetHeight > roomBelow) {
      const aboveTop = r.top - KEBAB_GAP - menu.offsetHeight;
      if (aboveTop >= MARGIN) menu.style.top = `${aboveTop}px`;
    }
  }, [open]);

  // Autofocus the first menuitem on open (WAI-ARIA menu contract).
  useLayoutEffect(() => {
    if (open) menuItemRefs.current[0]?.focus();
  }, [open]);

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        onClick={handleToggle}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        aria-label="Agent node actions"
        title="Agent node actions"
        className="w-5 h-5 flex items-center justify-center rounded-md text-text-muted hover:text-text-primary hover:bg-bg-base transition-[color,background-color] opacity-0 group-hover:opacity-100 focus-visible:opacity-100"
      >
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <circle cx="12" cy="5" r="1.2" fill="currentColor" />
          <circle cx="12" cy="12" r="1.2" fill="currentColor" />
          <circle cx="12" cy="19" r="1.2" fill="currentColor" />
        </svg>
      </button>
      {open && (
        <div
          ref={menuRef}
          id={menuId}
          role="menu"
          aria-label="Agent node actions"
          className="fixed bg-bg-overlay border border-border-default rounded-md shadow-md animate-scale-in origin-top-right z-[100] py-1"
          style={{ top: 0, left: 0, minWidth: KEBAB_MIN_WIDTH }}
        >
          <button
            ref={(el) => { menuItemRefs.current[0] = el; }}
            role="menuitem"
            onClick={(e) => { closeAndReturnFocus(); onOpenInExplorer(e); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <FolderOpenIcon className="w-3 h-3" />
            Open in file explorer
          </button>
          <button
            ref={(el) => { menuItemRefs.current[1] = el; }}
            role="menuitem"
            onClick={(e) => { closeAndReturnFocus(); onToggleMaximized(e); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              {isMaximized ? (
                <path d="M9 9H4m0 0V4m0 5 6-6m5 16v-5m0 0h5m-5 0 6 6M9 15H4m0 0v5m0-5 6 6m5-16V4m0 0h5m-5 0 6 6" />
              ) : (
                <path d="M15 3h6m0 0v6m0-6-7 7M9 21H3m0 0v-6m0 6 7-7" />
              )}
            </svg>
            {isMaximized ? `Restore grid (${toggleShortcutHint})` : `Maximize (${toggleShortcutHint})`}
          </button>
          <button
            ref={(el) => { menuItemRefs.current[2] = el; }}
            role="menuitem"
            onClick={(e) => { closeAndReturnFocus(); onClose(e); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <span className="text-text-muted" aria-hidden="true">×</span>
            Close agent node
          </button>
        </div>
      )}
    </>
  );
}
