import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import type { NamingBackendFailedPayload } from '../types/generated/NamingBackendFailedPayload';

// Re-export so existing callers that imported `NamingBackendFailedPayload`
// from the hook file keep compiling — the canonical declaration is now the
// generated ts-rs binding (issue #359, issue #846).
export type { NamingBackendFailedPayload };

/**
 * Event name emitted by `session_naming::on_turn_with` when the LLM rename
 * has failed `MAX_RENAME_ATTEMPTS` times in a row for a single node — the
 * sticky lockout reached. Centralised so the Rust and TS halves stay in
 * sync (one grep-able symbol beats a stringly-typed literal duplicated
 * across files).
 */
export const NAMING_BACKEND_FAILED_EVENT = 'naming-backend-failed';

/**
 * Subscribes to the `naming-backend-failed` Tauri event and forwards the
 * payload to `onFailure`. The parent decides how to render — `App.tsx`
 * passes its local `addToast` so the existing toast primitive renders the
 * failure with the rest of the runtime-warning stream (Sync, Worktree,
 * Autopilot). Extracted as a hook so the listener contract is unit-tested
 * without mounting `App.tsx` — mirrors the `useProviderListInvalidation`
 * shape.
 *
 * Pass a stable reference (wrap with `useCallback` if your handler would
 * otherwise be a new closure on every render) so the listener isn't torn
 * down and re-attached on every parent re-render.
 */
export function useNamingBackendFailureToast(
  onFailure: (payload: NamingBackendFailedPayload) => void,
): void {
  useEffect(() => {
    const unlisten = listen<NamingBackendFailedPayload>(
      NAMING_BACKEND_FAILED_EVENT,
      (event) => {
        onFailure(event.payload);
      },
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [onFailure]);
}