/**
 * The dismissal-attempt channel between the shared `<Modal>` and the fields
 * it hosts.
 *
 * Issue #2458: "save on blur" and "ask before discarding" are two contracts
 * that collide. The browser moves focus — and fires `blur` — as part of the
 * dismissal gesture itself: pressing Escape makes the Modal move focus to
 * the discard banner's "Keep editing" button, and pressing the header × or
 * the backdrop moves focus *before* the `click` that starts the dismissal
 * runs. An unguarded blur-commit therefore persists the very draft the user
 * is trying to abandon, the dirty signal clears, and the discard prompt
 * can be skipped altogether — or dismissed again on the next frame, because
 * the Modal auto-dismisses the banner once the dirty signal drops.
 *
 * The Modal owns the attempt, so the Modal owns the signal. While an attempt
 * is in flight, a blur-to-save field must skip its commit and leave the
 * draft dirty for the prompt to ask about.
 *
 * The signal is a *predicate over a ref*, not React state. The attempt has to
 * be latched synchronously on `mousedown` — before the browser moves focus
 * and fires `blur` — and React state is not committed until after the event
 * handler returns, so a state-based flag would still read "no attempt" when
 * the blur arrives.
 */
import { createContext, useContext, type ReactNode } from 'react';

const ModalDismissalContext = createContext<(() => boolean) | null>(null);

/** Stable fallback for fields rendered outside a Modal: no dismissal in flight. */
const NEVER_IN_FLIGHT = () => false;

/**
 * Whether the enclosing Modal is intercepting a dismissal attempt.
 *
 * True from the moment the user starts a dismissal gesture (Escape, the
 * header ×, a backdrop click, or the discard banner's own buttons) until
 * that attempt resolves — the banner is dismissed ("Keep editing") or the
 * modal closes. Outside a Modal this reports false, so a standalone field
 * keeps its ordinary blur-to-save behaviour.
 */
export function useDismissalInFlight(): () => boolean {
  const isInFlight = useContext(ModalDismissalContext);
  return isInFlight ?? NEVER_IN_FLIGHT;
}

/**
 * Publishes the Modal's dismissal signal to its subtree. `isInFlight` must
 * read a ref rather than state — see the module comment.
 */
export function ModalDismissalProvider({
  isInFlight,
  children,
}: {
  isInFlight: () => boolean;
  children: ReactNode;
}) {
  return (
    <ModalDismissalContext.Provider value={isInFlight}>{children}</ModalDismissalContext.Provider>
  );
}
