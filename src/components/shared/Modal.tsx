import { forwardRef, useEffect, useRef, type ReactNode, type KeyboardEvent as ReactKeyboardEvent } from 'react';

interface ModalProps {
  onClose: () => void;
  /** id of the heading element inside the modal (preferred accessible name). */
  labelledBy?: string;
  /** Fallback accessible name when there is no heading to point at. */
  ariaLabel?: string;
  /** Tailwind max-width class for the panel. */
  maxWidth?: string;
  /** Extra classes merged onto the panel (padding overrides etc.). */
  className?: string;
  /** Set false for flows where a stray backdrop click would destroy input. */
  closeOnBackdrop?: boolean;
  children: ReactNode;
}

const FOCUSABLE =
  'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * The one modal shell. Owns the behaviours every dialog needs and that the
 * four hand-rolled modals had drifted apart on: backdrop + blur, Escape to
 * close (works even when the WebView is occluded — issue #643), focus moved
 * into the dialog on open and restored on close, Tab trapped inside, and
 * dialog ARIA semantics. Render it conditionally — mounting is what arms the
 * Escape listener, so an always-mounted Modal would steal Escape from agent
 * terminals.
 */
export function Modal({
  onClose,
  labelledBy,
  ariaLabel,
  maxWidth = 'max-w-sm',
  className = 'p-6',
  closeOnBackdrop = true,
  children,
}: ModalProps) {
  const panelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const previouslyFocused = document.activeElement as HTMLElement | null;
    panelRef.current?.focus();

    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
      previouslyFocused?.focus?.();
    };
    // Arm once per mount; onClose identity churn must not re-run focus moves.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Keep Tab cycling inside the panel. A full roving-focus implementation is
  // overkill for these dialogs; wrapping first<->last covers the trap.
  const trapTab = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (e.key !== 'Tab' || !panelRef.current) return;
    const focusable = panelRef.current.querySelectorAll<HTMLElement>(FOCUSABLE);
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const active = document.activeElement;
    if (e.shiftKey && (active === first || active === panelRef.current)) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && active === last) {
      e.preventDefault();
      first.focus();
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center"
      onClick={closeOnBackdrop ? onClose : undefined}
    >
      <div className="absolute inset-0 bg-bg-base/70 backdrop-blur-sm animate-fade-in" />
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={labelledBy}
        aria-label={ariaLabel}
        tabIndex={-1}
        onKeyDown={trapTab}
        className={`relative bg-bg-overlay border border-border-default rounded-lg shadow-md animate-scale-in outline-none w-full ${maxWidth} ${className}`}
        onClick={e => e.stopPropagation()}
      >
        {children}
      </div>
    </div>
  );
}

/** The standard close “×” for modal headers — labelled, hover state, red-shift on hover. */
export const ModalCloseButton = forwardRef<HTMLButtonElement, { onClose: () => void; label?: string }>(
  function ModalCloseButton({ onClose, label = 'Close' }, ref) {
    return (
      <button
        ref={ref}
        type="button"
        onClick={onClose}
        aria-label={label}
        className="shrink-0 flex items-center justify-center w-7 h-7 rounded-md text-text-secondary hover:text-text-primary hover:bg-white/10 transition-colors text-xl leading-none"
      >
        ×
      </button>
    );
  },
);
