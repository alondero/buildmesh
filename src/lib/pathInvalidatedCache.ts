/**
 * Path-invalidated cache primitive (issue #282), split into two factories
 * (issue #347):
 *
 * - `createPathKeyedCache<V>({fetcher, name})` — for hooks whose key IS the
 *   git path: `useGitSummary`, `useMeshGitStatus`, `useGitBranchStatus`,
 *   `useChangedFiles`. The `subscribe(key, cb)` method takes no separate
 *   `path` arg because the key and the path are the same string.
 *
 * - `createDualKeyCache<K, V>({fetcher, name})` — for hooks whose key is
 *   an entity id (nodeId, meshId) but whose GIT_CHANGED subscription path
 *   is a different string: `useOpenPr`, `useMeshHealth`. The
 *   `subscribeByPath(key, path, cb)` method makes the dual-key shape
 *   explicit at the type level.
 *
 * The single-factory `createPathInvalidatedCache` that lived here before
 * issue #347 forced every caller to write
 * `usePathInvalidatedQuery(client, path, path)` — the duplicated `path`
 * arg was a smell. The split makes the two shapes first-class; a reader
 * of `usePathInvalidatedQuery(client, gitPath)` vs
 * `usePathInvalidatedQuery(client, nodeId, gitPath)` sees immediately
 * whether the hook treats its key as a path or as an id.
 *
 * Also exposes a component-level subscription API (`subscribeGitPathInvalidation`,
 * issue #345) so React components that only need the invalidation callback
 * (no key, no cache) can share the same bus + `pathMatchesGitEvent` plumbing
 * without hand-rolling their own `listen(GIT_CHANGED, ...)` + cleanup pair.
 * Used by `AgentReviewPanel` and `CenterDiffOverlay`.
 *
 * Architecture
 * ------------
 * - ONE module-level `GIT_CHANGED` listener is installed the first time any
 *   client subscribes.
 * - Each factory call returns a `QueryClient` (one of the two flavours)
 *   tagged with a unique `clientId` (Symbol). The client registers a single
 *   bus-handler in a global map keyed by that id.
 * - Subscriptions are stored in `pathSubscribers` keyed by the watched path.
 *   Each subscriber carries its `clientId` and the `notify` callback to
 *   invoke when the bus reports a match.
 * - On a `GIT_CHANGED` event, the listener iterates `pathSubscribers` and
 *   uses `pathMatchesGitEvent` to find matches. For each matched subscriber
 *   it calls **that subscriber's owning client's handler** (via the
 *   per-variant handler maps — `keyedBusHandlers.get(sub.clientId)` or
 *   `callbackBusHandlers.get(sub.clientId)` — keyed on the `kind`
 *   discriminator), not a global "invalidate every key in every client"
 *   sweep.
 * - The callback-only subscribers from `subscribeGitPathInvalidation` share
 *   a single `NOOP_CLIENT_ID` + stateless handler (`sub.notify()`), so
 *   they ride the same dispatch without per-callback entries in
 *   `callbackBusHandlers`.
 *
 * Why the clientId-scoped dispatch matters (Footgun 1)
 * ----------------------------------------------------
 * Two clients (e.g. a useOpenPr client keyed by nodeId and a useMeshHealth
 * client keyed by meshId) can both have a subscriber whose key is `7`. A
 * global sweep that wiped "every cache entry where key === 7" would nuke
 * the wrong client on every event. Scoping dispatch to the owning client
 * (via `clientId`) keeps the invalidation local.
 *
 * `null` vs `undefined` cache reads
 * ---------------------------------
 * Presence and value are tracked in two separate Maps (issue #346): `known`
 * (`Map<K, true>`) records whether a key has EVER been cached — the boolean
 * value is irrelevant, only `.has(key)` matters — while `values` (`Map<K,
 * V>`) holds the actual cached value, and simply has no entry for a key
 * whose cached value is `null`. `read` checks `known.has(key)` first (an
 * absent entry there means "uncached", i.e. `undefined`); if present, it
 * falls back to `values.get(key) ?? null` to normalize the "known but no
 * value" case to `null`. This avoids a `Symbol`-sentinel dance in one Map:
 * most callers treat `null` as a real state ("no open PR", "no files
 * changed", etc.), so we can't rely on `Map.get` returning `undefined` to
 * mean "uncached".
 */

import { listen } from '@tauri-apps/api/event';
import { GIT_CHANGED } from './events';
import { pathMatchesGitEvent } from './paths';

// Single shared clientId for all "callback-only" subscribers (see
// `subscribeGitPathInvalidation` below). The handler registered for it is
// stateless — it just calls `sub.notify()` — so one entry in
// `callbackBusHandlers` serves every such caller. Generating a fresh
// symbol per call would be functionally equivalent but would bloat
// `callbackBusHandlers` for no benefit.
const NOOP_CLIENT_ID = Symbol('subscribeGitPathInvalidation');
// Stateless: dispatches to whichever subscriber the bus matched, regardless
// of which `subscribeGitPathInvalidation` call added it. Hoisted to a const
// so we register the SAME function reference in `callbackBusHandlers` on
// every call (no fresh arrow allocation per component mount).
const NOOP_HANDLER: CallbackBusHandler = (sub) => sub.notify();

// ------------------------------------------------------------------
// Public client interfaces
// ------------------------------------------------------------------

/** Single-key client: `key` is a string that doubles as the GIT_CHANGED
 * subscription path. `subscribe(key, cb)` has no separate `path` arg. */
export interface PathKeyedClient<V> {
  /** Returns the cached value for `key`, or `undefined` if no entry exists.
   * A cached `null` is returned as `null` (not `undefined`); callers that
   * need to distinguish "uncached" from "cached null" should check the
   * return value directly. */
  read(key: string): V | null | undefined;
  /** Fetches the value for `key`, deduping with any concurrent caller. On
   * rejection, the cache is left untouched and the in-flight is cleared; the
   * returned promise resolves to `null` so callers can `await` without
   * try/catch but should still treat `null` from `refresh` as "no data".
   *
   * The thrown error is recorded per key — see `lastError(key)` — so callers
   * that need to disambiguate "fetch failed" from "fetcher returned null"
   * can read it after the promise resolves. Issue #342. */
  refresh(key: string): Promise<V | null>;
  /** Returns the most recent error caught by `refresh(key)`, or `null` if
   * the last refresh succeeded (or no refresh has run yet). The slot is
   * per-key: a failure for `keyA` does not leak to `keyB`, and a
   * subsequent successful refresh of `keyA` clears it. Issue #342. */
  lastError(key: string): Error | null;
  /** Erases the cached value AND any in-flight fetch for `key`. */
  invalidate(key: string): void;
  /** Registers a callback to be invoked when a `GIT_CHANGED` event matches
   * `key` (via `pathMatchesGitEvent` — worktree-subdir + WUNC-aware). The
   * returned function unsubscribes; call it from the hook's cleanup. */
  subscribe(key: string, onInvalidate: () => void): () => void;
  /**
   * Programmatically forces every subscriber of `path` to re-fetch,
   * bypassing the `minRefetchIntervalMs` freshness window. Mirrors the
   * bus listener's per-path dispatch but skips the freshness check (a
   * deliberate invalidation from the same process is always
   * authoritative). Drops the cache + freshness stamp per keyed
   * subscriber first so the subscriber's `onInvalidate` sees the same
   * "evicted, please refetch" precondition the bus path establishes,
   * then calls the per-subscriber `notify` callback — hook subscribers
   * re-fetch through `client.refresh(key)` exactly as they would on a
   * real `GIT_CHANGED` event.
   *
   * Scoping mirrors the bus dispatch one-for-one:
   *   - Callback-only subscribers (from `subscribeGitPathInvalidation`,
   *     registered with the shared `NOOP_CLIENT_ID`) carry no key
   *     and no client state — always fire `notify`.
   *   - Keyed subscribers fire only when their `clientId` matches
   *     THIS client, so sibling cache clients on the same path are
   *     untouched (footgun 1 in the module docstring).
   * No-op when `path` has no subscribers. Issue #780 — the buildmesh
   * create/merge PR flows use this to update the Open PR chip in
   * GridNodeHeader immediately instead of waiting up to
   * `minRefetchIntervalMs` for the bus-driven trailing refetch.
   */
  notifyByPath(path: string): void;
}

/** Dual-key client: `key` is an entity id; `path` is the separate git path
 * the GIT_CHANGED subscription should match. The `subscribeByPath(key, path,
 * cb)` method makes the dual shape explicit at the type level. */
export interface DualKeyClient<K, V> {
  /** Returns the cached value for `key`, or `undefined` if no entry exists.
   * A cached `null` is returned as `null` (not `undefined`). */
  read(key: K): V | null | undefined;
  /** Fetches the value for `key`, deduping with any concurrent caller. On
   * rejection, the cache is left untouched and the in-flight is cleared.
   * The thrown error is recorded per key — see `lastError(key)`. Issue #342. */
  refresh(key: K): Promise<V | null>;
  /** Returns the most recent error caught by `refresh(key)`, or `null` if
   * the last refresh succeeded. Per-key; a subsequent successful refresh
   * of the same key clears it. Issue #342. */
  lastError(key: K): Error | null;
  /** Erases the cached value AND any in-flight fetch for `key`. */
  invalidate(key: K): void;
  /** Registers a callback to be invoked when a `GIT_CHANGED` event matches
   * `path` (via `pathMatchesGitEvent`). The returned function unsubscribes. */
  subscribeByPath(key: K, path: string, onInvalidate: () => void): () => void;
  /**
   * Same as [`PathKeyedClient.notifyByPath`] but for the dual-key shape —
   * drops the cache + freshness stamp for each keyed subscriber whose
   * `clientId` matches this client, then fires the subscriber's `notify`.
   * Callback-only subscribers (`subscribeGitPathInvalidation` callers) on
   * the same path also fire (they have no cache to evict). See
   * `PathKeyedClient.notifyByPath` for the full contract. Issue #780.
   */
  notifyByPath(path: string): void;
}

/** Options accepted by both factories. */
export interface PathInvalidatedCacheOptions<K, V> {
  /** Loads the value for a given key. Resolves to `null` for "known empty". */
  fetcher: (key: K) => Promise<V | null>;
  /** Tag used in the dev-console warning when `fetcher` throws. */
  name?: string;
  /**
   * Freshness window for bus-driven invalidation, in milliseconds. While a
   * key's last successful fetch is younger than this, a matching
   * `GIT_CHANGED` event neither evicts the cache nor notifies subscribers
   * immediately — the cached value stays authoritative. Instead, ONE
   * trailing evict+notify is scheduled for the moment the window expires,
   * so the settled on-disk state always lands (a burst of suppressed
   * events collapses into a single deferred refetch rather than being
   * silently dropped — the panel can never go permanently stale).
   *
   * Use for expensive fetchers: `useOpenPr`'s live GitHub request, and the
   * git status/summary/branch/health walks — every agent file-write fires
   * `GIT_CHANGED` (up to ~2/s per watched node via the backend coalescer),
   * and refetching each of several full-repo status walks at that rate is
   * the steady-state load that makes the app feel sluggish while agents
   * stream edits. Manual `invalidate()`/`refresh()` are NOT gated — an
   * explicit user refresh always wins (and cancels any pending trailing
   * refetch). Defaults to `0` (evict on every matching event, the
   * original behaviour).
   */
  minRefetchIntervalMs?: number;
}

// ------------------------------------------------------------------
// Subscriber + handler types (shared by both factories)
// ------------------------------------------------------------------

interface KeyedPathSubscriber<K> {
  /** Discriminator — `'keyed'` means the subscriber carries a cache key and
   * its `notify` will be routed through the owning client's
   * `BusHandler` (which evicts `known[key]` / `values[key]` / `pending[key]`). */
  kind: 'keyed';
  clientId: symbol;
  key: K;
  notify: () => void;
}

interface CallbackPathSubscriber {
  /** `'callback'` means the subscriber has no key and the bus dispatches
   * `notify` directly — the owning clientId is used only to register a
   * shared stateless handler (`NOOP_HANDLER`). */
  kind: 'callback';
  clientId: symbol;
  notify: () => void;
}

type PathSubscriber = KeyedPathSubscriber<unknown> | CallbackPathSubscriber;

function isCallbackSubscriber(sub: PathSubscriber): sub is CallbackPathSubscriber {
  return sub.kind === 'callback';
}

// Per-variant handler types. The factory's `handler` only ever sees keyed
// subscribers (it can't receive callback ones — they route via
// NOOP_HANDLER), so it's typed as `KeyedBusHandler<K>`. NOOP_HANDLER is
// typed as `CallbackBusHandler` for symmetry. Splitting them is what
// kills the `as K` cast in the factory — issue #355.
type KeyedBusHandler<K> = (sub: KeyedPathSubscriber<K>) => void;
type CallbackBusHandler = (sub: CallbackPathSubscriber) => void;

// ------------------------------------------------------------------
// Module-level bus globals — shared by both factories + the
// `subscribeGitPathInvalidation` callback-only API.
// ------------------------------------------------------------------
const pathSubscribers = new Map<string, Set<PathSubscriber>>();
// Per-variant handler maps. Splitting keyed vs callback lets each bus
// dispatch do a single, well-typed lookup — `busHandlers.get(...)` on a
// single mixed map would return `KeyedBusHandler<any> | CallbackBusHandler`
// and force the dispatch to cast (`(handler as CallbackBusHandler)(sub)`),
// because TypeScript can't narrow a map lookup from a sibling
// discriminator. Two maps = two monomorphic lookups, no casts. The
// subscriber's `kind` field still narrows the SUBSCRIBER (so `sub.key` is
// always defined inside a keyed handler); the lookup-side narrowing is
// what this split fixes. Issue #355.
const keyedBusHandlers = new Map<symbol, KeyedBusHandler<any>>();
const callbackBusHandlers = new Map<symbol, CallbackBusHandler>();
let listenerInstalled = false;

// Test-only: every client registers a closure that clears its own `cache` +
// `pending` Maps. `resetPathInvalidatedCacheForTests` calls them so per-client
// state is wiped too — the bus globals above aren't enough, because a
// module-level client's `cache` is closure state that would otherwise leak
// values across tests that don't `vi.resetModules()` (e.g. component tests that
// consume a shared client). Each client registers exactly once at construction.
const clientCacheResets = new Set<() => void>();

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
        if (isCallbackSubscriber(sub)) {
          // Single, monomorphic map lookup — no cast needed (issue #355).
          const handler = callbackBusHandlers.get(sub.clientId);
          if (handler) handler(sub);
        } else {
          const handler = keyedBusHandlers.get(sub.clientId);
          if (handler) handler(sub);
        }
      }
    }
  });
}

/**
 * Test-only: clears all module-level state so each test can re-import and
 * start with a clean bus. The hook tests also use `vi.resetModules()` so
 * the freshly-imported module's `createPathKeyedCache` /
 * `createDualKeyCache` call gets a fresh entry on top of this cleared
 * global state.
 */
export function resetPathInvalidatedCacheForTests(): void {
  pathSubscribers.clear();
  keyedBusHandlers.clear();
  callbackBusHandlers.clear();
  listenerInstalled = false;
  clientCacheResets.forEach((reset) => reset());
}

/**
 * Idempotently registers a keyed bus handler for `clientId`. Called on
 * every `subscribe` (and on every factory call) so the bus keeps working
 * after `resetPathInvalidatedCacheForTests` wipes `keyedBusHandlers`
 * between tests. Issue #355 split this from the callback variant so the
 * dispatch-side lookup is monomorphic (no union cast).
 */
function registerKeyedBusHandler(clientId: symbol, handler: KeyedBusHandler<any>): void {
  keyedBusHandlers.set(clientId, handler);
}

/**
 * Idempotently registers a callback bus handler for `clientId`. Same
 * contract as `registerKeyedBusHandler` — split by variant so the bus
 * dispatch lookup returns the right shape (issue #355).
 */
function registerCallbackBusHandler(clientId: symbol, handler: CallbackBusHandler): void {
  callbackBusHandlers.set(clientId, handler);
}

/**
 * Adds `sub` to the path-keyed set, creating the set on first use, and
 * returns an idempotent unsubscribe closure. The closure removes `sub`
 * from the set; if that was the last subscriber, the (now-empty) set is
 * also pruned from `pathSubscribers` so the global map doesn't accumulate
 * dead entries. Issue #356: this used to be duplicated between
 * `client.subscribe` and `subscribeGitPathInvalidation` — the prune
 * was the most likely drift point.
 */
function addPathSubscriber(sub: PathSubscriber, path: string): () => void {
  let set = pathSubscribers.get(path);
  if (!set) {
    set = new Set();
    pathSubscribers.set(path, set);
  }
  set.add(sub);
  return () => {
    const live = pathSubscribers.get(path);
    if (!live) return;
    live.delete(sub);
    if (live.size === 0) pathSubscribers.delete(path);
  };
}

// ------------------------------------------------------------------
// Shared client internals. Both factories construct the same
// read/refresh/invalidate/subscribeOn methods; only the public
// `subscribe*` shape differs. The helper is internal — never exported.
// Issue #347 follow-up: pulled read/refresh/invalidate into the helper
// after the refactor initially left them duplicated between the two
// factories.
// ------------------------------------------------------------------

interface InternalClient<K, V> {
  /** Returns the cached value for `key`, or `undefined` if no entry exists.
   * A cached `null` is returned as `null` (not `undefined`). */
  read(key: K): V | null | undefined;
  /** Fetches the value for `key`, deduping with any concurrent caller. On
   * rejection, the cache is left untouched and the in-flight is cleared. */
  refresh(key: K): Promise<V | null>;
  /** Returns the most recent error for `key`, or `null` if the last refresh
   * succeeded (or no refresh has run yet). Issue #342. */
  lastError(key: K): Error | null;
  /** Erases the cached value AND any in-flight fetch for `key`. */
  invalidate(key: K): void;
  /** Wires the bus handler + listener and registers a keyed subscriber
   * on `path`. Returns the idempotent unsubscribe. */
  subscribeOn(key: K, path: string, onInvalidate: () => void): () => void;
  /** See the public-client `notifyByPath` docstring — issue #780. */
  notifyByPath(path: string): void;
}

function createInternalClient<K, V>(
  fetcher: (key: K) => Promise<V | null>,
  name: string,
  minRefetchIntervalMs = 0,
): InternalClient<K, V> {
  // Per-client state. A `Symbol` clientId is the load-bearing piece that
  // makes the cross-client dispatch scoping work — see module docstring.
  const clientId = Symbol('pathInvalidatedCache');
  // Presence marker: `known.has(key)` is `true` iff `key` has ever been
  // cached (via a successful `refresh`); the boolean value itself is
  // irrelevant. `values` holds the actual cached value and simply has no
  // entry for a key whose cached value is `null` — see the `null` vs
  // `undefined` docstring above (issue #346).
  const known = new Map<K, true>();
  const values = new Map<K, V>();
  const pending = new Map<K, Promise<V | null>>();
  // Per-key most-recent error, populated by `refresh`'s catch and cleared
  // on the next successful refresh for the same key. Exposed via
  // `lastError(key)` so callers can disambiguate "fetch failed" from
  // "fetcher returned null" — issue #342.
  const errors = new Map<K, Error>();
  // Per-key timestamp of the last successful refresh — drives the
  // `minRefetchIntervalMs` freshness window in the bus handler below.
  const lastFetchedAt = new Map<K, number>();
  // Trailing-refetch state for the freshness window: when a bus event is
  // suppressed (value still fresh), we remember the suppressed subscribers'
  // notify callbacks and arm ONE timer per key for the window's expiry.
  // Firing evicts + notifies, so the settled state after a burst always
  // lands — suppression bounds the refetch RATE, it never drops the final
  // refetch (the pre-existing behaviour left panels stale until the next
  // unrelated event).
  const trailingTimers = new Map<K, ReturnType<typeof setTimeout>>();
  const trailingNotifies = new Map<K, Set<() => void>>();

  const cancelTrailing = (key: K) => {
    const timer = trailingTimers.get(key);
    if (timer !== undefined) clearTimeout(timer);
    trailingTimers.delete(key);
    trailingNotifies.delete(key);
  };

  const fireTrailing = (key: K) => {
    trailingTimers.delete(key);
    const notifies = trailingNotifies.get(key);
    trailingNotifies.delete(key);
    known.delete(key);
    values.delete(key);
    pending.delete(key);
    // The stamp must go too: the value is no longer authoritative, so the
    // next bus event (or this notify's refetch) must not be re-suppressed
    // off the stale timestamp.
    lastFetchedAt.delete(key);
    notifies?.forEach((notify) => notify());
  };

  // Let `resetPathInvalidatedCacheForTests` wipe this client's state too.
  clientCacheResets.add(() => {
    known.clear();
    values.clear();
    pending.clear();
    errors.clear();
    lastFetchedAt.clear();
    trailingTimers.forEach((timer) => clearTimeout(timer));
    trailingTimers.clear();
    trailingNotifies.clear();
  });

  // The bus calls this for every matched KEYED subscriber of THIS client. It
  // evicts the cache (so the next read returns `undefined`) and clears the
  // in-flight (so the next refresh starts fresh instead of resolving to a
  // stale value), then calls the subscriber's notify. Typed as
  // `KeyedBusHandler<K>` so the `sub.key` reads/writes are checked
  // against `K` — no `as K` cast (issue #355).
  const handler: KeyedBusHandler<K> = (sub) => {
    // Freshness window (see `minRefetchIntervalMs` docs): a value fetched
    // recently enough is authoritative — skip the immediate eviction and
    // notify, but arm the trailing refetch so the settled state still
    // lands once the window expires.
    if (minRefetchIntervalMs > 0) {
      const fetchedAt = lastFetchedAt.get(sub.key);
      if (fetchedAt !== undefined && Date.now() - fetchedAt < minRefetchIntervalMs) {
        let notifies = trailingNotifies.get(sub.key);
        if (!notifies) {
          notifies = new Set();
          trailingNotifies.set(sub.key, notifies);
        }
        notifies.add(sub.notify);
        if (!trailingTimers.has(sub.key)) {
          const delay = minRefetchIntervalMs - (Date.now() - fetchedAt);
          trailingTimers.set(
            sub.key,
            setTimeout(() => fireTrailing(sub.key), delay),
          );
        }
        return;
      }
    }
    cancelTrailing(sub.key);
    known.delete(sub.key);
    values.delete(sub.key);
    pending.delete(sub.key);
    sub.notify();
  };

  return {
    read(key) {
      if (!known.has(key)) return undefined;
      return values.get(key) ?? null;
    },

    refresh(key) {
      const inFlight = pending.get(key);
      if (inFlight) return inFlight;

      const p = fetcher(key)
        .then((result) => {
          known.set(key, true);
          // `values` only holds non-null results — a `null` result leaves
          // (or clears) no entry there, so `read`'s `values.get(key) ?? null`
          // falls through to `null` correctly, including on a refetch that
          // flips a previously-cached non-null value back to `null`.
          if (result === null) {
            values.delete(key);
          } else {
            values.set(key, result);
          }
          pending.delete(key);
          lastFetchedAt.set(key, Date.now());
          // A success erases the previous failure for this key — the hook
          // layer's `error` field will read `null` on the next state read.
          errors.delete(key);
          // A fresh fetch already reflects any changes the suppressed
          // events described — the pending trailing refetch would just
          // duplicate this result one window later. Cancel it.
          cancelTrailing(key);
          return result;
        })
        .catch((err) => {
          pending.delete(key);
          // Record the error for `lastError(key)`. The cache itself is
          // left untouched (the success branch owns writes), so a
          // successful refresh is the only path that populates the cache.
          // Wrap non-Error throws so the contract is always `Error`.
          errors.set(
            key,
            err instanceof Error ? err : new Error(String(err)),
          );
          // eslint-disable-next-line no-console
          console.warn(`${name}: fetch failed for key`, key, err);
          return null;
        });
      pending.set(key, p);
      return p;
    },

    lastError(key) {
      return errors.get(key) ?? null;
    },

    invalidate(key) {
      known.delete(key);
      values.delete(key);
      pending.delete(key);
      // An explicit invalidation says the value is no longer authoritative:
      // drop the freshness stamp so the `minRefetchIntervalMs` window can't
      // keep suppressing bus notifications while the cache sits empty, and
      // cancel any armed trailing refetch (the caller is about to drive its
      // own refresh — a deferred second eviction would race it).
      lastFetchedAt.delete(key);
      cancelTrailing(key);
    },

    subscribeOn(key, path, onInvalidate) {
      // Re-register the handler (idempotent — see `registerKeyedBusHandler`)
      // and install the global listener. Then add the subscriber via the
      // shared helper. Issue #356.
      registerKeyedBusHandler(clientId, handler);
      installListener();
      const sub: KeyedPathSubscriber<K> = { kind: 'keyed', clientId, key, notify: onInvalidate };
      return addPathSubscriber(sub, path);
    },

    notifyByPath(path) {
      // Programmatic invalidation for buildmesh's own write-paths
      // (create/merge PR flows, issue #780). Mirrors the bus listener's
      // per-path dispatch (`installListener`) but skips the freshness
      // check entirely — a deliberate invalidation from the same
      // process is always authoritative.
      //
      // Scoping mirrors the bus dispatch one-for-one:
      //   - Callback-only subscribers (from `subscribeGitPathInvalidation`,
      //     registered with the shared `NOOP_CLIENT_ID`) carry no key
      //     and no client state — always fire `notify`.
      //   - Keyed subscribers fire only when their `clientId` matches
      //     THIS client, so a sibling cache client on the same path
      //     is untouched. The keyed branch drops `known` / `values` /
      //     `pending` / `lastFetchedAt` (mirroring the bus handler's
      //     evict step) and cancels any armed trailing refetch — the
      //     caller is about to drive its own refresh, so a deferred
      //     second eviction would race it.
      const subs = pathSubscribers.get(path);
      if (!subs) return;
      for (const sub of subs) {
        if (isCallbackSubscriber(sub)) {
          sub.notify();
          continue;
        }
        if (sub.clientId !== clientId) continue;
        // `pathSubscribers` stores `KeyedPathSubscriber<unknown>` (the
        // shared, cross-client Set); we just narrowed `clientId` to
        // OUR client, so `sub.key` is in fact a `K`. The cast mirrors
        // the per-client handler's typing (see `KeyedBusHandler<K>`).
        const keyed = sub as KeyedPathSubscriber<K>;
        cancelTrailing(keyed.key);
        known.delete(keyed.key);
        values.delete(keyed.key);
        pending.delete(keyed.key);
        lastFetchedAt.delete(keyed.key);
        keyed.notify();
      }
    },
  };
}

// ------------------------------------------------------------------
// The two factories
// ------------------------------------------------------------------

/**
 * Single-key cache client. `key` is a path-shaped string that doubles as
 * the GIT_CHANGED subscription path; the `subscribe(key, cb)` method takes
 * no separate `path` argument.
 *
 * @example
 * ```ts
 * const summaryClient = createPathKeyedCache<GitSummary>({
 *   fetcher: getGitSummary,
 *   name: 'useGitSummary',
 * });
 * // hook usage:
 * usePathInvalidatedQuery(summaryClient, gitPath);
 * ```
 */
export function createPathKeyedCache<V>(
  options: PathInvalidatedCacheOptions<string, V>,
): PathKeyedClient<V> {
  const { fetcher, name = 'pathInvalidatedCache', minRefetchIntervalMs } = options;
  const internal = createInternalClient<string, V>(fetcher, name, minRefetchIntervalMs);

  return {
    ...internal,
    // For the single-key shape, the key IS the path the bus matches.
    subscribe: (key, onInvalidate) => internal.subscribeOn(key, key, onInvalidate),
  };
}

/**
 * Dual-key cache client. `key` is an arbitrary id (e.g. `nodeId`, `meshId`);
 * `path` is the separate string the GIT_CHANGED subscription should match.
 * The `subscribeByPath(key, path, cb)` method makes the dual shape explicit
 * at the type level — there's no longer a "key = path" idiom to read past.
 *
 * @example
 * ```ts
 * const prClient = createDualKeyCache<number, OpenPr>({
 *   fetcher: getOpenPrForNode,
 *   name: 'useOpenPr',
 * });
 * // hook usage:
 * usePathInvalidatedQuery(prClient, nodeId, gitPath);
 * ```
 */
export function createDualKeyCache<K, V>(
  options: PathInvalidatedCacheOptions<K, V>,
): DualKeyClient<K, V> {
  const { fetcher, name = 'pathInvalidatedCache', minRefetchIntervalMs } = options;
  const internal = createInternalClient<K, V>(fetcher, name, minRefetchIntervalMs);

  // The `subscribe` and `subscribeOn` members are intentionally NOT
  // exposed on the dual-key public type — `subscribeByPath` is the only
  // public subscription method. `read`/`refresh`/`lastError`/`invalidate`/
  // `notifyByPath` come straight from the internal client. Issue #342 adds
  // `lastError`; #780 adds `notifyByPath`.
  const { read, refresh, lastError, invalidate, subscribeOn, notifyByPath } = internal;
  return {
    read,
    refresh,
    lastError,
    invalidate,
    subscribeByPath: subscribeOn,
    notifyByPath,
  };
}

/**
 * Subscribes `cb` to `GIT_CHANGED` events that match `path` (using the
 * same `pathMatchesGitEvent` helper the hook-backed clients use, so the
 * worktree-subdir + WUNC + cross-platform case rules apply). Returns a
 * synchronous unsubscribe function.
 *
 * Use this from components that need the invalidation callback but don't
 * have a key+cache to manage — i.e. consumers for whom
 * `createPathKeyedCache` / `createDualKeyCache` is overkill. Used directly
 * by `AgentReviewPanel` and `CenterDiffOverlay`; intentionally NOT used by
 * the `usePathInvalidatedQuery` hook (which subscribes via the
 * cache-bearing `client.subscribe(key, cb)` on its own client).
 * Issue #345.
 *
 * Compared to the hand-rolled `listen(GIT_CHANGED, ...) + pathMatchesGitEvent`
 * pattern this replaces:
 *   - ONE global listener is still installed by the primitive, so adding
 *     a subscriber never multiplies Tauri's per-event handler cost.
 *   - The worktree-subdir + WUNC path matching is shared, not duplicated.
 *   - The unsubscribe is synchronous, so the React effect's cleanup
 *     doesn't have to chase a Promise (the hand-rolled pattern did
 *     `unlisten.then(u => u())`, which leaks a microtask per unmount).
 *
 * `cb` is invoked with no arguments — the bus payload is the same shape
 * the hook subscribers see, but callback-only consumers have always
 * ignored it; pass an arrow if you need to capture component state.
 */
export function subscribeGitPathInvalidation(
  path: string,
  cb: () => void,
): () => void {
  installListener();
  // The noop handler is stateless and shared across every
  // `subscribeGitPathInvalidation` caller. The re-register call
  // (idempotent — see `registerCallbackBusHandler`) keeps the bus working
  // after `resetPathInvalidatedCacheForTests` wipes `callbackBusHandlers`
  // between tests.
  registerCallbackBusHandler(NOOP_CLIENT_ID, NOOP_HANDLER);
  const sub: CallbackPathSubscriber = { kind: 'callback', clientId: NOOP_CLIENT_ID, notify: cb };
  return addPathSubscriber(sub, path);
}
