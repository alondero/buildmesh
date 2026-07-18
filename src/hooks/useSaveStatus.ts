import { formatError } from '../lib/errorUtils';
import { useCallback, useEffect, useRef, useState } from 'react';

/**
 * `useSaveStatus` — small "Saving… / Saved / Save failed" indicator for
 * auto-save probe tabs (issue #729).
 *
 * State machine
 * -------------
 *   idle     — no save in flight, no last-result worth showing
 *   saving   — IPC in flight; clears prior error/saved
 *   saved    — last write succeeded; sticks around for `savedClearMs`
 *              (default 1500ms — long enough to read on a peripheral
 *              monitor, short enough to not linger) then auto-clears
 *              to `idle` if the user hasn't started another edit
 *   error    — last write rejected; `error` carries the rejection
 *              message; persists until the NEXT `start()` call so
 *              the user actually sees the problem
 *
 * This is the same shape `ScratchpadTab.tsx` inlines for its single
 * field. Extracted here so `MeshPropertiesTab` (which has 7 auto-save
 * fields — Name, Model, Effort, Build, Run, Provider, Sandbox) can
 * share the transitions without re-implementing them. Future probe
 * tabs that auto-save should reach for this hook rather than
 * duplicating the state.
 *
 * Single-instance design (issue #729)
 * -----------------------------------
 * The hook is INTENTIONALLY a single-instance state machine per tab
 * — NOT one hook per field. A `useSaveStatusMap` (one instance per
 * field) would prevent cross-talk, but with 7 auto-save fields it
 * would also produce 7 stacked tiny indicators competing for the
 * same attention as the form. The issue's `and/or` wording
 * (indicator inline per field *and/or* a global SaveIndicator) and
 * the existing `WorktreeManagerTab` single-banner precedent both
 * point at the cleaner single-instance path. The cross-talk cost
 * is real but bounded: a fast Model save flipping to "Saved" can
 * briefly mask a slow Build save's "Save failed", but in practice
 * IPC latency is sub-50ms and a 1.5s `savedClearMs` window covers
 * the race. A future "fan-out" form with independently-slow fields
 * would be a candidate to revisit this design.
 *
 * The accompanying `wrappedSave(op)` adapter in
 * `MeshPropertiesTab.tsx` adds a *mesh-switch guard* on top of the
 * single-instance hook: it captures `activeMeshId` at IPC start and
 * discards late results so a slow save from mesh A can't surface
 * its error on mesh B. The hook itself stays general-purpose; the
 * mesh-binding concern lives at the call site.
 *
 * Error-message contract
 * ----------------------
 * `fail(e)` accepts an unknown and normalises it through `formatError`
 * (`src/lib/errorUtils.ts`, issue #663), which unwraps `e.message` for
 * `Error` instances and coerces everything else. The IPC promise
 * rejection carries an `Error` instance from Rust (Tauri's command
 * wrapper), so the message is user-readable copy like "Failed to update
 * mesh column: database is locked" — WITHOUT the `"Error: "` prefix that
 * a bare `String(e)` would prepend.
 *
 * Field-text policy
 * -----------------
 * The hook does NOT touch the caller's input value on `fail`. Whether
 * to revert is the caller's choice; for the mesh probe we deliberately
 * keep the user's text on rejection (issue #729 AC: "On `error`, show
 * the message AND keep the field's text"). The hook's `error` carry
 * provides the surface for the message without forcing a re-render of
 * the input's `value` prop.
 */
export type SaveStatus = 'idle' | 'saving' | 'saved' | 'error';

export interface UseSaveStatusOptions {
  /** How long the `saved` indicator sticks around before going to `idle`.
   *  Set to `0` to disable the auto-clear (suitable for tests that need
   *  the indicator stable across `await waitFor`). */
  savedClearMs?: number;
}

export interface UseSaveStatusResult {
  status: SaveStatus;
  /** Rejection reason for `status === 'error'`; `null` otherwise. */
  error: string | null;
  /** Mark a save as in flight; clears prior error/saved. Returns a
   *  promise so the caller can `await` it (mostly for tests). */
  start: () => void;
  /** Mark a save as successful; auto-clears `saved` after `savedClearMs`. */
  success: () => void;
  /** Mark a save as rejected with `e`. Persists until next `start()`. */
  fail: (e: unknown) => void;
  /** Force-reset to `idle` and cancel any pending auto-clear timer. */
  reset: () => void;
}

export function useSaveStatus(opts: UseSaveStatusOptions = {}): UseSaveStatusResult {
  const { savedClearMs = 1500 } = opts;

  const [status, setStatus] = useState<SaveStatus>('idle');
  const [error, setError] = useState<string | null>(null);

  // Auto-clear handle for the `saved → idle` transition. Stored in a ref
  // (not state) so a re-render during the timer doesn't keep extending
  // the deadline via stale-closure capture. The unmount effect below
  // clears it so a late `setStatus` can't land on an unmounted tree.
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const cancelSavedTimer = useCallback(() => {
    if (savedTimerRef.current !== null) {
      clearTimeout(savedTimerRef.current);
      savedTimerRef.current = null;
    }
  }, []);

  // Cancel any pending timer on unmount, otherwise the post-unmount
  // `setStatus((s) => ...)` would log a React 18+ "state update on
  // unmounted component" warning (visible in dev only, but ugly in
  // test output when fast-fail cascades).
  useEffect(() => {
    return () => cancelSavedTimer();
  }, [cancelSavedTimer]);

  const start = useCallback(() => {
    cancelSavedTimer();
    setError(null);
    setStatus('saving');
  }, [cancelSavedTimer]);

  const success = useCallback(() => {
    cancelSavedTimer();
    setError(null);
    setStatus('saved');
    if (savedClearMs > 0) {
      savedTimerRef.current = setTimeout(() => {
        savedTimerRef.current = null;
        // Guard against a saved → saving transition racing the timer —
        // if the user started a new save, don't undo it back to idle.
        setStatus((s) => (s === 'saved' ? 'idle' : s));
      }, savedClearMs);
    }
  }, [cancelSavedTimer, savedClearMs]);

  const fail = useCallback((e: unknown) => {
    cancelSavedTimer();
    setError(formatError(e));
    setStatus('error');
  }, [cancelSavedTimer]);

  const reset = useCallback(() => {
    cancelSavedTimer();
    setError(null);
    setStatus('idle');
  }, [cancelSavedTimer]);

  return { status, error, start, success, fail, reset };
}
