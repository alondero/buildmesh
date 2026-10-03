/**
 * `useSettingsResources` — extracted from `AppSettingsModal` (issue
 * #1534 round-5 review) to keep the resource-load state machine out
 * of the view component. The modal grew past 2,300 lines once the
 * per-resource states, loaders, and retry logic were all inline;
 * this hook isolates the load-status bookkeeping behind a small
 * API the modal consumes.
 *
 * Scope: this hook owns the resource load state machine — the
 * `idle | loading | loaded | failed` status + last-error message
 * for each of the resources (preferences, routing choices, providers,
 * accounts, pairings, coordinator, devices, network). It does NOT
 * own the derived state the modal reads from each successful
 * load (the `providers` array, the `pairings` array, etc.).
 * Mutation handlers mix loader refreshes with imperative optimistic
 * updates (`setSelected(newValue)` then `await api.setAppDefaultProvider(...)`
 * then `await loadPreferences()`); pushing derived state into the
 * hook would add friction without architectural gain. The modal
 * keeps its derived state and passes `onSuccess` callbacks into
 * each loader.
 *
 * Concurrency: each loader bumps a per-resource monotonic request
 * id captured in a ref. After the API settles, the loader commits
 * only if the captured id is still the current one — a slow
 * rejection can never overwrite a fast retry's success. This is the
 * request-sequence pattern round-5 review asked for.
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { formatError } from '../../lib/errorUtils';
import * as api from '../../lib/tauri';

/** Per-resource load status (issue #1534). */
export type ResourceStatus = 'idle' | 'loading' | 'loaded' | 'failed';

/** Per-resource load bookkeeping: status + last error message. */
export interface ResourceState {
  status: ResourceStatus;
  error: string | null;
}

/** Named-resource keys (issue #1534). */
export type ResourceKey =
  | 'preferences'
  | 'routing'
  | 'providers'
  | 'accounts'
  | 'pairings'
  | 'coordinator'
  | 'devices'
  | 'network';

export const INITIAL_RESOURCES: Record<ResourceKey, ResourceState> = {
  preferences: { status: 'loading', error: null },
  routing: { status: 'loading', error: null },
  providers: { status: 'loading', error: null },
  accounts: { status: 'loading', error: null },
  // Issue #1935 — pairings no longer waits on providers. Its attach-picker map
  // comes from the backend in one call, so the resource starts with the rest of
  // the mount fan-out and costs `max(...)` instead of `providers + pairings`.
  pairings: { status: 'loading', error: null },
  coordinator: { status: 'loading', error: null },
  devices: { status: 'loading', error: null },
  network: { status: 'loading', error: null },
};

/** Pairings load needs a runtime-specific helper for fetching
 *  verification state across Windows + WSL. The modal owns this
 *  helper and passes it into `loadPairings`. */
export type HostPairingVerificationsFn = () => Promise<api.PairingVerification[]>;

/** Per-resource success callbacks. Each fires after the loader
 *  commits the `loaded` status; the modal uses them to update its
 *  derived state. All callbacks are optional — a modal that
 *  doesn't care about a particular resource's data can omit it. */
export interface SettingsResourceCallbacks {
  onPreferencesLoaded?: (prefs: api.AppPreferences) => void;
  onRoutingLoaded?: (list: api.ProviderInfo[]) => void;
  onProvidersLoaded?: (list: api.ProviderInfo[]) => void;
  onAccountsLoaded?: (data: {
    accountList: api.ProviderAccount[];
    catalog: api.ProviderAccount[];
  }) => void;
  onPairingsLoaded?: (data: {
    effective: api.ProviderPairing[];
    verifications: api.PairingVerification[];
    storedKeys: Set<string>;
    compatible: Record<string, api.ProviderAccount[]>;
  }) => void;
  onCoordinatorLoaded?: (data: { enabled: boolean; has_token: boolean }) => void;
  onDevicesLoaded?: (list: api.DeviceSession[]) => void;
  onNetworkLoaded?: (data: {
    lan_exposure_enabled: boolean;
    tls_active: boolean;
    exposed_interfaces: api.RealizedBind[];
  }) => void;
}

export interface UseSettingsResources extends SettingsResourceCallbacks {
  resources: Record<ResourceKey, ResourceState>;
  setResource: (key: ResourceKey, next: ResourceState) => void;
  loadPreferences: () => Promise<api.AppPreferences | null>;
  loadRouting: () => Promise<api.ProviderInfo[] | null>;
  loadProviders: () => Promise<api.ProviderInfo[] | null>;
  loadAccounts: () => Promise<{ accountList: api.ProviderAccount[]; catalog: api.ProviderAccount[] } | null>;
  loadPairings: () => Promise<{
    effective: api.ProviderPairing[];
    verifications: api.PairingVerification[];
    storedKeys: Set<string>;
    compatible: Record<string, api.ProviderAccount[]>;
  } | null>;
  loadCoordinator: () => Promise<{ enabled: boolean; has_token: boolean } | null>;
  loadDevices: () => Promise<api.DeviceSession[] | null>;
  loadNetwork: () => Promise<{
    lan_exposure_enabled: boolean;
    tls_active: boolean;
    exposed_interfaces: api.RealizedBind[];
  } | null>;
  /** Retry a single failed resource. Takes no preconditions — every loader
   *  reads what it needs. */
  retryResource: (key: ResourceKey) => void;
}

export function useSettingsResources(
  callbacks: SettingsResourceCallbacks & { getHostPairingVerifications?: HostPairingVerificationsFn } = {},
): UseSettingsResources {
  const [resources, setResourcesState] = useState<Record<ResourceKey, ResourceState>>(
    INITIAL_RESOURCES,
  );

  /** Stash callbacks + helpers in a ref so the loaders don't need
   *  to depend on callback identity (which changes on every render
   *  if defined inline). The ref's `current` is updated each
   *  render. */
  const callbacksRef = useRef(callbacks);
  callbacksRef.current = callbacks;

  /** Set one resource's status + error atomically. */
  const setResource = useCallback((key: ResourceKey, next: ResourceState) => {
    setResourcesState((prev) => ({ ...prev, [key]: next }));
  }, []);

  // Per-resource monotonic request id. Issue #1534 (review round 5)
  // — the concurrency token pattern. A loader bumps the counter
  // when it starts; after the API settles, it commits only if the
  // counter hasn't advanced (i.e. a newer request superseded this
  // one). Without this, a slow rejection could overwrite a fast
  // retry's success.
  const requestIdsRef = useRef<Record<ResourceKey, number>>({
    preferences: 0,
    routing: 0,
    providers: 0,
    accounts: 0,
    pairings: 0,
    coordinator: 0,
    devices: 0,
    network: 0,
  });
  useEffect(() => () => {
    for (const key of Object.keys(requestIdsRef.current) as ResourceKey[]) requestIdsRef.current[key]++;
  }, []);

  /** Generic loader that does the bookkeeping: bump request id,
   *  flip to `loading`, run the loader, commit only if our id is
   *  still current, fire the success callback. Returns the data on
   *  success, `null` on failure OR if the request was superseded. */
  const withResourceLoad = useCallback(
    async <T,>(
      key: ResourceKey,
      loader: () => Promise<T>,
      onSuccess: (data: T) => void,
    ): Promise<T | null> => {
      const myId = ++requestIdsRef.current[key];
      setResource(key, { status: 'loading', error: null });
      try {
        const data = await loader();
        // Concurrency check: a newer request superseded us (rapid
        // retry, or a different mutation triggered a re-load).
        // Discard this result rather than overwrite.
        if (requestIdsRef.current[key] !== myId) return null;
        onSuccess(data);
        setResource(key, { status: 'loaded', error: null });
        return data;
      } catch (e) {
        if (requestIdsRef.current[key] !== myId) return null;
        setResource(key, { status: 'failed', error: formatError(e) });
        return null;
      }
    },
    [setResource],
  );

  /** One preferences read shared by the two loaders that need it (issue
   *  #1935). `loadPreferences` and `loadPairings` start concurrently on mount
   *  and both want `provider_pairings`; preferences is a process-cached read,
   *  so a second round trip would re-read the store the modal already holds.
   *  The in-flight promise is dropped once it settles, so a post-mutation
   *  refresh still reads fresh state — this coalesces, it does not cache. */
  const prefsInFlightRef = useRef<Promise<api.AppPreferences> | null>(null);
  const readPreferences = useCallback(() => {
    const inFlight = prefsInFlightRef.current;
    if (inFlight) return inFlight;
    const request = api.getAppPreferences();
    prefsInFlightRef.current = request;
    const clear = () => {
      if (prefsInFlightRef.current === request) prefsInFlightRef.current = null;
    };
    // Both arms are handlers (not a bare `.then`), so a rejected shared read
    // never surfaces as an unhandled rejection from this bookkeeping branch.
    request.then(clear, clear);
    return request;
  }, []);

  const loadPreferences = useCallback(
    () => withResourceLoad('preferences', () => readPreferences(), (prefs) => {
      callbacksRef.current.onPreferencesLoaded?.(prefs);
    }),
    [withResourceLoad, readPreferences],
  );

  const loadRouting = useCallback(
    () => withResourceLoad('routing', () => api.listRoutingOptions(), list => {
      if (!Array.isArray(list)) throw new Error('Invalid routing choices response');
      callbacksRef.current.onRoutingLoaded?.(list);
    }), [withResourceLoad],
  );

  const loadProviders = useCallback(
    () => withResourceLoad('providers', () => api.listProviders(), (list) => {
      callbacksRef.current.onProvidersLoaded?.(list);
    }),
    [withResourceLoad],
  );

  const loadAccounts = useCallback(
    () =>
      withResourceLoad(
        'accounts',
        async () => {
          // Issue #1534 (review round 4) — accounts is the primary
          // read; catalog is auxiliary. Catalog failures must not
          // hide real accounts.
          const accountList = await api.getProviderAccounts();
          let catalog: api.ProviderAccount[] = [];
          try {
            catalog = await api.getKeyedFirstClassCatalog();
          } catch {
            // Non-fatal: account cards still render correctly
            // without the catalog. Log so a backend issue is
            // debuggable.
            console.warn(
              '[AppSettings] Failed to load keyed-first-class catalog; add-picker will be empty.',
            );
          }
          return { accountList, catalog };
        },
        (data) => {
          callbacksRef.current.onAccountsLoaded?.(data);
        },
      ),
    [withResourceLoad],
  );

  const loadPairings = useCallback(
    () => {
      const helper = callbacksRef.current.getHostPairingVerifications;
      if (!helper) {
        // The modal must wire `getHostPairingVerifications` via
        // the hook's callbacks. Without it, we can't fetch
        // verifications across Windows + WSL — but we can still
        // return empty verifications and mark pairings as
        // loaded with whatever else we got. This is a defensive
        // path that should never fire in production (the modal
        // always wires the helper).
        return withResourceLoad(
          'pairings',
          async () => ({
            effective: [],
            verifications: [],
            storedKeys: new Set<string>(),
            compatible: {},
          }),
          (data) => callbacksRef.current.onPairingsLoaded?.(data),
        );
      }
      return withResourceLoad(
        'pairings',
        async () => {
          // Issue #1935 — one fan-out, no `providers` in the critical path.
          // The attach-picker map is one backend call over every harness the
          // pane can render (`compatible_providers_by_harness`), so this loader
          // needs nothing from the Spawn Menu and starts with the rest of the
          // mount fan-out.
          //
          // Issue #1534 (review round 4) — stored pairings (which rows are
          // user-detachable vs derived) live on the preferences object.
          // Coupling pairings to a strict getAppPreferences call was the
          // failure-isolation bug; now the lookup is best-effort. The read is
          // shared with `loadPreferences` rather than issued a second time.
          const [prefs, effective, verifications, compatible] = await Promise.all([
            readPreferences().then((p) => p, () => null),
            api.getProviderPairings(),
            helper(),
            api.compatibleProvidersByHarness(),
          ]);
          const stored = prefs?.provider_pairings ?? [];
          return {
            effective: Array.isArray(effective) ? effective : [],
            verifications: Array.isArray(verifications) ? verifications : [],
            storedKeys: new Set(stored.map((p) => `${p.harness_id}:${p.provider_id}`)),
            compatible:
              compatible && typeof compatible === 'object' ? compatible : {},
          };
        },
        (data) => {
          callbacksRef.current.onPairingsLoaded?.(data);
        },
      );
    },
    [withResourceLoad, readPreferences],
  );

  const loadCoordinator = useCallback(
    () =>
      withResourceLoad(
        'coordinator',
        () => api.getCoordinatorStatus(),
        (data) => {
          callbacksRef.current.onCoordinatorLoaded?.(data);
        },
      ),
    [withResourceLoad],
  );

  const loadDevices = useCallback(
    () =>
      withResourceLoad(
        'devices',
        () => api.listDeviceSessions(),
        (list) => {
          callbacksRef.current.onDevicesLoaded?.(list);
        },
      ),
    [withResourceLoad],
  );

  const loadNetwork = useCallback(
    () =>
      withResourceLoad(
        'network',
        () => api.getNetworkStatus(),
        (data) => {
          callbacksRef.current.onNetworkLoaded?.(data);
        },
      ),
    [withResourceLoad],
  );

  /** Retry a single failed resource. No cross-resource preconditions. The
   *  pairings boundary check (issue #1534 round 4) is gone: it demanded a
   *  non-empty providers list, but no call site ever passed one —
   *  `AppSettingsModal` calls `retryResource('pairings')` — so Retry on a
   *  failed pairings load never retried anything and instead replaced the real
   *  error with "Awaiting providers list". That was a dead end: providers has
   *  usually succeeded, so there is no providers banner to point the user at,
   *  and the pane recovers only on a modal reopen. Its justification was that
   *  the loader could not build its picker map without the provider list, which
   *  issue #1935 removed. When providers genuinely fails, the Harnesses pane
   *  renders the providers banner ahead of the pairings one — that ordering is
   *  the affordance the guard stood in for.
   *
   * Issue #1534 (review round 5) — replaced the resource switch with
   * a `Record<ResourceKey, () => void>` lookup so adding a
   * resource is type-checked at the table's construction site, and
   * removed the redundant `void` cast on each invocation. */
  const retryResource = useCallback(
    (key: ResourceKey) => {
      const retryFns: Record<ResourceKey, () => void> = {
        preferences: () => void loadPreferences(),
        routing: () => void loadRouting(),
        providers: () => void loadProviders(),
        accounts: () => void loadAccounts(),
        pairings: () => void loadPairings(),
        coordinator: () => void loadCoordinator(),
        devices: () => void loadDevices(),
        network: () => void loadNetwork(),
      };
      retryFns[key]();
    },
    [
      loadPreferences,
      loadRouting,
      loadProviders,
      loadAccounts,
      loadPairings,
      loadCoordinator,
      loadDevices,
      loadNetwork,
    ],
  );

  return {
    resources,
    setResource,
    loadPreferences,
    loadRouting,
    loadProviders,
    loadAccounts,
    loadPairings,
    loadCoordinator,
    loadDevices,
    loadNetwork,
    retryResource,
  };
}
