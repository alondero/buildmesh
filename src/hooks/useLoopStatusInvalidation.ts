import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';

/**
 * Tauri events that imply the Looping Autopilot status may have moved
 * (issue #1751). The backend emits no dedicated `loop-status-changed`
 * event — the poller (`services::autopilot`) derives status from the
 * `autopilot_enabled` flag plus the loop-iteration ledger — so the tab
 * refreshes on the lifecycle transitions that move that ledger:
 * spawns and submissions start iterations, finishing/PR/failed/closed
 * transitions end them, and `agent-lifecycle` covers every mark
 * transition in between. Each name below is subscribed verbatim by a
 * distinct backend emit site or an existing frontend listener
 * (`agentNodeListeners.ts`, `App.tsx`), so a rename on either side is
 * caught by the event-name test in
 * `tests/unit/useLoopStatusInvalidation.test.tsx`.
 */
export const LOOP_STATUS_INVALIDATION_EVENTS = [
  'agent-lifecycle',
  'node-created',
  'node-spawn-completed',
  'node-spawn-failed',
  'autopilot-submitted',
  'autopilot-blocked',
  'autopilot-finishing',
  'autopilot-pr-created',
  'autopilot-finish-failed',
  'autopilot-node-closed',
] as const;

/**
 * Stale-while-revalidate fallback for the loop-status poll (issue
 * #1751). Event-driven refresh is the live path; this interval only
 * covers a dropped Tauri event so the badge never strands for longer
 * than 45s. It must never gate or overwrite the live path — both the
 * event handler and this fallback funnel through the same
 * mesh-switch-guarded refresh, so whichever fires last commits the
 * freshest read (issue #1073 pattern).
 */
export const LOOP_STATUS_FALLBACK_MS = 45_000;

/** Coalescing window for event bursts (mirrors `CircuitsProbeTab`'s
 *  `circuit-run-updated` debounce): a spawn storm (created +
 *  spawn-completed + lifecycle) schedules one refresh, not three. */
const INVALIDATION_DEBOUNCE_MS = 200;

/**
 * Subscribes to every `LOOP_STATUS_INVALIDATION_EVENTS` Tauri event and
 * calls `refresh` (debounced) whenever one fires. Follows the
 * `useProviderListInvalidation(refresh)` pattern: pass a stable
 * reference (wrap with `useCallback`) so the listeners aren't torn
 * down and re-attached on every parent re-render.
 */
export function useLoopStatusInvalidation(refresh: () => void): void {
  useEffect(() => {
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const schedule = () => {
      if (disposed) return;
      if (timer !== null) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        if (!disposed) refresh();
      }, INVALIDATION_DEBOUNCE_MS);
    };
    const unlistens = Promise.all(
      LOOP_STATUS_INVALIDATION_EVENTS.map((name) => listen(name, schedule)),
    );
    return () => {
      disposed = true;
      if (timer !== null) clearTimeout(timer);
      void unlistens.then((fns) => {
        for (const fn of fns) fn();
      });
    };
  }, [refresh]);
}
