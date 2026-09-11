import { useCallback, useId, useRef, useState, type ReactNode } from 'react';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useEscapeKey } from '../../hooks/useEscapeKey';

interface InfoTipProps {
  /** Setting name; the trigger reads as "About {label}" to assistive tech. */
  label: string;
  /** Full explanation, revealed in the popover. */
  children: ReactNode;
  testId?: string;
}

/**
 * A small ⓘ button that reveals secondary help text in a popover — the
 * affordance that lets Settings rows stay compact instead of stacking a
 * multi-line paragraph under every label. Opens on hover and toggles on click
 * (so touch users can reveal it without hover); closes on mouse-leave, Escape,
 * or an outside mousedown. Escape returns focus to the trigger, matching the
 * disclosure contract in `TitleBar/ZoomControl.tsx`.
 */
export function InfoTip({ label, children, testId }: InfoTipProps) {
  const [open, setOpen] = useState(false);
  const panelId = useId();
  const triggerRef = useRef<HTMLButtonElement>(null);
  const close = () => setOpen(false);

  // `panelId` doubles as the scoped selector value for useClickOutside
  // (mirrors ZoomControl's dropdown id). Escape is handled by the shared
  // LIFO dispatcher, so the tip closes before the surrounding Modal.
  // Escape walks focus back to the trigger so a keyboard user isn't dumped at
  // document.body; an outside click closes without stealing focus (the user
  // clicked elsewhere) — hence the separate handler.
  const closeAndReturnFocus = useCallback(() => {
    const trigger = triggerRef.current;
    setOpen(false);
    requestAnimationFrame(() => trigger?.focus());
  }, []);

  useClickOutside(open ? panelId : null, close);
  useEscapeKey(closeAndReturnFocus, open);

  return (
    <span
      className="relative inline-flex shrink-0"
      data-dropdown-for={open ? panelId : undefined}
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
    >
      <button
        ref={triggerRef}
        type="button"
        aria-label={`About ${label}`}
        aria-expanded={open}
        aria-controls={open ? panelId : undefined}
        onClick={() => setOpen((value) => !value)}
        className="flex h-4 w-4 items-center justify-center rounded-full text-text-muted transition-colors hover:text-accent-cyan focus:outline-none focus-visible:text-accent-cyan"
      >
        <svg viewBox="0 0 16 16" className="h-3.5 w-3.5" fill="none" aria-hidden="true">
          <circle cx="8" cy="8" r="6.25" stroke="currentColor" strokeWidth="1.5" />
          <path d="M8 7.25v3.75" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          <circle cx="8" cy="4.75" r="0.9" fill="currentColor" />
        </svg>
      </button>
      {open && (
        <div
          id={panelId}
          role="tooltip"
          data-testid={testId}
          className="absolute left-0 top-full z-50 mt-1 w-72 max-w-[min(20rem,calc(100vw-4rem))] rounded-md border border-border-default bg-bg-overlay p-3 text-sm text-text-muted shadow-md animate-scale-in origin-top-left"
        >
          {children}
        </div>
      )}
    </span>
  );
}
