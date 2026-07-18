/**
 * Optimistic-with-rollback toggle helper (issue #587).
 *
 * Repeated for every setting toggle in the Settings modal before this helper
 * existed: save the previous value, flip the local state immediately, call the
 * backend mutation, and roll the local state back if the call rejects. The
 * shape is identical across `handleToggleCoordinator` and
 * `handleToggleLanExposure`; the LAN handler additionally re-reads the
 * realized network status after a successful mutation, which the `onSuccess`
 * hook covers.
 *
 * Why a plain function (not a hook): the component owns the state setters
 * already; a hook would just close over them. A function with explicit
 * `current` keeps the rollback value fresh across the call (no stale
 * `useRef`), and matches the surrounding `src/lib/*.ts` style.
 *
 * `user.click` swallows a rejected async `onClick`, so the try/catch/finally
 * MUST live inside the handler — callers wire this helper straight into the
 * toggle's onClick and the swallowed rejection is handled here.
 */
import { formatError } from './errorUtils';

export async function optimisticToggle<T>(args: {
  current: T;
  next: T;
  setValue: (v: T) => void;
  setBusy: (b: boolean) => void;
  setError: (e: string | null) => void;
  mutation: () => Promise<void>;
  /** Optional post-success hook. Awaited before `setBusy(false)` so a
   *  follow-up refresh can still fail visibly inside the toggle's busy
   *  state (e.g. the LAN handler's `refreshNetworkStatus`). */
  onSuccess?: () => void | Promise<void>;
}): Promise<void> {
  const previous = args.current;
  args.setValue(args.next);
  args.setBusy(true);
  args.setError(null);
  try {
    await args.mutation();
    if (args.onSuccess) {
      await args.onSuccess();
    }
  } catch (e) {
    args.setValue(previous);
    args.setError(formatError(e));
  } finally {
    args.setBusy(false);
  }
}
