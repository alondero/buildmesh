import { useSyncExternalStore } from 'react';
import { listen } from '@tauri-apps/api/event';
import { listProviders } from '../lib/tauri';
import { mapBackendProviders, type SpawnOption } from '../lib/groups';
import { PROVIDER_LIST_CHANGED_EVENT } from './useProviderListInvalidation';

/**
 * Issue #1502 — the single shared Spawn Option list.
 *
 * `Sidebar`, every `GridNodeHeader`, and any future spawn surface read the
 * same module-scope snapshot instead of each owning a `useState` + IPC
 * fetch + `provider-list-changed` subscription. A 3x3 grid mounts nine
 * headers; without this they would issue nine identical fetches and hold
 * nine identical event listeners. Here there is exactly one in-flight
 * fetch and one process-lifetime subscription no matter how many
 * components are mounted (the listener intentionally lives for the
 * process, mirroring `pathInvalidatedCache`'s `GIT_CHANGED` listener —
 * App Settings busts the `tauri.ts` cache on upsert/remove and emits the
 * event, so this hook drops its snapshot and reloads).
 *
 * Readers subscribe via `useSyncExternalStore` (tearing-safe, zero mount
 * re-renders) rather than a hand-rolled `useState` + `useEffect` pub-sub.
 *
 * A failed fetch resolves to `[]` (same contract as `loadSpawnOptions`
 * in `omnibarActions.ts`) so pickers simply disable instead of crashing
 * their surface.
 */
let cached: SpawnOption[] | null = null;
let inflight: Promise<SpawnOption[]> | null = null;
const subscribers = new Set<() => void>();
let listening = false;

const EMPTY: SpawnOption[] = [];

function publish(list: SpawnOption[]): void {
  cached = list;
  for (const notify of subscribers) notify();
}

function load(): void {
  if (inflight) return;
  const request = listProviders()
    .then((backend) => mapBackendProviders(backend ?? []))
    .catch(() => [] as SpawnOption[]);
  inflight = request;
  // Clear the single-flight slot on completion so a failed (or stale)
  // fetch never pins future callers to its result forever. The identity
  // check drops a superseded response: if an invalidation reload started
  // while this request was in flight, its rows lose and the newer fetch
  // owns the publish.
  void request.then((list) => {
    if (inflight === request) {
      inflight = null;
      publish(list);
    }
  });
}

function ensureListening(): void {
  if (listening) return;
  listening = true;
  void listen(PROVIDER_LIST_CHANGED_EVENT, () => {
    inflight = null;
    cached = null;
    load();
  });
}

function subscribe(onStoreChange: () => void): () => void {
  ensureListening();
  subscribers.add(onStoreChange);
  if (!cached && !inflight) load();
  return () => {
    subscribers.delete(onStoreChange);
  };
}

export function useProviderList(): SpawnOption[] {
  return useSyncExternalStore(subscribe, () => cached ?? EMPTY, () => EMPTY);
}

/** Test-only: reset the module snapshot between cases. Wired into
 *  `tests/setup/vitest.setup.ts` alongside the other cache resets, and
 *  callable directly when a test re-installs the `invoke` mock mid-file
 *  (same discipline as `__resetProviderCachesForTests`). */
export function __resetSharedProviderListForTests(): void {
  cached = null;
  inflight = null;
  subscribers.clear();
  listening = false;
}
