/**
 * Path-invalidated cache primitive (issue #282).
 *
 * Replaces the four near-identical `listen(GIT_CHANGED)` + module-level
 * cache + in-flight dedupe + path-keyed subscription patterns that lived in
 * `useGitSummary`, `useOpenPr`, `useMeshHealth`, and `useMeshGitStatus`.
 *
 * Architecture
 * ------------
 * - ONE module-level `GIT_CHANGED` listener is installed the first time any
 *   client subscribes.
 * - Each `createPathInvalidatedCache` call returns a `QueryClient` tagged
 *   with a unique `clientId` (Symbol). The client registers a single
 *   bus-handler in a global map keyed by that id.
 * - Subscriptions are stored in `pathSubscribers` keyed by the watched path.
 *   Each subscriber carries its `clientId` and the `notify` callback to
 *   invoke when the bus reports a match.
 * - On a `GIT_CHANGED` event, the listener iterates `pathSubscribers` and
 *   uses `pathMatchesGitEvent` to find matches. For each matched subscriber
 *   it calls **that subscriber's owning client's handler** (via
 *   `busHandlers.get(sub.clientId)`), not a global "invalidate every key in
 *   every client" sweep.
 *
 * Why the clientId-scoped dispatch matters (Footgun 1)
 * ----------------------------------------------------
 * Two clients (e.g. useOpenPr keys by nodeId, useMeshHealth keys by meshId)
 * can both have a subscriber whose key is `7`. A global sweep that wiped
 * "every cache entry where key === 7" would nuke the wrong client on every
 * event. Scoping dispatch to the owning client (via `clientId`) keeps the
 * invalidation local.
 *
 * `null` vs `undefined` cache reads
 * ---------------------------------
 * The cache uses a `Symbol` sentinel (`HAS_NULL`) to distinguish "no entry"
 * from "entry whose value is `null`". Most callers treat `null` as a real
 * state ("no open PR", "no files changed", etc.), so we can't rely on
 * `Map.get` returning `undefined` to mean "uncached".
 */

import { listen } from '@tauri-apps/api/event';
import { GIT_CHANGED } from './events';
import { pathMatchesGitEvent } from './paths';

const HAS_NULL = Symbol('pathInvalidatedCache.has-null');

export interface PathInvalidatedCacheOptions<K, V> {
  /** Loads the value for a given key. Resolves to `null` for "known empty". */
  fetcher: (key: K) => Promise<V | null>;
  /** Tag used in the dev-console warning when `fetcher` throws. */
  name?: string;
}

export interface QueryClient<K, V> {
  /**
   * Returns the cached value for `key`, or `undefined` if no entry exists.
   * A cached `null` is returned as `null` (not `undefined`); callers that
   * need to distinguish "uncached" from "cached null" should check the
   * return value directly.
   */
  read(key: K): V | null | undefined;
  /**
   * Fetches the value for `key`, deduping with any concurrent caller. On
   * rejection, the cache is left untouched and the in-flight is cleared; the
   * returned promise resolves to `null` so callers can `await` without
   * try/catch but should still treat `null` from `refresh` as "no data".
   */
  refresh(key: K): Promise<V | null>;
  /** Erases the cached value AND any in-flight fetch for `key`. */
  invalidate(key: K): void;
  /**
   * Registers a callback to be invoked when a `GIT_CHANGED` event matches
   * `path` (via `pathMatchesGitEvent` — worktree-subdir + WUNC-aware). The
   * returned function unsubscribes; call it from the hook's cleanup.
   *
   * The callback fires AFTER the client's own cache entry for `key` has
   * been evicted, so a subsequent `client.read(key)` returns `undefined`
   * and a subsequent `client.refresh(key)` hits the backend (or dedups
   * onto a sibling subscriber's in-flight fetch).
   */
  subscribe(key: K, path: string, onInvalidate: () => void): () => void;
}

interface PathSubscriber<K = unknown> {
  clientId: symbol;
  key: K;
  notify: () => void;
}

type BusHandler = (sub: PathSubscriber<unknown>) => void;

// Module-level globals — there is exactly one bus per process, regardless
// of how many clients/subscribers exist.
const pathSubscribers = new Map<string, Set<PathSubscriber>>();
const busHandlers = new Map<symbol, BusHandler>();
let listenerInstalled = false;

function installListener(): void {
  if (listenerInstalled) return;
  listenerInstalled = true;

  // We deliberately don't await the unlisten handle — this listener lives
  // for the whole process. Mirrors the pattern in the four original hooks.
  void listen(GIT_CHANGED, (event) => {
    const payload = event.payload as { path: string; internal_path?: string };
    for (const [path, subs] of pathSubscribers) {
      if (!pathMatchesGitEvent(payload, path)) continue;
      // Invalidate the owning client's cache for this key, then notify the
      // subscriber. The first notify for a given (client, key) starts a
      // fetch that subsequent sibling subscribers dedup onto.
      for (const sub of subs) {
        const handler = busHandlers.get(sub.clientId);
        if (handler) handler(sub);
      }
    }
  });
}

/**
 * Test-only: clears all module-level state so each test can re-import and
 * start with a clean bus. The hook tests also use `vi.resetModules()` so
 * the freshly-imported module's `createPathInvalidatedCache` call gets a
 * fresh `busHandlers` entry on top of this cleared global state.
 */
export function resetPathInvalidatedCacheForTests(): void {
  pathSubscribers.clear();
  busHandlers.clear();
  listenerInstalled = false;
}

export function createPathInvalidatedCache<K, V>(
  options: PathInvalidatedCacheOptions<K, V>,
): QueryClient<K, V> {
  const { fetcher, name = 'pathInvalidatedCache' } = options;

  // Per-client state. A `Symbol` clientId is the load-bearing piece that
  // makes the cross-client dispatch scoping work — see module docstring.
  const clientId = Symbol('pathInvalidatedCache');
  const cache = new Map<K, V | typeof HAS_NULL>();
  const pending = new Map<K, Promise<V | null>>();

  // The bus calls this for every matched subscriber of THIS client. It
  // evicts the cache (so the next read returns `undefined`) and clears the
  // in-flight (so the next refresh starts fresh instead of resolving to a
  // stale value), then calls the subscriber's notify.
  const handler: BusHandler = (sub) => {
    const k = sub.key as K;
    cache.delete(k);
    pending.delete(k);
    sub.notify();
  };
  // Register the handler at creation AND re-register on every `subscribe`
  // (idempotently). The re-registration handles the test case where
  // `resetPathInvalidatedCacheForTests()` cleared `busHandlers` between
  // mounts; without it, an emit that fires after the reset would
  // silently no-op because `busHandlers.get(sub.clientId)` is undefined.
  busHandlers.set(clientId, handler);

  return {
    read(key) {
      if (!cache.has(key)) return undefined;
      const entry = cache.get(key);
      return entry === HAS_NULL ? null : (entry as V);
    },

    refresh(key) {
      const inFlight = pending.get(key);
      if (inFlight) return inFlight;

      const p = fetcher(key)
        .then((result) => {
          cache.set(key, result === null ? HAS_NULL : result);
          pending.delete(key);
          return result;
        })
        .catch((err) => {
          pending.delete(key);
          // eslint-disable-next-line no-console
          console.warn(`${name}: fetch failed for key`, key, err);
          return null;
        });
      pending.set(key, p);
      return p;
    },

    invalidate(key) {
      cache.delete(key);
      pending.delete(key);
    },

    subscribe(key, path, onInvalidate) {
      // Idempotent re-registration of the bus handler — see the comment
      // at the factory-level `busHandlers.set` call above for why.
      busHandlers.set(clientId, handler);
      installListener();
      let set = pathSubscribers.get(path);
      if (!set) {
        set = new Set();
        pathSubscribers.set(path, set);
      }
      const sub: PathSubscriber<K> = { clientId, key, notify: onInvalidate };
      set.add(sub as PathSubscriber);
      return () => {
        const live = pathSubscribers.get(path);
        if (!live) return;
        live.delete(sub as PathSubscriber);
        if (live.size === 0) pathSubscribers.delete(path);
      };
    },
  };
}
