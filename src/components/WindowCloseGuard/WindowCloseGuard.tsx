import { useEffect, useRef } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useExitPromptStore } from '../../stores/exitPromptStore';
import * as api from '../../lib/tauri';
import {
  buildSupportsResumeMap,
  exitNodeProviderDisplay,
  getActiveExitNodes,
  partitionExitNodes,
  shouldConfirmExit,
} from '../../lib/exitGuard';
import { ExitConfirmationModal } from '../ExitConfirmationModal/ExitConfirmationModal';
import type { ProviderInfo } from '../../types/generated/ProviderInfo';

/**
 * Window close guard (issue #1501).
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
 * Prompt state lives in `useExitPromptStore` (the same decoupled-store
 * pattern as `WorktreeCloseDialog` + `useWorktreeClosePromptStore`), so
 * the veto decision is a synchronous store read — no cold IPC on the
 * close path, no fail-into-modal when IPC struggles, no concurrent
 * double-close race. The provider list read stays async (it rides the
 * shared `listProviders` cache), but only runs *after* the synchronous
 * veto. Mounted in every `App` branch (boot splash, boot error, ready)
 * so the listener is armed from first paint.
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
          const nodes = useAgentNodeStore.getState().getAgentNodes();
          const active = getActiveExitNodes(nodes);
          if (!shouldConfirmExit(active, store.confirmBeforeQuit)) return;
          event.preventDefault();
          fetchingRef.current = true;
          // Fail-closed on provider list: unknown harnesses partition as
          // non-resumable so the modal warns instead of staying silent.
          let providers: ProviderInfo[] = [];
          try {
            providers = await api.listProviders();
          } catch {
            providers = [];
          }
          if (cancelled) {
            fetchingRef.current = false;
            return;
          }
          fetchingRef.current = false;
          const supportsMap = buildSupportsResumeMap(providers);
          const { nonResumable } = partitionExitNodes(active, supportsMap);
          useExitPromptStore.getState().showExitPrompt(
            active.length,
            nonResumable.map((n) => ({
              id: n.id,
              name: n.name,
              providerDisplay: exitNodeProviderDisplay(n, providers),
            })),
          );
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
      onKeepWorking={() => useExitPromptStore.getState().keepWorking()}
      onExit={() => void useExitPromptStore.getState().confirmExit()}
    />
  );
}
