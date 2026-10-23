import { useEffect } from 'react';
import {
  PROVIDER_LIST_CHANGED_EVENT,
  subscribeProviderListChanges,
} from '../lib/streams/providerListStream';

// Re-exported so existing importers keep working — the constant itself is
// owned by `lib/streams/providerListStream` (the module that subscribes to
// the event), and the emitters in `lib/tauri/mesh.ts` and
// `lib/tauri/preferences.ts` import the same symbol from there.
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
 * The event has exactly one subscriber in the app (issue #2531; proved by
 * `tests/unit/live-event-stream-ownership.test.ts`): the owner module
 * `lib/streams/providerListStream`, which publishes typed changes to
 * each consumer. This hook is one of them.
 *
 * Pass a stable reference (wrap with `useCallback` if your refresh function
 * would otherwise be a new closure on every render) so the listener isn't
 * torn down and re-attached on every parent re-render.
 */
export function useProviderListInvalidation(refresh: () => void): void {
  useEffect(() => subscribeProviderListChanges(() => {
    refresh();
  }), [refresh]);
}
