import { useEffect, useSyncExternalStore } from 'react';
import { listHarnessLogins, type HarnessLoginStatus } from '../lib/tauri/provider';
import { subscribeProviderListChanges } from '../lib/streams/providerListStream';

/**
 * The shared Harness Login list (the default Codex login first, then each
 * additional one).
 *
 * Every spawn menu reads one module-scope snapshot, like `useProviderList`:
 * one fetch in flight and one `provider-list-changed` subscription however
 * many menus are mounted. The login commands announce their changes through
 * that event, so the snapshot reloads after an add, rename or removal.
 *
 * A failed fetch publishes `[]`, which hides the login submenu rather than
 * blocking the spawn menu.
 */
let cached: HarnessLoginStatus[] | null = null;
let inflight: Promise<void> | null = null;
let stopListening: (() => void) | null = null;
const subscribers = new Set<() => void>();
const EMPTY: HarnessLoginStatus[] = [];

/** Replace the snapshot, e.g. with a list the settings pane just polled. */
export function publishHarnessLogins(list: HarnessLoginStatus[]): void {
  cached = list;
  for (const notify of subscribers) notify();
}

function load(): void {
  if (inflight) return;
  // `Promise.resolve().then` turns a synchronous throw into a rejection, so a
  // missing command can only ever hide the submenu.
  const request = Promise.resolve()
    .then(() => listHarnessLogins())
    .then((list) => (Array.isArray(list) ? list : []))
    .catch(() => [] as HarnessLoginStatus[])
    .then((list) => {
      if (inflight === request) {
        inflight = null;
        publishHarnessLogins(list);
      }
    });
  inflight = request;
}

function ensureListening(): void {
  if (stopListening) return;
  stopListening = subscribeProviderListChanges(() => {
    inflight = null;
    load();
  });
}

function subscribe(notify: () => void): () => void {
  subscribers.add(notify);
  return () => { subscribers.delete(notify); };
}

/**
 * Codex logins for the spawn menu. `enabled: false` skips the fetch for
 * surfaces that never offer a login choice.
 */
export function useHarnessLogins(enabled = true): HarnessLoginStatus[] {
  useEffect(() => {
    if (!enabled) return;
    ensureListening();
    if (cached === null) load();
  }, [enabled]);
  const snapshot = useSyncExternalStore(subscribe, () => cached ?? EMPTY);
  return enabled ? snapshot : EMPTY;
}

/** Test-only: forget the snapshot between cases. */
export function __resetHarnessLoginsForTests(): void {
  cached = null;
  inflight = null;
  stopListening?.();
  stopListening = null;
  subscribers.clear();
}
