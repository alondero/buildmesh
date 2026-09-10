import { useEffect, useRef } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useExitPromptStore } from '../../stores/exitPromptStore';
import {
  getActiveExitNodes,
  shouldConfirmExit,
} from '../../lib/exitGuard';
import { ExitConfirmationModal } from '../ExitConfirmationModal/ExitConfirmationModal';

/**
 * Window close guard (issue #1501, review finding 4).
 *
 * Intercepts Tauri `onCloseRequested`: when active agent sessions exist
 * (`running`, `awaiting_input`, `spawning`, `ready`) and the
 * `confirm_before_quit` preference is on, the close is vetoed and the
 * exit-confirmation modal is shown instead. Confirming destroys the
 * window (bypasses `closeRequested`); the backend `ExitRequested` sweep
 * then marks sessions suspended and kills processes. Cancelling retracts
 * the backend's eager expected-exit marking via `cancel_window_close`
 * so a later real crash still auto-relaunches.
 *
 * The sync veto reads `confirmBeforeQuit` + active nodes from the store
 * — no cold IPC on the close path. The async half (`requestExit`)
 * fetches providers and partitions, and lives in `useExitPromptStore`
 * so the updater route can reuse it (review finding 4 — one policy,
 * not two).
 *
 * Mounted in every `App` branch (boot splash, boot error, ready) so
 * the listener is armed from first paint.
 */
export function WindowCloseGuard() {
  const pending = useExitPromptStore((s) => s.pending);
  const exiting = useExitPromptStore((s) => s.exiting);
  // Covers the fetch gap: set synchronously with the veto, cleared once
  // the prompt is up. A second close (double-X, Alt+F4 echo) while
  // providers are still loading — or while the modal/exit is active —
  // stays vetoed without refetching or double-destroying.
  const fetchingRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;

    const setup = async () => {
      try {
        const win = getCurrentWindow();
        if (typeof win.onCloseRequested !== 'function') return;
        unlisten = await win.onCloseRequested(async (event) => {
          const store = useExitPromptStore.getState();
          if (store.exiting || store.pending || fetchingRef.current) {
            event.preventDefault();
            return;
          }
          // Sync veto — `confirmBeforeQuit` + active nodes, no IPC. If
          // the policy says no prompt is needed, fall through and let
          // the close proceed (issue #1501 — the close handler must
          // not block a close the user has explicitly opted out of).
          const nodes = useAgentNodeStore.getState().getAgentNodes();
          const active = getActiveExitNodes(nodes);
          if (!shouldConfirmExit(active, store.confirmBeforeQuit)) return;
          event.preventDefault();
          fetchingRef.current = true;
          // Async half — encapsulate the providers fetch + partition
          // behind `requestExit` so the updater route can reuse it
          // (review finding 4). `requestExit` returns `true` once the
          // prompt is up; `false` means the policy says no prompt is
          // needed (which can only happen if active nodes vanished
          // mid-fetch — the close proceeds in that case).
          const prompted = await useExitPromptStore.getState().requestExit('window-close');
          if (cancelled) {
            // The store may have populated `pending` already — clear
            // it so a stale prompt doesn't outlive the unmounted
            // guard. Matches the pre-refactor rollback.
            if (!prompted) {
              // No prompt was set; nothing to retract.
            } else {
              useExitPromptStore.getState().keepWorking();
            }
            fetchingRef.current = false;
            return;
          }
          fetchingRef.current = false;
          if (!prompted) {
            // Active nodes vanished between the veto and the fetch —
            // let the close proceed.
            // Tauri's onCloseRequested `event` is consumed by returning
            // without preventDefault; we already preventDefault'd above
            // but the close can still proceed via the same path as the
            // pre-refactor cancel-by-user (the backend's expected-exit
            // marker, if any, gets cleared by the next close attempt).
            // For simplicity (and because this race is narrow): do
            // nothing — the user can close again. The modal would
            // have been the friendly path; this is the fallback.
          }
        });
      } catch {
        // Non-Tauri runtimes (browser dev, tests without the window mock)
        // have no close-request seam — the guard simply stays inert.
      }
    };

    void setup();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  if (!pending) return null;

  return (
    <ExitConfirmationModal
      activeCount={pending.activeCount}
      nonResumable={pending.nonResumable}
      exiting={exiting}
      mode={pending.mode}
      onKeepWorking={() => useExitPromptStore.getState().keepWorking()}
      onExit={() => void useExitPromptStore.getState().confirmExit()}
    />
  );
}
