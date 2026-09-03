import { useEffect, useState } from 'react';
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
 * A failed fetch resolves to `[]` (same contract as `loadSpawnOptions`
 * in `omnibarActions.ts`) so pickers simply disable instead of crashing
 * their surface.
 */
let cached: SpawnOption[] | null = null;
let inflight: Promise<SpawnOption[]> | null = null;
const subscribers = new Set<(list: SpawnOption[]) => void>();
let listening = false;

function publish(list: SpawnOption[]): void {
  cached = list;
  for (const notify of subscribers) notify(list);
}

function load(): void {
  if (!inflight) {
    inflight = listProviders()
      .then((backend) => mapBackendProviders(backend ?? []))
      .catch(() => [] as SpawnOption[]);
    // Fan out to every mounted reader. React bails out when the
    // reference is unchanged, so re-publishing the same array is free.
    inflight.then(publish);
  }
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

export function useProviderList(): SpawnOption[] {
  const [list, setList] = useState<SpawnOption[]>(() => cached ?? []);
  useEffect(() => {
    ensureListening();
    subscribers.add(setList);
    if (cached) {
      setList(cached);
    } else {
      // `load` publishes to all subscribers on resolve, which includes
      // this `setList` — no per-mount `.then` needed, and unmounting
      // before resolve is safe (the subscriber is removed below, and
      // React 18+ no longer warns on post-unmount sets anyway).
      load();
    }
    return () => {
      subscribers.delete(setList);
    };
  }, []);
  return list;
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
