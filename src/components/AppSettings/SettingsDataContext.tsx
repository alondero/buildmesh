/**
 * `SettingsDataContext` — the shared spine of the Settings modal
 * (issue #1880).
 *
 * Before this split, `AppSettingsModal` held the resource state machine
 * *and* every pane's drafts, toggles and mutation handlers — ~2,600 lines
 * and roughly 30 `useState` sites in one component. Each pane module now
 * owns its own state; this context carries only what genuinely cannot be
 * pane-local.
 *
 * ## What lives here, and why
 *
 * **The resource state machine** (`useSettingsResources`). It is one hook
 * driving eight independent loads with a shared request-sequence token and
 * a coalesced preferences read. Splitting it per pane would multiply IPC
 * calls and re-break the `max(...)`-instead-of-`sum(...)` latency work in
 * issue #1935, so it stays shared and panes read `resources` from here.
 *
 * **`accounts` and `providers`.** These are the only two collections that
 * genuinely cross a pane boundary: the Providers pane renders the account
 * cards, while the Harnesses pane looks up account display names and keys
 * in `HarnessConfigList` and lists the spawn-menu order from `providers`.
 * Hoisting them here is the alternative to two panes each fetching the
 * same list independently.
 *
 * **The latest `preferences` payload.** Panes hydrate their own drafts
 * from it (see `GeneralPane` / `ProvidersPane` / `HarnessesPane`). This
 * replaces the old `onPreferencesLoaded` callback, which reached into a
 * dozen `useState` setters belonging to three different future panes.
 * The loading side effect still lives on the resource hook's loader, so a
 * superseded read is still discarded — the panes only ever see a payload
 * that already won its request token.
 *
 * ## Why one value rather than a slice per pane
 *
 * A review flagged this as a god-context: `coordinator` / `devices` /
 * `network` are read by one pane and `pairings` by another, yet all panes
 * subscribe to the same value. That was measured rather than assumed. With a
 * `MutationObserver` on the General pane's pool input — a pane the user is
 * *not* looking at — a full eight-resource settle produced **one** commit
 * on that inactive pane, not one per resource transition. React batches the
 * eight loaders' `setResource` calls into a single render pass, so the
 * fan-out the shape implies does not actually occur.
 *
 * Slicing would also fight the reason the hook is shared at all: the resource
 * machine must stay one instance for #1935's latency argument, and a
 * per-pane context would have to re-expose the same `resources` map anyway,
 * leaving two ways to read the same state — the ambiguity the engineering
 * contract warns against. A single read path is the deliberate trade, and it
 * is the one to revisit first if render cost ever becomes measurable.
 *
 * ## What deliberately does NOT live here
 *
 * The dirty-site aggregator (`dirtySites`, `siteDirtyChange`,
 * `dirtyPanes`). That is the modal's own transition state — the modal owns
 * the discard banner and the nav-rail dots, so it keeps the setter and the
 * invariant (issue #730) next to the state it guards, exactly as the
 * engineering contract requires. Panes receive `siteDirtyChange` as a
 * plain callback; they cannot read or write the set.
 */
import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from 'react';
import * as api from '../../lib/tauri';
import { useSettingsResources, type ResourceKey, type ResourceState } from './useSettingsResources';
import { getHostPairingVerifications } from './settingsUtils';
import type { ProviderAccount, ProviderInfo } from '../../lib/tauri';

export interface SettingsData {
  /** Per-resource load status, keyed by resource (issue #1534). */
  resources: Record<ResourceKey, ResourceState>;
  /** Non-null when `preferences.json` exists but could not be read (issue
   *  #1523). Gates every preference-backed control app-wide. */
  preferencesCorruption: api.CorruptionInfo | null;
  /**
   * Issue #1523: a corrupt `preferences.json` reads *successfully* (the
   * backend serves defaults so read-only surfaces keep working), so status
   * alone would leave every control enabled — and every one of them would
   * then fail on save. Gate on the corruption flag too, so one boolean
   * disables every preference-backed control in every pane.
   */
  prefsLoaded: boolean;
  /** The most recent preferences payload that won its request token.
   *  Panes seed their drafts from this; `null` until the first load
   *  succeeds. */
  preferences: api.AppPreferences | null;
  /** The most recent pairings payload that won its request token. Only
   *  the Harnesses pane renders pairings, so it mirrors this into its own
   *  local state rather than the context holding pairing state outright —
   *  this is the read model, not the pane's state. `null` until the first
   *  load succeeds. */
  pairings: PairingsPayload | null;
  /** Committed payloads for the Remote Access pane's three resources, on the
   *  same read-model footing as `preferences` / `pairings`: the pane mirrors
   *  each into its own local state. `null` until the corresponding load
   *  succeeds. */
  coordinator: CoordinatorPayload | null;
  devices: api.DeviceSession[] | null;
  network: NetworkPayload | null;

  loadPreferences: () => Promise<api.AppPreferences | null>;
  loadRouting: () => Promise<api.ProviderInfo[] | null>;
  loadProviders: () => Promise<api.ProviderInfo[] | null>;
  loadAccounts: () => Promise<{
    accountList: api.ProviderAccount[];
    catalog: api.ProviderAccount[];
    keysInPreferences: string[] | null;
  } | null>;
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
   *  reads what it needs (see `useSettingsResources` for why the old
   *  cross-resource guard was removed). */
  retryResource: (key: ResourceKey) => void;

  /** Provider accounts, shared: the Providers pane renders the cards and
   *  the Harnesses pane resolves account names for proxied rows. */
  accounts: ProviderAccount[];
  setAccounts: React.Dispatch<React.SetStateAction<ProviderAccount[]>>;
  /** The keyed first-class catalog that ships with the app. Arrives with
   *  the accounts load; only the Providers pane's add-form consumes it,
   *  but it is one IPC payload with `accounts`, so it travels with them. */
  keyedCatalog: ProviderAccount[];
  /** Issue #2154 — account ids whose API key is persisted in
   *  `preferences.json` because the OS credential store could not be used,
   *  so the Accounts pane can say so on the affected card. `null` until the
   *  probe answers, and `null` again if it fails: unknown is not safe. */
  keysInPreferences: Set<string>;
  /** The live provider list (spawn menu + routing choices), shared: the
   *  Providers pane feeds the routing pickers and the Harnesses pane lists
   *  the spawn-menu order. */
  providers: ProviderInfo[];
  setProviders: React.Dispatch<React.SetStateAction<ProviderInfo[]>>;
  /** The persisted routing choices, used as the dropdown fallback until
   *  `providers` loads. `providers` is gated on the Codex install probe
   *  chain (issue #1534), so without this fallback a cold WSL distro would
   *  show three dead pickers even though saved routing choices were ready. */
  routingProviders: ProviderInfo[];

  /** Shared error surface, rendered above every pane so a failed save is
   *  visible no matter which pane the user is looking at. Panes call
   *  `setError`; only the modal renders it. */
  error: string | null;
  setError: (message: string | null) => void;

  /** Report a dirty site to the modal's aggregator. The pane cannot clear
   *  another pane's site, and cannot clear its own on unmount without an
   *  explicit call (see `AddProviderForm`'s Cancel contract). */
  siteDirtyChange: (site: string, dirty: boolean) => void;
}

const SettingsDataContext = createContext<SettingsData | null>(null);

/** What a committed `loadPairings` produced. */
export interface PairingsPayload {
  effective: api.ProviderPairing[];
  verifications: api.PairingVerification[];
  storedKeys: Set<string>;
  compatible: Record<string, api.ProviderAccount[]>;
}

/** What a committed `loadCoordinator` produced. */
export interface CoordinatorPayload {
  enabled: boolean;
  has_token: boolean;
}

/** What a committed `loadNetwork` produced. `lan_exposure_enabled` is the
 *  persisted intent; the realized fields describe what the server actually
 *  bound. */
export interface NetworkPayload {
  lan_exposure_enabled: boolean;
  tls_active: boolean;
  exposed_interfaces: api.RealizedBind[];
}

export function useSettingsData(): SettingsData {
  const value = useContext(SettingsDataContext);
  if (!value) {
    throw new Error('useSettingsData must be used inside a SettingsDataProvider');
  }
  return value;
}

export function SettingsDataProvider({
  children,
  siteDirtyChange,
}: {
  children: ReactNode;
  /** Supplied by the modal, which owns the dirty-site set (issue #730). */
  siteDirtyChange: (site: string, dirty: boolean) => void;
}) {
  const [accounts, setAccounts] = useState<ProviderAccount[]>([]);
  const [keyedCatalog, setKeyedCatalog] = useState<ProviderAccount[]>([]);
  const [keysInPreferences, setKeysInPreferences] = useState<Set<string>>(new Set());
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [routingProviders, setRoutingProviders] = useState<ProviderInfo[]>([]);
  const [preferences, setPreferences] = useState<api.AppPreferences | null>(null);
  const [pairings, setPairings] = useState<PairingsPayload | null>(null);
  const [coordinator, setCoordinator] = useState<CoordinatorPayload | null>(null);
  const [devices, setDevices] = useState<api.DeviceSession[] | null>(null);
  const [network, setNetwork] = useState<NetworkPayload | null>(null);
  const [error, setError] = useState<string | null>(null);

  const {
    resources,
    preferencesCorruption,
    loadPreferences,
    loadRouting,
    loadProviders,
    loadAccounts,
    loadPairings,
    loadCoordinator,
    loadDevices,
    loadNetwork,
    retryResource,
  } = useSettingsResources({
    onPreferencesLoaded: (prefs) => setPreferences(prefs),
    onRoutingLoaded: setRoutingProviders,
    onPairingsLoaded: (data) => setPairings(data),
    onCoordinatorLoaded: (data) => setCoordinator(data),
    onDevicesLoaded: (list) => setDevices(list),
    onNetworkLoaded: (data) => setNetwork(data),
    onAccountsLoaded: ({ accountList, catalog, keysInPreferences }) => {
      setAccounts(accountList);
      setKeyedCatalog(Array.isArray(catalog) ? catalog : []);
      // Issue #2154 — a failed probe leaves the set empty, which hides the
      // notice rather than showing it for every account. The alternative
      // (treating unknown as "on disk") would cry wolf on every card.
      setKeysInPreferences(new Set(keysInPreferences ?? []));
    },
    onProvidersLoaded: (list) => setProviders(list),
    getHostPairingVerifications,
  });

  // Fan out the eight independent initial loads concurrently.
  // `Promise.allSettled` here is *defensive*: each `loadX` already
  // swallows its own errors via `withResourceLoad` and updates
  // `resources[key]` independently, so a rejection in one loader
  // doesn't affect the others. `allSettled` just makes the
  // intent explicit and protects against a loader that escapes
  // its catch in the future.
  //
  // Issue #1935 — pairings used to be excluded from this fan-out and
  // started only after `providers` settled, because it took its
  // harness-id list from the provider menu and issued one
  // `compatible_providers_for_harness` call per harness. It now takes
  // the whole attach-picker map from the backend in a single call, so
  // it is just another load here: the modal opens in
  // max(providers, pairings) rather than providers + pairings, and a
  // slow Codex install probe no longer delays the Harnesses pane.
  //
  // The dependency that *did* matter is gone with the loader's own use of
  // it. Pairings is also not auto-chained into `loadProviders` (issue #1534
  // round 5): account-only refreshes must not re-probe WSL harness
  // verifications.
  useEffect(() => {
    void Promise.allSettled([
      loadPreferences(),
      loadRouting(),
      loadProviders(),
      loadAccounts(),
      loadPairings(),
      loadCoordinator(),
      loadDevices(),
      loadNetwork(),
    ]);
  }, [
    loadPreferences,
    loadRouting,
    loadProviders,
    loadAccounts,
    loadPairings,
    loadCoordinator,
    loadDevices,
    loadNetwork,
  ]);

  const prefsLoaded =
    resources.preferences.status === 'loaded' && !preferencesCorruption;

  const value = useMemo<SettingsData>(
    () => ({
      resources,
      preferencesCorruption,
      prefsLoaded,
      preferences,
      pairings,
      coordinator,
      devices,
      network,
      loadPreferences,
      loadRouting,
      loadProviders,
      loadAccounts,
      loadPairings,
      loadCoordinator,
      loadDevices,
      loadNetwork,
      retryResource,
      accounts,
      setAccounts,
      keyedCatalog,
      keysInPreferences,
      providers,
      setProviders,
      routingProviders,
      error,
      setError,
      siteDirtyChange,
    }),
    [
      resources,
      preferencesCorruption,
      prefsLoaded,
      preferences,
      pairings,
      coordinator,
      devices,
      network,
      loadPreferences,
      loadRouting,
      loadProviders,
      loadAccounts,
      loadPairings,
      loadCoordinator,
      loadDevices,
      loadNetwork,
      retryResource,
      accounts,
      keyedCatalog,
      keysInPreferences,
      providers,
      routingProviders,
      error,
      siteDirtyChange,
    ],
  );

  return (
    <SettingsDataContext.Provider value={value}>
      {children}
    </SettingsDataContext.Provider>
  );
}