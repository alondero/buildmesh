import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useEscapeKey } from '../../hooks/useEscapeKey';
import { useAnchoredPosition } from '../../hooks/useAnchoredPosition';

interface InfoTipProps {
  /** Setting name; the trigger reads as "About {label}" to assistive tech. */
  label: string;
  /** Full explanation, revealed in the popover. */
  children: ReactNode;
  testId?: string;
  /** `warning` swaps the ⓘ for an amber !, for a standing condition worth
   *  noticing without spending a line of text. Give it a `triggerLabel` that
   *  names the condition, since the glyph alone says nothing to a screen reader. */
  variant?: 'info' | 'warning';
  /** Overrides the trigger's accessible name (default "About {label}"). */
  triggerLabel?: string;
  /** Test id on the trigger button (`testId` is on the popover). */
  triggerTestId?: string;
}

/** Space between the trigger and the panel. */
const TIP_GAP = 4;
/** Gutter kept between the panel and the viewport edges. */
const TIP_MARGIN = 8;
/**
 * How long the tip waits before it closes when the pointer leaves it.
 *
 * The panel sits a few pixels away from the trigger, so a close that fires
 * on the first `mouseleave` would dismiss the tip while the pointer was
 * still crossing that gap on its way into the panel. The delay is what
 * keeps the pointer path into the help usable.
 */
const HOVER_CLOSE_DELAY_MS = 200;

/**
 * A small ⓘ button that reveals secondary help text in a popover — the
 * affordance that lets Settings rows stay compact instead of stacking a
 * multi-line paragraph under every label.
 *
 * Behaviour (issue #2773):
 *
 *  - **Hover** reveals the tip; leaving it closes the tip again.
 *  - **Click** pins the tip open. It does not close the tip the pointer
 *    just hovered open — that reversal made the help unusable — and a
 *    second click unpins and closes it. A click is also the only way to
 *    open the tip on a touch device, where there is no hover.
 *  - **Escape** closes the tip and returns focus to the trigger. An
 *    outside click closes it without stealing focus (the user clicked
 *    somewhere else).
 *
 * The panel is portalled to `document.body` and positioned against the
 * viewport. Inside the settings scroller it was clipped by that
 * ancestor's `overflow`, so most of the explanation was unreadable, and a
 * higher z-index could not have fixed that. The shared anchoring hook
 * keeps it inside the viewport gutter, flipping it above or to the other
 * edge when the trigger sits near one, and bounds its height so long
 * content scrolls inside the panel instead of running off the screen.
 */
export function InfoTip({ label, children, testId, variant = 'info', triggerLabel, triggerTestId }: InfoTipProps) {
  const [open, setOpen] = useState(false);
  const panelId = useId();
  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  // The pin is read outside React's render cycle (by the close timer and by
  // the click handler), so it lives in a ref rather than in state.
  const pinnedRef = useRef(false);
  const closeTimer = useRef<number | null>(null);

  useAnchoredPosition(triggerRef, panelRef, open, {
    align: 'start',
    gap: TIP_GAP,
    margin: TIP_MARGIN,
    constrainToViewport: true,
  });

  const clearCloseTimer = useCallback(() => {
    if (closeTimer.current !== null) {
      window.clearTimeout(closeTimer.current);
      closeTimer.current = null;
    }
  }, []);

  /** Reveal the tip and cancel a pending close. */
  const reveal = useCallback(() => {
    clearCloseTimer();
    setOpen(true);
  }, [clearCloseTimer]);

  /**
   * Close the tip once the pointer has left it — unless the user pinned
   * it open with a click, in which case it stays until Escape, an outside
   * click, or another click.
   */
  const scheduleClose = useCallback(() => {
    clearCloseTimer();
    closeTimer.current = window.setTimeout(() => {
      closeTimer.current = null;
      if (!pinnedRef.current) setOpen(false);
    }, HOVER_CLOSE_DELAY_MS);
  }, [clearCloseTimer]);

  /** Close the tip and drop the pin, leaving focus where the user left it. */
  const dismiss = useCallback(() => {
    clearCloseTimer();
    pinnedRef.current = false;
    setOpen(false);
  }, [clearCloseTimer]);

  const closeAndReturnFocus = useCallback(() => {
    dismiss();
    const trigger = triggerRef.current;
    requestAnimationFrame(() => trigger?.focus());
  }, [dismiss]);

  /**
   * A click pins the tip open. It does not close the tip the pointer just
   * hovered open — that reversal was the bug in issue #2773. A second
   * click unpins and closes the tip.
   */
  const handleTriggerClick = useCallback(() => {
    clearCloseTimer();
    if (pinnedRef.current) {
      pinnedRef.current = false;
      setOpen(false);
      return;
    }
    pinnedRef.current = true;
    setOpen(true);
  }, [clearCloseTimer]);

  useClickOutside(open ? panelId : null, dismiss);
  useEscapeKey(closeAndReturnFocus, open);

  // A pending close must not fire after the tip is gone.
  useEffect(() => clearCloseTimer, [clearCloseTimer]);

  return (
    <span
      className="relative inline-flex shrink-0"
      data-dropdown-for={open ? panelId : undefined}
      onMouseEnter={reveal}
      onMouseLeave={scheduleClose}
    >
      <button
        ref={triggerRef}
        type="button"
        aria-label={triggerLabel ?? `About ${label}`}
        aria-expanded={open}
        aria-controls={open ? panelId : undefined}
        data-testid={triggerTestId}
        onClick={handleTriggerClick}
        // 24px target (DESIGN.md, explicit px: the root font is 13px); the -m-1
        // margin keeps the 16px layout footprint so rows don't grow.
        className={`-m-1 flex h-[24px] w-[24px] items-center justify-center rounded-full transition-colors focus-visible:outline focus-visible:outline-[1.5px] focus-visible:outline-accent-cyan ${
          variant === 'warning'
            ? 'text-status-warning hover:text-text-primary focus-visible:text-text-primary'
            : 'text-text-muted hover:text-accent-cyan focus-visible:text-accent-cyan'
        }`}
      >
        <svg viewBox="0 0 16 16" className="h-3.5 w-3.5" fill="none" aria-hidden="true">
          <circle cx="8" cy="8" r="6.25" stroke="currentColor" strokeWidth="1.5" />
          {variant === 'warning' ? (
            <>
              <path d="M8 4.5v4.25" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
              <circle cx="8" cy="11.25" r="0.9" fill="currentColor" />
            </>
          ) : (
            <>
              <path d="M8 7.25v3.75" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
              <circle cx="8" cy="4.75" r="0.9" fill="currentColor" />
            </>
          )}
        </svg>
      </button>
      {open && createPortal(
        <div
          ref={panelRef}
          id={panelId}
          role="tooltip"
          data-testid={testId}
          // The panel is portalled out of the trigger's subtree, so it
          // carries the outside-click scope itself — a click inside the
          // panel is not an outside click.
          data-dropdown-for={panelId}
          onMouseEnter={reveal}
          onMouseLeave={scheduleClose}
          className="fixed z-[60] w-72 max-w-[min(20rem,calc(100vw-4rem))] overflow-y-auto overscroll-contain rounded-md border border-border-default bg-bg-overlay p-3 text-sm text-text-muted shadow-md animate-scale-in origin-top-left"
        >
          {children}
        </div>,
        document.body,
      )}
    </span>
  );
}
