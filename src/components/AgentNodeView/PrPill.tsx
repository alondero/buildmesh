import { useEffect, useId, useRef, useState } from 'react';
import { openUrl } from '@tauri-apps/plugin-opener';
import { mergePr } from '../../lib/tauri';
import { formatError } from '../../lib/errorUtils';
import { invalidateOpenPrForNode } from '../../hooks/useOpenPr';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useAriaMenu } from '../../hooks/useAriaMenu';
import { useViewportClamp } from '../../hooks/useViewportClamp';
import { dropdownId } from '../../lib/dropdownId';
import type { OpenPr } from '../../types/generated/OpenPr';

interface PrPillProps {
  nodeId: number;
  gitPath: string | null;
  openPr: OpenPr;
}

/**
 * PR pill merge menu — the agent-node title's `PR #N` chip.
 *
 * Click opens a menu with Open on GitHub plus Merge (squash and
 * delete branch) behind an inline confirm, matching the Probe Pull
 * Requests tab contract. Drafts expose merge as aria-disabled.
 * A merge failure keeps the menu open with the error; the error
 * persists across close/reopen until the next merge attempt.
 */
export function PrPill({ nodeId, gitPath, openPr }: PrPillProps) {
  const [open, setOpen] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [merging, setMerging] = useState(false);
  const [mergeError, setMergeError] = useState<string | null>(null);
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
  // starts — so the count always matches the rendered rows:
  // merging renders Open + Merging (2), confirming renders
  // Open + Confirm + Cancel (3), otherwise Open + Merge (2).
  const itemCount = merging ? 2 : confirming ? 3 : 2;

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

  useClickOutside<string>(open ? dropdownId('pr-pill', nodeId) : null, handleDismiss);

  useAriaMenu({
    rootRef: menuRef,
    itemCount,
    activeIndex,
    setActiveIndex,
    onClose: closeAndReturnFocus,
    enabled: open,
  });

  useViewportClamp(menuRef, [open, confirming, merging, mergeError]);

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

  const handleArmConfirm = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (merging) return;
    setMergeError(null);
    setConfirming(true);
  };

  const handleCancelConfirm = (e: React.MouseEvent) => {
    e.stopPropagation();
    setConfirming(false);
    // After cancel, itemCount drops from 3 back to 2 (Open + Merge).
    // The previous activeIndex (2 = Cancel) is now out of bounds, so
    // both menuitems would render with tabIndex=-1 until an arrow key
    // moved focus — a focus drop. Pin to the Merge slot (1) and pull
    // focus onto it after the unmount has settled, mirroring the
    // closeAndReturnFocus trigger-return pattern used elsewhere in
    // this component.
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
    // Reset confirming synchronously with arming merging: from this
    // render on the menu shows Open + Merging (2 rows) and itemCount
    // is 2, so arrow navigation can never address a stale index 2.
    // activeIndex returns to 0 for the same reason (Cancel sat at 2).
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
        className="text-2xs font-mono px-1.5 py-0.5 rounded-full leading-none font-medium select-none cursor-pointer whitespace-nowrap bg-accent-green/10 text-accent-green ring-1 ring-inset ring-accent-green/30 drop-shadow-sm hover:brightness-125 transition-colors flex-shrink-0"
      >
        PR #{openPr.number}
      </button>

      {open && (
        <div
          ref={menuRef}
          id={menuId}
          role="menu"
          aria-label={`Pull request #${openPr.number} actions`}
          className="absolute left-0 top-full mt-1 min-w-[240px] bg-bg-overlay border border-border-default rounded-md shadow-md py-1 z-50 animate-scale-in origin-top-left"
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
          {mergeError && (
            <p role="alert" className="text-2xs text-status-error px-3 py-1 max-w-[240px] break-words">
              {mergeError}
            </p>
          )}
        </div>
      )}
    </div>
  );
}
