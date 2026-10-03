import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { PROVIDER_LIST_CHANGED_EVENT } from '../lib/tauri/provider';

// Re-exported so existing importers keep working — the constant itself is
// owned by `lib/tauri/provider` (one grep-able symbol for the Rust and TS
// halves; lib-ward import direction, never hooks-ward).
export { PROVIDER_LIST_CHANGED_EVENT };

/**
 * Subscribes to the `provider-list-changed` Tauri event and calls `refresh`
 * whenever the provider list is mutated from anywhere — App Settings upsert
 * or remove, or any future external trigger.
 *
 * The tauri.ts `listProviders` cache is also busted in the JS wrapper, but
 * that only helps callers within the same component (the same call chain).
 * Components that snapshot the provider list into local state on mount
 * (Sidebar spawn menu, Probe tabs that render provider pickers) need an
 * explicit cross-component signal to re-read — this hook provides it.
 *
 * Pass a stable reference (wrap with `useCallback` if your refresh function
 * would otherwise be a new closure on every render) so the listener isn't
 * torn down and re-attached on every parent re-render.
 */
export function useProviderListInvalidation(refresh: () => void): void {
  useEffect(() => {
    const unlisten = listen(PROVIDER_LIST_CHANGED_EVENT, () => {
      refresh();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [refresh]);
}
