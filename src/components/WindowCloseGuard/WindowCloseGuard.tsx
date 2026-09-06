import { useEffect, useRef, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import * as api from '../../lib/tauri';
import {
  buildSupportsResumeMap,
  exitNodeProviderDisplay,
  getActiveExitNodes,
  partitionExitNodes,
  shouldConfirmExit,
} from '../../lib/exitGuard';
import { ExitConfirmationModal, type ExitNonResumableEntry } from '../ExitConfirmationModal/ExitConfirmationModal';
import type { AgentNode } from '../../types/generated/AgentNode';
import type { ProviderInfo } from '../../types/generated/ProviderInfo';

interface ExitSnapshot {
  active: AgentNode[];
  nonResumable: ExitNonResumableEntry[];
}

/**
 * Window close guard (issue #1501).
 *
 * Intercepts Tauri `onCloseRequested`: when active agent sessions exist
 * (`running`, `awaiting_input`, `spawning`, `ready`) and the
 * `confirm_before_quit` preference is on, the close is vetoed and the
 * exit-confirmation modal is shown instead. Confirming destroys the
 * window (bypasses `closeRequested`); the backend `ExitRequested` sweep
 * then marks sessions suspended and kills processes.
 *
 * Mounted unconditionally in `App.tsx` — the Tauri listener is the only
 * always-on part; the `<Modal>` (and its Escape listener) mounts only
 * while the confirmation is showing, so Escape is never stolen from
 * agent terminals.
 */
export function WindowCloseGuard() {
  const [snapshot, setSnapshot] = useState<ExitSnapshot | null>(null);
  const [exiting, setExiting] = useState(false);
  // Set once the user confirms — `destroy()` bypasses `closeRequested`,
  // so this is purely defensive against a second event racing the destroy.
  const confirmedRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;

    const setup = async () => {
      try {
        const win = getCurrentWindow();
        if (typeof win.onCloseRequested !== 'function') return;
        unlisten = await win.onCloseRequested(async (event) => {
          if (confirmedRef.current) return;
          // Sync fast path first: the store read is synchronous, so an
          // idle app never touches IPC and never races the close. Any
          // close that MIGHT need a prompt is vetoed synchronously here,
          // before the first await — awaiting `getAppPreferences` before
          // `preventDefault` would let the window close before the veto
          // arrives (code review, issue #1501).
          const nodes = useAgentNodeStore.getState().getAgentNodes();
          const active = getActiveExitNodes(nodes);
          if (active.length === 0) return;
          event.preventDefault();
          // Fail-closed on preference read: an unreadable preference must
          // prompt rather than silently drop running sessions.
          let confirmBeforeQuit = true;
          try {
            const prefs = await api.getAppPreferences();
            confirmBeforeQuit = prefs.confirm_before_quit ?? true;
          } catch {
            confirmBeforeQuit = true;
          }
          if (!shouldConfirmExit(active, confirmBeforeQuit)) {
            // User opted out: continue the close we just vetoed. `destroy`
            // bypasses `closeRequested` (no second prompt); the backend
            // `ExitRequested` sweep still marks sessions suspended and
            // kills processes.
            try {
              await getCurrentWindow().destroy();
            } catch {
              // Non-fatal (e.g. mocked window) — leave the app open.
            }
            return;
          }
          // Fail-closed on provider list: unknown harnesses partition as
          // non-resumable so the modal warns instead of staying silent.
          let providers: ProviderInfo[] = [];
          try {
            providers = await api.listProviders();
          } catch {
            providers = [];
          }
          if (cancelled) return;
          const supportsMap = buildSupportsResumeMap(providers);
          const { nonResumable } = partitionExitNodes(active, supportsMap);
          setSnapshot({
            active,
            nonResumable: nonResumable.map((n) => ({
              id: n.id,
              name: n.name,
              providerDisplay: exitNodeProviderDisplay(n, providers),
            })),
          });
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

  if (!snapshot) return null;

  const handleKeepWorking = () => {
    setSnapshot(null);
  };

  const handleExit = async () => {
    confirmedRef.current = true;
    setExiting(true);
    try {
      await getCurrentWindow().destroy();
    } catch {
      // A failed destroy (e.g. mocked window in tests) must not strand
      // the modal in a busy state — reset so the user can retry.
      confirmedRef.current = false;
      setExiting(false);
    }
  };

  return (
    <ExitConfirmationModal
      activeCount={snapshot.active.length}
      nonResumable={snapshot.nonResumable}
      exiting={exiting}
      onKeepWorking={handleKeepWorking}
      onExit={handleExit}
    />
  );
}
