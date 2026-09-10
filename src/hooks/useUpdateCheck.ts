// Thin React adapter over `useUpdaterStore` (issue #1526 + #1654 review).
//
// The state machine lives in the shared Zustand store so both
// `UpdatePrompt` and Settings > About subscribe to one source of truth
// (finding 1). This hook owns three thin responsibilities:
//   - fire the one-time boot quiet check (idempotent across surfaces),
//   - fire the one-time `updaterEnabled()` probe so Settings knows
//     whether to render the button or a disabled notice (finding 3),
//   - subscribe to the current `phase` for rendering.
//
// Action closures (`check`, `install`, …) come straight from the
// store's `getState()` so they don't re-bind on every render — they
// don't need to.

import { useEffect } from 'react';
import { useUpdaterStore, type UpdateCheckApi } from '../stores/updaterStore';

export type { UpdatePhase } from '../stores/updaterStore';

export function useUpdateCheck(): UpdateCheckApi {
  const state = useUpdaterStore((s) => s.phase);
  const enabled = useUpdaterStore((s) => s.enabled);

  // Fire the boot-time probes exactly once across every component that
  // mounts this hook. The store guards re-entry with module-level
  // flags, so React StrictMode's double-invoke (and any second
  // surface mounting its own copy of the hook) is a no-op.
  useEffect(() => {
    void useUpdaterStore.getState().startEnabledCheck();
    void useUpdaterStore.getState().startQuietCheck();
  }, []);

  // Bind action closures from the live store so callers always get the
  // latest implementations (the store re-creates them on Zustand
  // re-renders, but the public surface is stable across renders).
  const api = useUpdaterStore.getState();

  return {
    state,
    enabled: enabled ?? false,
    check: api.check,
    install: api.install,
    retry: api.retry,
    restart: api.restart,
    cancelInstall: api.cancelInstall,
    dismiss: api.dismiss,
  };
}