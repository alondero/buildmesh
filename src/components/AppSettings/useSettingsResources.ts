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
 * for each of the seven resources (preferences, providers,
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
import { useCallback, useRef, useState } from 'react';
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
  | 'providers'
  | 'accounts'
  | 'pairings'
  | 'coordinator'
  | 'devices'
  | 'network';

export const INITIAL_RESOURCES: Record<ResourceKey, ResourceState> = {
  preferences: { status: 'loading', error: null },
  providers: { status: 'loading', error: null },
  accounts: { status: 'loading', error: null },
  // Pairings derive their `compatibleByHarness` map from `providers`,
  // so they only start loading once providers loads successfully.
  pairings: { status: 'idle', error: null },
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
  loadProviders: () => Promise<api.ProviderInfo[] | null>;
  loadAccounts: () => Promise<{ accountList: api.ProviderAccount[]; catalog: api.ProviderAccount[] } | null>;
  loadPairings: (
    providerList: api.ProviderInfo[],
  ) => Promise<{
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
  /** Retry a single failed resource. The pairings path requires
   *  the current providers list (passed from the modal, which owns
   *  `providers` state). */
  retryResource: (key: ResourceKey, options?: { providers?: api.ProviderInfo[] }) => void;
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
    providers: 0,
    accounts: 0,
    pairings: 0,
    coordinator: 0,
    devices: 0,
    network: 0,
  });

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

  const loadPreferences = useCallback(
    () => withResourceLoad('preferences', () => api.getAppPreferences(), (prefs) => {
      callbacksRef.current.onPreferencesLoaded?.(prefs);
    }),
    [withResourceLoad],
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
    (providerList: api.ProviderInfo[]) => {
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
          // Issue #1534 (review round 4) — stored pairings
          // (which rows are user-detachable vs derived) live on
          // the preferences object. Coupling pairings to a
          // strict getAppPreferences call was the failure-
          // isolation bug; now the lookup is best-effort.
          let storedKeys = new Set<string>();
          try {
            const prefs = await api.getAppPreferences();
            const stored = prefs.provider_pairings ?? [];
            storedKeys = new Set(
              stored.map((p) => `${p.harness_id}:${p.provider_id}`),
            );
          } catch {
            // Non-fatal: pairings can render without the stored
            // hint.
          }
          const [effective, verifications] = await Promise.all([
            api.getProviderPairings(),
            helper(),
          ]);
          const nativeHarnesses = providerList.filter(
            (p) => !p.is_proxied && p.id !== 'terminal',
          );
          const entries = await Promise.all(
            nativeHarnesses.map(async (h) => {
              const list = await api.compatibleProvidersForHarness(h.id);
              return [h.id, Array.isArray(list) ? list : []] as const;
            }),
          );
          return {
            effective: Array.isArray(effective) ? effective : [],
            verifications: Array.isArray(verifications) ? verifications : [],
            storedKeys,
            compatible: Object.fromEntries(entries),
          };
        },
        (data) => {
          callbacksRef.current.onPairingsLoaded?.(data);
        },
      );
    },
    [withResourceLoad],
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

  /** Retry a single failed resource. The pairings path has a hard
   *  dependency on providers: it queries
   *  `compatible_providers_for_harness` for each non-terminal
   *  harness in the providers list. If providers isn't loaded,
   *  retrying pairings would fabricate a clean state. The boundary
   *  check lives here (issue #1534 round 4).
   *
   * Issue #1534 (review round 5) — replaced the 7-case switch with
   * a `Record<ResourceKey, (options?) => void>` lookup so adding a
   * resource is type-checked at the table's construction site, and
   * removed the redundant `void` cast on each invocation. */
  const retryResource = useCallback(
    (key: ResourceKey, options?: { providers?: api.ProviderInfo[] }) => {
      const retryFns: Record<
        ResourceKey,
        (opts?: { providers?: api.ProviderInfo[] }) => void
      > = {
        preferences: () => void loadPreferences(),
        providers: () => void loadProviders(),
        accounts: () => void loadAccounts(),
        pairings: () => {
          if (!options?.providers || options.providers.length === 0) {
            // Awaiting providers. The boundary check (issue
            // #1534 round 4) — pairings can't load without a
            // providers list. Mark pairings as failed with a
            // clear message so the user retries providers first.
            setResource('pairings', {
              status: 'failed',
              error: 'Awaiting providers list — retry providers first.',
            });
            return;
          }
          void loadPairings(options.providers);
        },
        coordinator: () => void loadCoordinator(),
        devices: () => void loadDevices(),
        network: () => void loadNetwork(),
      };
      retryFns[key](options);
    },
    [
      setResource,
      loadPreferences,
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
    loadProviders,
    loadAccounts,
    loadPairings,
    loadCoordinator,
    loadDevices,
    loadNetwork,
    retryResource,
  };
}
