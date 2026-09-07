import {
  createContext,
  useContext,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
  type Ref,
  type KeyboardEvent as ReactKeyboardEvent,
} from 'react';
import { createPortal } from 'react-dom';

// Lets `<ModalCloseButton>` (rendered inside the consumer's children) route
// its click through the Modal's dirty-aware close instead of closing
// unconditionally, so the header × honours the unsaved-changes guard the
// same way Escape and backdrop-click do (issue #808). `null` when a
// ModalCloseButton is used outside a Modal — it then falls back to its own
// `onClose` prop.
const ModalCloseContext = createContext<(() => void) | null>(null);

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
  /**
   * When true, Escape, backdrop-click, and the header close button
   * (`<ModalCloseButton>`) do NOT immediately close — they surface an inline
   * "Discard unsaved changes?" banner with Keep editing / Discard buttons.
   * Keep editing restores focus to the field the user was in (captured at
   * the moment of the close attempt); only the explicit Discard button calls
   * `onClose`. While the banner is up, a second Escape or backdrop-click
   * dismisses it (Keep editing) rather than confirming the discard — the
   * reflexive "press Escape to dismiss a prompt" gesture maps to the safe
   * option, never to destroying the user's work. The header × routes through
   * the same guard via `ModalCloseContext`. Issues #730, #808.
   */
  dirty?: boolean;
  /** Banner copy when `dirty` and the user tries to close. */
  dirtyMessage?: ReactNode;
  /** Override the Discard button label. */
  discardLabel?: string;
  /** Override the Keep editing button label. */
  cancelLabel?: string;
  /**
   * Override the element that receives focus when the modal mounts. Defaults
   * to the panel itself (`tabIndex={-1}`). Issue #748: lets any consumer
   * pick a focusable target (a header close button, a primary CTA, the
   * first form field) without coupling to `<ModalCloseButton>`'s chrome or
   * any other specific child. The ref MUST be pointing at a focusable
   * element at mount time — if it isn't (e.g. the consumer hasn't attached
   * the ref yet), the modal falls back to the panel.
   */
  defaultFocusRef?: RefObject<HTMLElement | null>;
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
 *
 * Dirty-check flow (issue #730): with `dirty` set, an Escape or stray
 * backdrop click shows an inline confirm banner instead of closing. The
 * banner is rendered above the panel contents so it doesn't displace the
 * form — the user can still tweak the field, hit Save, etc. before deciding.
 * Keep editing closes the banner and walks focus back to the element that
 * had focus when the close was attempted (captured in `lastFocusRef`).
 */
export function Modal({
  onClose,
  labelledBy,
  ariaLabel,
  maxWidth = 'max-w-sm',
  className = 'p-6',
  closeOnBackdrop = true,
  dirty = false,
  dirtyMessage,
  discardLabel = 'Discard changes',
  cancelLabel = 'Keep editing',
  defaultFocusRef,
  children,
}: ModalProps) {
  const panelRef = useRef<HTMLDivElement>(null);
  const cancelButtonRef = useRef<HTMLButtonElement>(null);
  // Set when the user attempts a dirty-gated close. We use it both to swap
  // the JSX (banner vs children) and to keep the Escape handler from
  // re-triggering after the banner is already up.
  const [confirmingDiscard, setConfirmingDiscard] = useState(false);
  // Element that had focus at the moment of the close attempt. Click on the
  // backdrop moves focus to body BEFORE the click handler fires, so we
  // capture on pointerdown; on Escape we capture synchronously in keydown.
  const lastFocusRef = useRef<HTMLElement | null>(null);
  // Mirror `dirty` into a ref so the once-armed Escape handler (registered
  // with `useEffect([], ...)`) sees the live prop value. Without this, a
  // dirty toggle that lands AFTER mount is invisible to Escape — the captured
  // closure still has the mount-time `dirty = false`. (Backdrop is fine
  // because its handler is an inline JSX prop that re-renders with the new
  // value; the Escape listener is the only one registered globally.) Use
  // useLayoutEffect so the ref is updated synchronously before the browser
  // commits the new render, closing the same-tick race that useEffect opens
  // (commit → effect → read window — could land on stale `false` if a user
  // keystroke + Escape land in the same microtask burst).
  const dirtyRef = useRef(dirty);
  useLayoutEffect(() => {
    dirtyRef.current = dirty;
  }, [dirty]);

  // When `dirty` flips false while the banner is up (e.g. the user typed in
  // an API key, hit Escape, then hit Save — Save succeeds, draft re-syncs
  // to account, parent removes the site from dirtySites), the banner would
  // otherwise keep rendering and clicking Discard would close the modal over
  // content that was just saved. Auto-dismiss on the dirty→false transition.
  useEffect(() => {
    if (!dirty) setConfirmingDiscard(false);
  }, [dirty]);

  useEffect(() => {
    const previouslyFocused = document.activeElement as HTMLElement | null;
    // Issue #748: `defaultFocusRef` lets consumers pick a different focus
    // target than the panel. The ref is attached during the children's
    // render (which happens before this effect runs), so the consumer's
    // element is mounted by the time we read it. Falls back to the
    // panel itself when the consumer doesn't pass a ref (preserves the
    // existing behaviour for every modal that doesn't opt in).
    (defaultFocusRef?.current ?? panelRef.current)?.focus();

    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      if (confirmingDiscardRef.current) {
        // Banner already showing — Escape dismisses the prompt (Keep editing),
        // it must NOT confirm the discard. Reflexively pressing Escape to
        // dismiss a confirmation is the near-universal OS gesture, and it maps
        // to the SAFE option; only the explicit Discard button abandons the
        // edits (issue #808). Restore focus to where the user was.
        setConfirmingDiscard(false);
        requestAnimationFrame(() => lastFocusRef.current?.focus?.());
        return;
      }
      if (dirtyRef.current) {
        lastFocusRef.current = document.activeElement as HTMLElement | null;
        setConfirmingDiscard(true);
        return;
      }
      // Not dirty — Escape closes.
      onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
      previouslyFocused?.focus?.();
    };
    // Arm once per mount; onClose identity churn must not re-run focus moves.
    // eslint-disable-next-line react-hooks/exhaustive-deps -- onClose captured on mount; consumer passes a stable callback identity (or accepts the stale closure).
  }, []);

  // Mirror `confirmingDiscard` into a ref so the unmount-once Escape handler
  // sees the live value without re-subscribing (re-subscribing on each toggle
  // would race with the keystroke that triggered it).
  const confirmingDiscardRef = useRef(false);
  useEffect(() => {
    confirmingDiscardRef.current = confirmingDiscard;
  }, [confirmingDiscard]);

  // WAI-ARIA APG alertdialog: when the banner appears, move focus to the
  // safer action (Keep editing) so a screen reader user gets the prompt in
  // their focus-driven virtual cursor, and a keyboard user can hit Space/
  // Enter to dismiss the discard threat without moving the mouse. The
  // captured focus is restored on cancel.
  useEffect(() => {
    if (confirmingDiscard) cancelButtonRef.current?.focus();
  }, [confirmingDiscard]);

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

  // Backdrop click. The wrapper has two children: the absolute-positioned
  // dimmer div and the panel. The panel's onClick stopPropagation handles
  // in-panel clicks (banner, form, etc.). Backdrop clicks come from the
  // dimmer or the wrapper itself, so this handler fires for them.
  //
  // We do NOT gate on e.target !== e.currentTarget — the dimmer div is the
  // actual click target for real users, so the strict check rejects every
  // real backdrop click (regression caught in code review). The panel
  // stopPropagation is the discriminator.
  const handleBackdropClick = () => {
    if (!closeOnBackdrop) return;
    if (confirmingDiscard) {
      // Banner is up — a stray backdrop click dismisses the prompt (Keep
      // editing), it must not confirm the discard (issue #808).
      handleCancelDiscard();
      return;
    }
    if (dirty) {
      setConfirmingDiscard(true);
      return;
    }
    onClose();
  };
  const captureFocusBeforeBackdrop = () => {
    const active = document.activeElement as HTMLElement | null;
    // body falls out of focus on the next click anyway; capture only real targets.
    if (active && active !== document.body) lastFocusRef.current = active;
  };

  const handleCancelDiscard = () => {
    setConfirmingDiscard(false);
    // Restore focus to the field the user was last in — captured at the
    // moment Escape/backdrop intercepted. Defer one tick so React has
    // finished unmounting the banner before we move focus, otherwise the
    // captured element may still be a child of the now-unrendered banner.
    requestAnimationFrame(() => {
      lastFocusRef.current?.focus?.();
    });
  };
  const handleConfirmDiscard = () => {
    setConfirmingDiscard(false);
    onClose();
  };

  // Dirty-aware close for the header × (issue #808). Provided to
  // `<ModalCloseButton>` via context so the most obvious close target honours
  // the same unsaved-changes guard as Escape/backdrop instead of discarding
  // silently. A no-op while the banner is already up (its own buttons decide);
  // a passthrough to `onClose` when the modal isn't dirty.
  const requestClose = () => {
    if (confirmingDiscard) return;
    if (dirty) {
      lastFocusRef.current = document.activeElement as HTMLElement | null;
      setConfirmingDiscard(true);
      return;
    }
    onClose();
  };

  // Issue #1292 — portal the modal to `document.body` so the fixed-
  // positioned wrapper resolves against the viewport, not against an
  // ancestor that creates a containing block (`filter`, `transform`,
  // `opacity`, `backdrop-filter`, `will-change`). The NodeItem row
  // applies `hover:brightness-125` (`filter`); the MeshItem parent
  // applies dnd-kit's `transform`. With the wrapper nested under either
  // of those, `fixed inset-0` shrinks to the ancestor's box and the
  // dialog stops covering the window. Doing this once at the Modal
  // boundary covers every consumer (ConfirmDialog, AppSettingsModal,
  // and the per-tab dialogs in Probe) without patching call sites.
  // Focus trap, defaultFocusRef, Escape/backdrop close, and the dirty
  // discard banner all keep working unchanged — the React tree is
  // identical, only the DOM parent moves.
  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center"
      onMouseDown={captureFocusBeforeBackdrop}
      onClick={handleBackdropClick}
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
        // Stop mousedown propagation too — otherwise captureFocusBeforeBackdrop
        // fires for every click inside the panel and can clobber lastFocusRef
        // with the Save button or banner button that the browser just focused.
        onMouseDown={e => e.stopPropagation()}
        className={`relative bg-bg-overlay border border-border-default rounded-lg shadow-md animate-scale-in outline-none w-full ${maxWidth} ${className}`}
        onClick={e => e.stopPropagation()}
      >
        {confirmingDiscard && (
          <div
            role="alertdialog"
            aria-labelledby="modal-discard-title"
            data-testid="modal-discard-banner"
            className="m-4 mb-0 border border-status-warning/40 bg-status-warning/10 rounded-md px-4 py-3 text-status-warning"
          >
            <p id="modal-discard-title" className="text-base mb-2">
              {dirtyMessage ?? 'Discard unsaved changes?'}
            </p>
            <div className="flex gap-2 justify-end">
              <button
                ref={cancelButtonRef}
                type="button"
                onClick={handleCancelDiscard}
                data-testid="modal-discard-cancel"
                className="px-3 py-1 text-sm rounded-md bg-bg-card text-text-secondary hover:bg-border-subtle"
              >
                {cancelLabel}
              </button>
              <button
                type="button"
                onClick={handleConfirmDiscard}
                data-testid="modal-discard-confirm"
                className="px-3 py-1 text-sm rounded-md bg-status-error text-white hover:bg-status-error/90"
              >
                {discardLabel}
              </button>
            </div>
          </div>
        )}
        <ModalCloseContext.Provider value={requestClose}>
          {children}
        </ModalCloseContext.Provider>
      </div>
    </div>,
    document.body,
  );
}

/**
 * The standard close "×" for modal headers — labelled, hover state, red-shift on hover.
 *
 * Issue #748: used to be wrapped in `forwardRef` so `<ShortcutCheatsheet>`
 * could override the Modal's default focus target. Replaced by the
 * `<Modal defaultFocusRef>` prop, which lets any consumer target an
 * arbitrary focusable element. React 19 accepts `ref` as a regular prop on
 * function components natively, so no wrapper is needed — consumers that
 * still want a handle on the button can pass `<ModalCloseButton ref={...} />`
 * directly (e.g. the cheatsheet uses this to point the Modal at the close
 * button via `defaultFocusRef`).
 */
export function ModalCloseButton({
  onClose,
  label = 'Close',
  ref,
}: {
  onClose: () => void;
  label?: string;
  ref?: Ref<HTMLButtonElement>;
}) {
  // When rendered inside a <Modal>, route the click through the Modal's
  // dirty-aware close so the × honours the unsaved-changes guard (issue #808).
  // Outside a Modal, fall back to the caller's own onClose.
  const requestClose = useContext(ModalCloseContext);
  return (
    <button
      ref={ref}
      type="button"
      onClick={requestClose ?? onClose}
      aria-label={label}
      className="shrink-0 flex items-center justify-center w-7 h-7 rounded-md text-text-secondary hover:text-text-primary hover:bg-white/10 transition-colors text-xl leading-none"
    >
      ×
    </button>
  );
}
