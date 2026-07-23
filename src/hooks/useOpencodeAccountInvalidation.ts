import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';

/**
 * Event name emitted by `commands::opencode_oauth::{persist_opencode_tokens,
 * revoke_opencode_console, set_opencode_console_workspace}` after a successful
 * credential change. Centralised here so the Rust and TS halves stay in
 * sync (one grep-able symbol beats five stringly-typed listeners).
 *
 * Mirrors the canonical `provider-list-changed` pattern (see
 * `useProviderListInvalidation`).
 */
export const OPENCODE_CONSOLE_CHANGED_EVENT = 'opencode-console-changed';

/**
 * Subscribes to the `opencode-console-changed` Tauri event and calls
 * `refresh` whenever any of the OpenCode OAuth commands complete
 * successfully (sign-in, sign-out, workspace switch). Used by the
 * Usage tab so a credential change immediately re-fetches the live
 * `_server billing.get` probe with `force=true` rather than waiting
 * for the 5-minute cache TTL to lapse — without this, a freshly
 * signed-in user sees stale (or empty) usage for up to 5 minutes
 * after the dance.
 *
 * Pass a stable reference (wrap with `useCallback` if your refresh
 * function would otherwise be a new closure on every render) so the
 * listener isn't torn down and re-attached on every parent re-render.
 */
export function useOpencodeAccountInvalidation(refresh: () => void): void {
  useEffect(() => {
    const unlisten = listen(OPENCODE_CONSOLE_CHANGED_EVENT, () => {
      refresh();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [refresh]);
}
