import { useEffect, useId, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { openUrl } from '@tauri-apps/plugin-opener';
import { mergePr } from '../../lib/tauri';
import { formatError } from '../../lib/errorUtils';
import { invalidateOpenPrForNode } from '../../hooks/useOpenPr';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useAriaMenu } from '../../hooks/useAriaMenu';
import { useAnchoredPosition } from '../../hooks/useAnchoredPosition';
import { dropdownId } from '../../lib/dropdownId';
import type { OpenPr } from '../../types/generated/OpenPr';
import type { SpawnOption } from '../../lib/groups';
import { PrReviewerSpawnDialog } from './PrReviewerSpawnDialog';

interface PrPillProps {
  nodeId: number;
  meshId: number;
  gitPath: string | null;
  openPr: OpenPr;
  /** Spawn Options for the reviewer picker — the shared Spawn Menu
   *  (`GroupedProviderMenu`), same list the PRs probe's `+ ▾` renders. */
  providers: SpawnOption[];
  compact?: boolean;
}

/**
 * PR pill menu — the agent-node title's `PR #N` chip.
 *
 * Click opens a menu with Open on GitHub plus Merge (squash and
 * delete branch) behind an inline confirm, matching the Probe Pull
 * Requests tab contract. Drafts expose merge as aria-disabled.
 * A merge failure keeps the menu open with the error; the error
 * persists across close/reopen until the next merge attempt.
 *
 * The last row spawns a **reviewer agent** for this PR: the same
 * `create_pr_node` spawn the Probe Pull Requests tab's `+` performs, but with
 * `reviewer: true` (own worktree) and grouped onto this node's card as another
 * Node Activity tab. The provider chooser is the shared Spawn Menu, rendered
 * in a modal (`PrReviewerSpawnDialog`) rather than a submenu so its own
 * keyboard handling can't fight this menu's roving focus.
 */
export function PrPill({ nodeId, meshId, gitPath, openPr, providers, compact = false }: PrPillProps) {
  const [open, setOpen] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [merging, setMerging] = useState(false);
  const [mergeError, setMergeError] = useState<string | null>(null);
  const [spawnOpen, setSpawnOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  // Guards the post-await setStates: the menu may close (outside
  // click / Escape / Open click) or the whole pill may unmount
  // (cache invalidation flips openPr to null) while merge is in
  // flight. Without this the resolution would set state on an
  // unmounted component and a late failure would be invisible.
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // State-machine invariant: confirming and merging are never true
  // together — handleMerge resets confirming the moment merge
  // starts — so the count always matches the rendered rows, plus the
  // trailing "Spawn reviewer agent…" row that is present in every
  // state: merging renders Open + Merging + Spawn (3), confirming
  // renders Open + Confirm + Cancel + Spawn (4), otherwise
  // Open + Merge + Spawn (3).
  const itemCount = confirming ? 4 : 3;
  // The spawn row is always last, so the merge rows keep their indices
  // (handleCancelConfirm re-pins the Merge slot at 1).
  const spawnIndex = itemCount - 1;

  const closeAndReturnFocus = () => {
    const trigger = triggerRef.current;
    setOpen(false);
    setConfirming(false);
    requestAnimationFrame(() => trigger?.focus());
  };

  const handleDismiss = () => {
    setOpen(false);
    setConfirming(false);
  };

  // Closing the reviewer dialog must return focus to the pill trigger. The
  // dialog's own `Modal` restores focus to whatever was focused when it
  // mounted — the portaled menu row — but that row unmounts in the same commit
  // that mounts the dialog, so the restore lands on <body>. The trigger is the
  // persistent control for this flow, so reuse the menu's trigger-return path.
  const closeSpawnDialog = () => {
    setSpawnOpen(false);
    closeAndReturnFocus();
  };

  useClickOutside<string>(open ? dropdownId('pr-pill', nodeId) : null, handleDismiss);

  useAnchoredPosition(triggerRef, menuRef, open);

  useAriaMenu({
    rootRef: menuRef,
    itemCount,
    activeIndex,
    setActiveIndex,
    onClose: closeAndReturnFocus,
    enabled: open,
  });

  const menuId = useId();

  const handleToggle = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (open) {
      handleDismiss();
    } else {
      // Deliberately preserves mergeError: a failure that landed
      // while the menu was closed (dismissed mid-merge) must still
      // be readable on reopen instead of wiped before first paint.
      setOpen(true);
    }
  };

  const handleOpen = (e: React.MouseEvent) => {
    e.stopPropagation();
    // Disabled while merging so the click cannot unmount the menu
    // out from under the in-flight merge IPC.
    if (merging) return;
    handleDismiss();
    openUrl(openPr.url).catch(console.error);
  };

  const handleSpawnReviewer = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (merging) return;
    handleDismiss();
    setSpawnOpen(true);
  };

  const handleArmConfirm = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (merging) return;
    setMergeError(null);
    setConfirming(true);
  };

  const handleCancelConfirm = (e: React.MouseEvent) => {
    e.stopPropagation();
    setConfirming(false);
    // After cancel the menu is back to Open + Merge + Spawn (itemCount 3), so
    // index 2 (Cancel) is still in range — pin the caret to the Merge slot (1)
    // anyway, so focus returns to the action the user just backed out of rather
    // than landing on the spawn row, and pull focus onto it after the unmount
    // has settled (mirroring the closeAndReturnFocus trigger-return pattern
    // used elsewhere in this component).
    setActiveIndex(1);
    requestAnimationFrame(() => {
      menuRef.current
        ?.querySelectorAll<HTMLElement>('[role="menuitem"]')[1]
        ?.focus();
    });
  };

  const handleMerge = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (merging) return;
    // Reset confirming synchronously with arming merging: from this render on
    // the menu shows Open + Merging + Spawn (3 rows), matching itemCount 3, and
    // the roving index returns to the top row alongside it (Cancel sat at 2).
    setMerging(true);
    setConfirming(false);
    setActiveIndex(0);
    setMergeError(null);
    try {
      await mergePr(openPr.url);
      if (!mountedRef.current) return;
      // Drops this node's cache entry even when no hook instance is
      // mounted, then notifies path subscribers — the chip flips to
      // "no open PR" instead of lagging behind the freshness window.
      // gitPath is non-null whenever the pill renders (the header
      // returns null when the node is not loaded), so the guard is
      // dead-code defensive, never a silent skip in practice.
      if (gitPath) invalidateOpenPrForNode(nodeId, gitPath);
      setOpen(false);
    } catch (err) {
      if (!mountedRef.current) return;
      // confirming is already false: the error presents alongside
      // the plain Merge row, menu stays open for a retry.
      setMergeError(formatError(err));
    } finally {
      if (mountedRef.current) setMerging(false);
    }
  };

  const openDisabled = merging;
  const mergeDisabled = merging;

  return (
    <div
      className="relative flex-shrink-0"
      onPointerDown={(e) => e.stopPropagation()}
      data-dropdown-for={open ? dropdownId('pr-pill', nodeId) : undefined}
    >
      <button
        ref={triggerRef}
        type="button"
        onClick={handleToggle}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        aria-label={`Open pull request #${openPr.number} options`}
        title={openPr.draft ? `Draft · ${openPr.title}` : openPr.title}
        data-testid="pr-pill-trigger"
        className={compact
          ? 'flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-accent-green/10 text-accent-green ring-1 ring-inset ring-accent-green/30 hover:brightness-125 transition-colors'
          : 'text-2xs font-mono px-1.5 py-0.5 rounded-full leading-none font-medium select-none cursor-pointer whitespace-nowrap bg-accent-green/10 text-accent-green ring-1 ring-inset ring-accent-green/30 drop-shadow-sm hover:brightness-125 transition-colors flex-shrink-0'}
      >
        {compact ? (
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <circle cx="18" cy="18" r="3" />
            <circle cx="6" cy="6" r="3" />
            <path d="M13 6h3a2 2 0 0 1 2 2v7" />
            <line x1="6" x2="6" y1="9" y2="21" />
          </svg>
        ) : `PR #${openPr.number}`}
      </button>

      {/* Escape the title's clipping and transformed node containing blocks. */}
      {open && createPortal(
        <div
          ref={menuRef}
          id={menuId}
          role="menu"
          aria-label={`Pull request #${openPr.number} actions`}
          data-dropdown-for={dropdownId('pr-pill', nodeId)}
          className="fixed w-[240px] max-w-[calc(100vw-8px)] max-h-[calc(100vh-8px)] overflow-y-auto bg-bg-overlay border border-border-default rounded-md shadow-md animate-scale-in origin-top-left z-[100]"
        >
          <button
            role="menuitem"
            tabIndex={activeIndex === 0 ? 0 : -1}
            aria-disabled={openDisabled}
            onClick={handleOpen}
            aria-label={`Open pull request #${openPr.number} on GitHub`}
            title={merging ? 'Merge in progress' : openPr.url}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors aria-disabled:opacity-50 aria-disabled:cursor-not-allowed aria-disabled:hover:bg-transparent aria-disabled:hover:text-text-primary"
          >
            Open on GitHub ↗
          </button>
          {merging ? (
            <button
              role="menuitem"
              tabIndex={activeIndex === 1 ? 0 : -1}
              aria-disabled="true"
              onClick={(e) => e.stopPropagation()}
              aria-label={`Merging pull request #${openPr.number}`}
              title="Merge in progress"
              className="w-full px-3 py-1.5 text-left text-xs text-text-muted animate-pulse cursor-wait"
            >
              Merging…
            </button>
          ) : confirming ? (
            <>
              <button
                role="menuitem"
                tabIndex={activeIndex === 1 ? 0 : -1}
                onClick={handleMerge}
                aria-label={`Confirm squash merge of pull request #${openPr.number}`}
                title="Confirm squash merge"
                className="w-full px-3 py-1.5 text-left text-xs font-medium text-accent-green hover:bg-accent-green/15 transition-colors"
              >
                Confirm squash merge
              </button>
              <button
                role="menuitem"
                tabIndex={activeIndex === 2 ? 0 : -1}
                onClick={handleCancelConfirm}
                aria-label={`Cancel merge of pull request #${openPr.number}`}
                title="Cancel"
                className="w-full px-3 py-1.5 text-left text-xs text-text-muted hover:bg-bg-base hover:text-text-secondary transition-colors"
              >
                Cancel
              </button>
            </>
          ) : openPr.draft ? (
            <button
              role="menuitem"
              tabIndex={activeIndex === 1 ? 0 : -1}
              aria-disabled="true"
              onClick={(e) => e.stopPropagation()}
              aria-label={`Merge pull request #${openPr.number} (unavailable for drafts)`}
              title="Draft PR can't be merged yet"
              className="w-full px-3 py-1.5 text-left text-xs text-text-muted opacity-50 cursor-not-allowed"
            >
              Merge (squash &amp; delete branch)
            </button>
          ) : (
            <button
              role="menuitem"
              tabIndex={activeIndex === 1 ? 0 : -1}
              aria-disabled={mergeDisabled}
              onClick={handleArmConfirm}
              aria-label={`Merge pull request #${openPr.number}`}
              title="Merge pull request (squash & delete branch)"
              className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
            >
              Merge (squash &amp; delete branch)
            </button>
          )}
          <button
            role="menuitem"
            tabIndex={activeIndex === spawnIndex ? 0 : -1}
            aria-disabled={merging}
            onClick={handleSpawnReviewer}
            data-testid="pr-spawn-reviewer"
            aria-label={`Spawn reviewer agent for pull request #${openPr.number}`}
            title={merging ? 'Merge in progress' : 'Spawn a reviewer agent for this PR in its own worktree'}
            className="w-full border-t border-border-subtle px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors aria-disabled:opacity-50 aria-disabled:cursor-not-allowed aria-disabled:hover:bg-transparent aria-disabled:hover:text-text-primary"
          >
            Spawn reviewer agent…
          </button>
          {mergeError && (
            <p role="alert" className="text-2xs text-status-error px-3 py-1 max-w-[240px] break-words">
              {mergeError}
            </p>
          )}
        </div>,
        document.body,
      )}
      {spawnOpen && (
        <PrReviewerSpawnDialog
          nodeId={nodeId}
          meshId={meshId}
          openPr={openPr}
          providers={providers}
          onClose={closeSpawnDialog}
        />
      )}
    </div>
  );
}
