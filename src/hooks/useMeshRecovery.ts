import { useCallback, useRef, useState } from 'react';
import { formatError } from '../lib/errorUtils';
import {
  freeBaseBranch,
  restoreMeshToBase,
  type HoldingWorktree,
} from '../lib/tauri';

/**
 * Mesh-health recovery actions (issue #283 — extracted from the inline
 * orchestration that used to live in `BranchesWorktreesSection`; #377
 * moved the live UI to the `WorktreeManagerTab` inside the Probe Panel
 * and deleted the now-dead legacy file).
 *
 * Owns the in-flight flag + one-line status message + the "always invalidate
 * health + prune caches after a successful recovery" rule, so adding a third
 * recovery action means one more `useCallback` here rather than another
 * scattered `handleX` + two extra useState hooks in the panel.
 *
 * `meshId` is nullable to match the `useMeshHealth` signature: a tab
 * rendered against the probe's "no project selected" empty state may
 * briefly have `meshId === null` (or call the hook on every render
 * without paying the early-return tax). The `restore` / `free` callbacks
 * no-op when `meshId === null` so a stale closure from a previous mesh
 * can't fire `restoreMeshToBase(null)` against the backend.
 *
 * `onMutate` runs ONLY on a successful recovery. A failed restore/free must
 * NOT bump caches — otherwise the panel loses its stale-but-actionable state
 * and looks "fixed". The hook also blocks re-entry while a recovery is in
 * flight: the HealthBlock button already disables itself on `inFlight`, but
 * the guard lives in the hook so a future caller (test, programmatic) can't
 * double-fire the same IPC.
 */
export function useMeshRecovery(meshId: number | null, onMutate: () => void) {
  const [restoreInFlight, setRestoreInFlight] = useState(false);
  const [freeInFlight, setFreeInFlight] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  // The functional setter races on rapid double-clicks (React batching),
  // so a ref makes the guard synchronous — pin the test that double-fires
  // restore while the first is still pending.
  const inFlightRef = useRef(false);

  const restore = useCallback(async () => {
    if (meshId === null) return;
    if (inFlightRef.current) return;
    inFlightRef.current = true;
    setRestoreInFlight(true);
    setMessage(null);
    try {
      const result = await restoreMeshToBase(meshId);
      setMessage(result.message);
      onMutate();
    } catch (e) {
      setMessage(`Restore error: ${formatError(e)}`);
    } finally {
      setRestoreInFlight(false);
      inFlightRef.current = false;
    }
  }, [meshId, onMutate]);

  const free = useCallback(async (holder: HoldingWorktree) => {
    if (meshId === null) return;
    if (inFlightRef.current) return;
    inFlightRef.current = true;
    setFreeInFlight(true);
    setMessage(null);
    try {
      const result = await freeBaseBranch(meshId, holder.path);
      setMessage(`Freed base branch at ${result.detached_at_sha}`);
      onMutate();
    } catch (e) {
      setMessage(`Free error: ${formatError(e)}`);
    } finally {
      setFreeInFlight(false);
      inFlightRef.current = false;
    }
  }, [meshId, onMutate]);

  return {
    restore,
    free,
    inFlight: restoreInFlight || freeInFlight,
    message,
  };
}
