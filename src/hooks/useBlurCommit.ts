/**
 * `useBlurCommit` — the "save on blur" contract, guarded against a modal's
 * discard prompt (issue #2458).
 *
 * A Settings field that commits its draft when focus leaves it collides with
 * the Modal's unsaved-changes guard: the dismissal gesture *is* a focus move,
 * so the blur fires while the user is trying to abandon the edit, and the
 * commit persists the draft the discard prompt is asking about. The Modal
 * owns the attempt (see `components/shared/modalDismissal`), so a
 * blur-to-save field asks it whether a dismissal is in flight and skips the
 * commit when the answer is yes.
 *
 * Outside a Modal there is no attempt to fear, so the guard is a no-op and
 * the field keeps its plain blur-to-save behaviour.
 *
 * ## What counts as a dismissal
 *
 * Only a gesture that tries to leave the modal: Escape, the header ×, a
 * backdrop click, and the discard banner's own buttons. Moving focus
 * *within* the modal is not a dismissal — clicking another Settings tab, or
 * tabbing to the next field, is an ordinary focus move, and the draft is
 * saved as the blur-to-save contract intends. The distinction is intent, not
 * mechanics: the guarded gestures are the ones that mean "I am done here",
 * and a draft must not be persisted behind the user's back while the modal
 * is asking whether to keep it.
 */
import { useCallback, type FocusEvent } from 'react';
import { useDismissalInFlight } from '../components/shared/modalDismissal';

/**
 * Returns `commitOnBlur(commit)` — a blur handler that runs `commit` unless
 * the enclosing Modal is intercepting a dismissal.
 *
 * `shouldCommit` adds a caller-specific condition on top (the Probe prompt
 * editors, for instance, skip the commit when focus moved to one of their
 * own placeholder buttons). It is only consulted when no dismissal is in
 * flight.
 */
export function useBlurCommit(): (
  commit: () => void | Promise<void>,
  shouldCommit?: (event: FocusEvent<HTMLElement>) => boolean,
) => (event: FocusEvent<HTMLElement>) => void {
  const isDismissalInFlight = useDismissalInFlight();
  return useCallback(
    (commit, shouldCommit) => (event) => {
      // The user is abandoning the edit, not finishing it: this blur is
      // part of the dismissal gesture, so the draft stays uncommitted and
      // the discard prompt can still ask about it.
      if (isDismissalInFlight()) return;
      if (shouldCommit && !shouldCommit(event)) return;
      void commit();
    },
    [isDismissalInFlight],
  );
}
