import { useRef, useState } from 'react';
import { openUrl } from '@tauri-apps/plugin-opener';
import { mergePr } from '../../lib/tauri';
import { formatError } from '../../lib/errorUtils';
import { refreshOpenPrByPath } from '../../hooks/useOpenPr';
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
 * PR pill with merge menu — the agent-node title's `PR #N` chip.
 *
 * Clicking the pill opens a menu (not the browser directly) with:
 *   - "Open on GitHub" (the old direct-click behaviour, now one level in)
 *   - "Merge (squash & delete branch)" gated behind an inline confirm,
 *     mirroring the Probe Panel's Pull Requests tab contract
 *     (`GitPullRequestsTab`: squash + delete branch via `merge_pr`,
 *     irreversible outward action so confirm is required).
 *
 * Draft PRs disable merge (GitHub would reject the squash) with a tooltip.
 * On success the Open-PR cache is refreshed by path so the chip flips to
 * "no open PR" immediately instead of lagging behind the 60s freshness
 * window (same `refreshOpenPrByPath` the Probe merge flow uses).
 *
 * Dropdown plumbing mirrors `BuildRunDropdown`: `relative` wrapper with
 * `data-dropdown-for` scoping, `useClickOutside` + `useAriaMenu` +
 * `useViewportClamp`, `absolute left-0 top-full` menu (left-aligned —
 * the pill lives on the title's left side, unlike the right-side Build
 * trigger which uses `right-0`).
 */
export function PrPill({ nodeId, gitPath, openPr }: PrPillProps) {
  const [open, setOpen] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [merging, setMerging] = useState(false);
  const [mergeError, setMergeError] = useState<string | null>(null);
  const [activeIndex, setActiveIndex] = useState(0);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  // Normal: Open + Merge (2). Confirming: Open + Confirm + Cancel (3).
  // Merging reuses the 2-slot (Open + disabled "Merging…" row).
  const itemCount = confirming ? 3 : 2;

  const closeAndReturnFocus = () => {
    const trigger = triggerRef.current;
    setOpen(false);
    setConfirming(false);
    requestAnimationFrame(() => trigger?.focus());
  };

  useClickOutside<string>(open ? dropdownId('pr-pill', nodeId) : null, () => {
    setOpen(false);
    setConfirming(false);
  });

  useAriaMenu({
    rootRef: menuRef,
    itemCount,
    activeIndex,
    setActiveIndex,
    onClose: closeAndReturnFocus,
    enabled: open,
  });

  useViewportClamp(menuRef, [open, confirming, merging]);

  const handleToggle = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (open) {
      setOpen(false);
      setConfirming(false);
    } else {
      setMergeError(null);
      setOpen(true);
    }
  };

  const handleOpen = (e: React.MouseEvent) => {
    e.stopPropagation();
    setOpen(false);
    setConfirming(false);
    openUrl(openPr.url).catch(console.error);
  };

  const handleMerge = async (e: React.MouseEvent) => {
    e.stopPropagation();
    setMerging(true);
    setMergeError(null);
    try {
      await mergePr(openPr.url);
      if (gitPath) refreshOpenPrByPath(gitPath);
      setOpen(false);
      setConfirming(false);
    } catch (err) {
      setMergeError(formatError(err));
    } finally {
      setMerging(false);
    }
  };

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
          role="menu"
          aria-label={`Pull request #${openPr.number} actions`}
          className="absolute left-0 top-full mt-1 min-w-[240px] bg-bg-overlay border border-border-default rounded-md shadow-md py-1 z-50 animate-scale-in origin-top-left"
        >
          <button
            role="menuitem"
            tabIndex={activeIndex === 0 ? 0 : -1}
            onClick={handleOpen}
            aria-label={`Open pull request #${openPr.number} on GitHub`}
            title={openPr.url}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            Open on GitHub ↗
          </button>
          {merging ? (
            <div
              role="menuitem"
              aria-disabled="true"
              aria-label={`Merging pull request #${openPr.number}`}
              className="w-full px-3 py-1.5 text-left text-xs text-text-muted animate-pulse"
            >
              Merging…
            </div>
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
                onClick={(e) => {
                  e.stopPropagation();
                  setConfirming(false);
                }}
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
              disabled
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
              onClick={(e) => {
                e.stopPropagation();
                setMergeError(null);
                setConfirming(true);
              }}
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
