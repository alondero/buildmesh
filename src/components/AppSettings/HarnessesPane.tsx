/**
 * `HarnessesPane` — spawn-menu composition and per-harness defaults
 * (issue #1880): launch configurations, the spawn-menu order, the
 * Advanced Provider Routes table, and the application-level harness
 * defaults.
 *
 * This pane owns the pairing state, because pairing is a harnesses-only
 * concern: a pairing row is always a (harness, provider) edge, and nothing
 * outside this tab reads it. The account and provider *lists* those rows
 * resolve against are shared through `SettingsDataContext`.
 *
 * Every mutation is optimistic and every refresh is routed through the
 * named resource loaders (issue #1534), so a failed refresh surfaces in a
 * banner instead of leaving a green "loaded" status over stale data.
 */
import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import * as api from '../../lib/tauri';
import { formatError } from '../../lib/errorUtils';
import { HarnessOrderList } from './HarnessOrderList';
import { getOrderableHarnesses } from './harnessOrder';
import { HarnessConfigList, type ProxyHarness } from './HarnessConfigList';
import { HarnessDefaultsSection } from './HarnessDefaultsSection';
import { LaunchConfigurations } from '../Providers/LaunchConfigurations';
import { SettingsSection } from './SettingsRow';
import { ResourceLoadStatus } from './ResourceLoadStatus';
import { getHostPairingVerifications } from './settingsUtils';
import { useSettingsData, type PairingsPayload } from './SettingsDataContext';
import { isWindows } from '../../lib/platform';
import { listSpawnConfigurations, getLaunchTargets, saveSpawnConfiguration, deleteSpawnConfiguration, verifyLaunchConfiguration } from '../../lib/tauri/provider';
import type { HarnessConfigValue } from '../../types/generated/HarnessConfigValue';
import type { ProviderAccount, ProviderPairing, PairingVerification } from '../../lib/tauri';

const launchConfigurationApi = { list: listSpawnConfigurations, targets: getLaunchTargets, save: saveSpawnConfiguration, remove: deleteSpawnConfiguration, verify: verifyLaunchConfiguration };

/** Stable empty map for "preferences not loaded yet". A module constant, not
 *  a fresh `{}` per render, so deriving from it does not invalidate
 *  `HarnessDefaultsSection`'s `defaults` dependency on every render. */
const EMPTY_HARNESS_DEFAULTS: Record<string, HarnessConfigValue> = {};

/** Stable empty reads so the derived pairings views keep a constant
 *  identity while no pairing load has committed. */
const EMPTY_STORED_PAIRING_KEYS: Set<string> = new Set();
const EMPTY_COMPATIBLE_BY_HARNESS: Record<string, ProviderAccount[]> = {};

export function HarnessesPane({
  launchDirtyChange,
}: {
  /** Dirty reporter for the Launch Configurations section. Routed through the
   *  modal because the site key it uses maps back to this pane via the shared
   *  `harness-` prefix rule. */
  launchDirtyChange: (dirty: boolean) => void;
}) {
  const {
    resources,
    prefsLoaded,
    preferences,
    pairings: pairingsPayload,
    accounts,
    providers,
    setProviders,
    loadPreferences,
    loadRouting,
    loadProviders,
    loadAccounts,
    loadPairings,
    retryResource,
    setError,
    siteDirtyChange,
  } = useSettingsData();

  // Proxied Provider pairings (ADR-0025): stored only.
  //
  // `pairings` is the one piece that must be *state* rather than a derived
  // read, because the drag handler reorders it optimistically and rolls back
  // on failure. The remaining three are pure reads off the committed
  // payload and are derived inline (see below).
  const [pairings, setPairings] = useState<ProviderPairing[]>([]);
  const [pairingVerifications, setPairingVerifications] = useState<PairingVerification[]>([]);

  // Adopt each committed pairing load *during render* rather than from an
  // effect. `HarnessConfigList` decides whether to render its harness list
  // from `compatibleByHarness` and `pairings` in the same pass, so seeding
  // them a render later would render the empty "no harnesses can host a
  // proxied provider" state for a frame after a successful retry — the
  // banner disappears and the list is not there yet.
  //
  // This is React's documented adjust-state-during-render pattern: guarded
  // by the payload identity, it re-renders immediately without committing a
  // stale intermediate UI, and the resource hook only publishes a payload
  // for a read that still owned its request token.
  const [adopted, setAdopted] = useState<PairingsPayload | null>(null);
  if (pairingsPayload && pairingsPayload !== adopted) {
    setAdopted(pairingsPayload);
    setPairings(pairingsPayload.effective);
    setPairingVerifications(pairingsPayload.verifications);
  }

  // Pure reads off the committed payload — no local copy, so they cannot
  // lag a render behind the status flip that revealed the list.
  const storedPairingKeys = pairingsPayload?.storedKeys ?? EMPTY_STORED_PAIRING_KEYS;
  const compatibleByHarness = pairingsPayload?.compatible ?? EMPTY_COMPATIBLE_BY_HARNESS;
  // Issue #1150: per-harness application defaults, keyed by harness profile
  // id. Read straight off the shared preferences payload rather than copied
  // into local state by an effect: `HarnessDefaultsSection` latches each
  // card's draft on first mount and thereafter only refreshes the card's
  // `committed` snapshot, so a value that arrives a render *after* the
  // cards mount never reaches the inputs. Deriving keeps the defaults in
  // the same render as the payload, which is what the pre-split modal got
  // for free from its single `useState`.
  const harnessDefaults = preferences?.harness_defaults ?? EMPTY_HARNESS_DEFAULTS;

  // Issue #581: mirror `providers` into a ref so the optimistic spawn-order
  // rollback reads the *latest* committed value instead of whichever value
  // the render that created the closure happened to hold. Two reorders fired
  // in quick succession would otherwise both roll back to the same stale
  // snapshot.
  const providersRef = useRef(providers);
  useEffect(() => {
    providersRef.current = providers;
  }, [providers]);

  const loadDirtyChange = launchDirtyChange;

  // Re-read verifications when the backend says a pairing verification
  // changed (a verify call, or an external attach). Best-effort: a failure
  // leaves the previous list in place rather than blanking the table.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen('pairing-verification-changed', () => {
      void getHostPairingVerifications().then(setPairingVerifications);
    }).then((stop) => {
      unlisten = stop;
    });
    return () => unlisten?.();
  }, []);

  // Attach a provider to a harness (ADR-0025). Client supplies base URL and
  // (for Anthropic) model tiers; backend seeds the global key set-if-absent.
  // Issue #1534 (review round 2) — mutation handlers previously
  // re-fetched providers/accounts/pairings via raw `api.listProviders()`
  // and friends, bypassing the resource-load state machine. A
  // post-mutation refresh failure used to silently leave stale data
  // with no visible signal. Now every refresh routes through the
  // named loaders so failures show in the per-resource banners
  // exactly like a fresh-modal-open failure.
  //
  // The mutation itself (e.g. `api.attachProxiedProvider`) still
  // uses `setError` — write failures are transient and the mutation
  // caller is right there to retry. Only the *refresh* goes through
  // the state machine.
  const handleAttachProvider = async (
    harnessId: string,
    providerId: string,
    apiKey: string | null,
    baseUrl: string | null,
    modelTiers: api.ModelTiers | null,
  ) => {
    setError(null);
    try {
      await api.attachProxiedProvider(harnessId, providerId, apiKey, baseUrl, modelTiers);
      // Issue #1534 (review round 5) — attach changes the
      // providers list AND the pairings (newly attached provider
      // is now in the effective pairings set), so all three refreshes
      // are needed. Issue #1935 — they no longer chain: pairings
      // reads its own harness ids from the backend, so waiting for
      // `loadProviders` only added the provider-menu probe to the
      // latency of a user-initiated refresh.
      //
      // Four loaders, not three: `attach_proxied_provider` seeds the
      // provider's global key set-if-absent, so attaching with a key
      // flips that account from keyless to keyed. Without
      // `loadAccounts`, `HarnessConfigList`'s `isKeyed` (which reads
      // the shared accounts list) keeps reporting the provider as
      // needing a key the user just supplied, and the Providers pane's
      // account card shows a stale "no key" state until the modal is
      // reopened. Update and detach below deliberately do NOT refresh
      // accounts — neither mutates a provider's key.
      await Promise.all([loadRouting(), loadProviders(), loadAccounts(), loadPairings()]);
    } catch (e) {
      setError(formatError(e));
      throw e;
    }
  };

  const handleUpdatePairing = async (
    harnessId: string,
    providerId: string,
    baseUrl: string | null,
    modelTiers: api.ModelTiers | null,
  ) => {
    setError(null);
    try {
      await api.updateProviderPairing(harnessId, providerId, baseUrl, modelTiers);
      // Update changes pairings too (model tiers / base URL live
      // on the pairing, not on the provider). Concurrent since
      // issue #1935 — neither refresh needs the other's result.
      await Promise.all([loadRouting(), loadProviders(), loadPairings()]);
    } catch (e) {
      setError(formatError(e));
      throw e;
    }
  };

  const handleDetachProvider = async (harnessId: string, providerId: string) => {
    setError(null);
    try {
      await api.removeProviderPairing(harnessId, providerId);
      // Detach removes the pairing — both providers and pairings
      // need refresh. Concurrent since issue #1935 (same reasoning
      // as attach).
      await Promise.all([loadRouting(), loadProviders(), loadPairings()]);
    } catch (e) {
      setError(formatError(e));
      throw e;
    }
  };

  const handleVerifyPairing = async (
    harnessId: string,
    providerId: string,
    envType: api.EnvType,
  ) => {
    setError(null);
    try {
      await api.verifyProviderPairing(harnessId, providerId, envType);
      // Issue #1534 (review round 4) — verifications aren't a
      // tracked resource (they live in `pairingVerifications`
      // alongside pairings, but this pane doesn't surface them
      // independently). The full `loadPairings()` would also
      // re-query pairings + compatibility, which is wasteful for
      // a verify-only operation. We keep this as a focused,
      // best-effort side-channel refresh — a failure here falls
      // back to an empty list and the pane continues rendering
      // pairings via the resource state machine.
      setPairingVerifications(await getHostPairingVerifications());
    } catch (e) {
      setError(formatError(e));
      setPairingVerifications(await getHostPairingVerifications().catch(() => []));
      throw e;
    }
  };

  // Issue #577 — persist the per-harness Proxied Provider child order.
  // Optimistically reorders the local `pairings` slice for the affected
  // harness (cross-harness drag is disallowed at the UI layer — each
  // `HarnessCard` is its own `DndContext`, so the harnessId here always
  // matches the card the drag started in). Rolls back on failure so the
  // visible order never lies about what was stored. The backend emits
  // `provider-list-changed`, so the sidebar / probes re-read live — same
  // pattern as the spawn-menu reorder.
  const handleReorderProxiedProviders = async (
    harnessId: string,
    newProviderIds: string[],
  ) => {
    const previous = pairings;
    // Partition the prior pairings once: this harness's children go through
    // the reorder; every other harness's pairings stay put.
    const within = previous.filter((p) => p.harness_id === harnessId);
    const outside = previous.filter((p) => p.harness_id !== harnessId);
    // Fallback to the prior index for any id the user didn't touch (a
    // dnd-kit drag only reorders the items the user interacted with, so
    // the dragged subset is the only re-ranked input — but a paired
    // provider that's currently rendered but not in `newProviderIds`
    // would otherwise land at the very bottom by accident).
    const previousIndex = new Map(within.map((p, i) => [p.provider_id, i]));
    const rank = new Map(newProviderIds.map((id, i) => [id, i]));
    const reorderedWithin = [...within].sort(
      (a, b) =>
        (rank.get(a.provider_id) ?? previousIndex.get(a.provider_id) ?? 0) -
        (rank.get(b.provider_id) ?? previousIndex.get(b.provider_id) ?? 0),
    );
    setPairings([...outside, ...reorderedWithin]);
    setError(null);
    try {
      await api.setProxiedProviderOrder(harnessId, newProviderIds);
    } catch (e) {
      setPairings(previous);
      setError(formatError(e));
    }
  };

  // Persist a new spawn-menu harness order (issue #573). Optimistically reorder
  // the shared `providers` list to match — keeping any non-listed rows
  // (Terminal) appended at the end, exactly as the backend re-derives them —
  // and roll back on failure so the visible order never lies about what was
  // stored. The backend emits `provider-list-changed`, so the sidebar /
  // probes re-read live.
  //
  // `previous` is read from `providersRef` (issue #581) so a handler running
  // against a stale closure still rolls back to the latest committed state.
  const handleReorderHarnesses = async (order: string[]) => {
    const previous = providersRef.current;
    const reordered = [
      ...order.flatMap(id => previous.filter(p => p.harness_id === id)),
      ...previous.filter(p => !order.includes(p.harness_id)),
    ];
    setProviders(reordered);
    setError(null);
    try {
      await api.setHarnessOrder(order);
    } catch (e) {
      setProviders(previous);
      setError(formatError(e));
    }
  };

  // Issue #1150 / #1148: persist one harness's application default.
  // Returns `true` on success so the section's optimistic card can commit
  // the draft into its `committed` snapshot; `false` on failure rolls
  // back the draft to the last confirmed value AND surfaces the shared
  // error banner (issue #1148 acceptance criteria 7).
  const handleSetHarnessDefault = async (
    profileId: string,
    value: HarnessConfigValue,
  ): Promise<boolean> => {
    setError(null);
    try {
      await api.setHarnessDefault(profileId, value);
      // Issue #1534 (review round 4) — route the post-write
      // preferences refresh through the loader so a
      // `get_app_preferences` rejection flips `prefsState` to
      // `failed` and surfaces the banner, instead of leaving the
      // optimistic `harness_defaults` commit silently stale.
      await loadPreferences();
      return true;
    } catch (e) {
      setError(formatError(e));
      return false;
    }
  };

  const handleClearHarnessDefault = async (profileId: string): Promise<boolean> => {
    setError(null);
    try {
      await api.clearHarnessDefault(profileId);
      // Issue #1534 (review round 5) — refresh via the loader so
      // a failed `get_app_preferences` (or a backend normalisation
      // that other harness defaults changed alongside) surfaces
      // in the preferences banner rather than leaving the optimistic
      // `harness_defaults` mutation silently stale.
      await loadPreferences();
      return true;
    } catch (e) {
      setError(formatError(e));
      return false;
    }
  };

  return (
    <>
      <SettingsSection title="Launch Configurations"><LaunchConfigurations api={launchConfigurationApi} onDirtyChange={loadDirtyChange} onChanged={() => { void loadRouting(); void loadProviders(); }} refreshToken={providers} /></SettingsSection>
      {/* Issue #1534 — the Agent Harness defaults section below is
          preferences-backed, so a failed preferences load must be visible
          here rather than silently disabling the per-harness inputs. */}
      {resources.preferences.status === 'failed' && (
        <ResourceLoadStatus
          resource="preferences"
          state={resources.preferences}
          onRetry={() => retryResource('preferences')}
        />
      )}
      {getOrderableHarnesses(providers).length >= 2 && (
        <SettingsSection title="Spawn menu order">
          <p className="pb-2 text-sm text-text-muted">
            Drag to reorder how harnesses appear in every spawn menu. Terminal stays pinned last.
          </p>
          <HarnessOrderList
            providers={providers}
            onReorder={handleReorderHarnesses}
            disabled={!prefsLoaded}
          />
        </SettingsSection>
      )}

      <SettingsSection
        title="Advanced Provider Routes"
        description={
          <>
            Proxy a model provider through a harness over a compatible API surface
            (e.g. MiniMax via Claude Code over Anthropic, or via Codex over OpenAI).
            Set the provider&apos;s API key on the Providers page; set base URL and
            model names here when attaching.
          </>
        }
      >
        {/* Issue #1534 (review round 2) — `idle` is now an explicit
            branch. Without it, an in-flight tab switch to the
            Harnesses pane (while `pairings` was still waiting on
            `providers`) fell through to the empty `HarnessConfigList`
            and flashed "no compatible providers" — the exact bug
            #1534 aimed to fix. Order of checks:
              1. providers failed      → providers error + retry
              2. pairings failed       → pairings error + retry
              3. pairings idle/loading → "Loading pairings…" (covers
                                         the initial-load window)
              4. otherwise             → HarnessConfigList (real data) */}
        {resources.providers.status === 'failed' ? (
          <ResourceLoadStatus
            resource="providers"
            state={resources.providers}
            onRetry={() => retryResource('providers')}
          />
        ) : resources.pairings.status === 'failed' ? (
          <ResourceLoadStatus
            resource="pairings"
            state={resources.pairings}
            onRetry={() => retryResource('pairings')}
          />
        ) : resources.pairings.status === 'idle' || resources.pairings.status === 'loading' ? (
          <ResourceLoadStatus
            resource="pairings"
            state={resources.pairings}
          />
        ) : (
          <HarnessConfigList
            harnesses={
              [...new Map(providers.filter((p) => p.harness_id !== 'terminal')
                .map((p) => [p.harness_id, { id: p.harness_id, label: p.harness_id }])).values()] as ProxyHarness[]
            }
            compatibleByHarness={compatibleByHarness}
            pairings={pairings}
            verifications={pairingVerifications}
            supportsWsl={isWindows}
            storedKeys={storedPairingKeys}
            accounts={accounts}
            onAttach={handleAttachProvider}
            onUpdate={handleUpdatePairing}
            onDetach={handleDetachProvider}
            onVerify={handleVerifyPairing}
            onReorderProxied={handleReorderProxiedProviders}
            onDirtyChange={(site, d) => siteDirtyChange(`harness-${site}`, d)}
            disabled={!prefsLoaded}
          />
        )}
      </SettingsSection>

      {/* Issue #1150 / #1148: application-level Agent Harness defaults.
          On-dirty mirrors into the modal's discard-confirm via
          `harness-defaults`, which maps to this pane. */}
      <HarnessDefaultsSection
        providers={providers}
        defaults={harnessDefaults}
        onChange={handleSetHarnessDefault}
        onReset={handleClearHarnessDefault}
        onDirtyChange={(d) => siteDirtyChange('harness-defaults', d)}
        disabled={!prefsLoaded}
      />
    </>
  );
}