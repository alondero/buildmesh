import { listen } from '@tauri-apps/api/event';
import { createStreamSubscription } from './streamSubscription';
import { setProviderListPromise } from '../providerCache';

/**
 * Owner of the `provider-list-changed` live stream (issue #2531).
 *
 * The provider catalogue is mutated from several places — App Settings
 * upsert/remove, a per-mesh default-provider write, a launch
 * configuration write, a preferences restore — and each mutation
 * notifies the open surfaces so they re-read the list. Before the
 * refactor three modules each held their own `listen` for the same
 * event and each re-implemented "the list changed, so refresh".
 *
 * This module owns the ONE `listen`, owns the event name (so the
 * constant and its subscriber are changed together), and publishes a
 * typed change to each consumer.
 *
 * The emitters import the constant from here — `lib/tauri/mesh.ts`
 * (per-mesh default write) and `lib/tauri/preferences.ts` (app-wide
 * default write) — so the producer and the consumer of the event name
 * import one symbol.
 */
export const PROVIDER_LIST_CHANGED_EVENT = 'provider-list-changed';

/**
 * The provider catalogue changed.
 *
 * The event carries a unit payload: the change says only that "the list
 * you are showing is stale".
 */
export interface ProviderListChange {
  readonly kind: 'provider-list-changed';
}

const PROVIDER_LIST_CHANGE: ProviderListChange = { kind: 'provider-list-changed' };

/**
 * Convert a raw `provider-list-changed` payload into a typed change.
 *
 * The backend emits a unit payload (`()`), so the TS side sees `null`;
 * the frontend's own `emit(name)` sends `undefined`. Both mean "the
 * list changed". Any other shape is not a payload this stream owns, so
 * it is dropped rather than waking every consumer with a change it
 * cannot describe.
 */
export function toProviderListChange(payload: unknown): ProviderListChange | null {
  if (payload === null || payload === undefined) return PROVIDER_LIST_CHANGE;
  return null;
}

const subscription = createStreamSubscription<ProviderListChange>(
  'providerListStream',
  async (notify) => {
    const unlisten = await listen<unknown>(PROVIDER_LIST_CHANGED_EVENT, ({ payload }) => {
      const change = toProviderListChange(payload);
      if (change !== null) {
        // Deferred backend discovery did not pass through a Settings wrapper
        // that would already have evicted the IPC promise cache.
        setProviderListPromise(null);
        notify(change);
      }
    });
    return unlisten;
  },
);

/**
 * Subscribe to provider-catalogue changes.
 *
 * Consumers that only need "something changed" can ignore the change
 * value; the typed payload exists so no consumer has to match the raw
 * event payload itself.
 */
export function subscribeProviderListChanges(
  listener: (change: ProviderListChange) => void,
): () => void {
  return subscription.subscribe(listener);
}

/** Test-only: drop every consumer and detach the backend subscription. */
export function __resetProviderListStreamForTests(): void {
  subscription.reset();
}
