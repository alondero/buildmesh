import { useState, useEffect } from 'react';
import { ProviderIcon } from '../Providers/ProviderIcon';
import { HarnessOrderList } from './HarnessOrderList';
import { HarnessConfigList, type ProxyHarness } from './HarnessConfigList';
import * as api from '../../lib/tauri';
import type {
  ProviderInfo,
  ProviderUsage,
  ProviderMeters,
  UsageWindow,
  ProviderAccount,
  ProviderPairing,
  BillingBalance,
  DeviceSession,
  RealizedBind,
} from '../../lib/tauri';

interface AppSettingsModalProps {
  onClose: () => void;
}

const NO_OVERRIDE = '__no_override__';

// Built-ins can only be disabled, never removed (a "remove" just reverts them to
// the code default), so we hide the Remove action for these ids. Kept in sync with
// `preferences::default_provider_accounts`.
const BUILTIN_PROVIDER_IDS = ['anthropic', 'codex', 'agy', 'minimax', 'kimi'];

// The five Claude model aliases a Claude-compatible provider can pin (issue #567).
// `key` is the ProviderAccount.model_tiers field; `label` is the UI caption.
const MODEL_TIER_FIELDS: { key: keyof ProviderAccount['model_tiers']; label: string }[] = [
  { key: 'default', label: 'Default model' },
  { key: 'opus', label: 'Opus' },
  { key: 'sonnet', label: 'Sonnet' },
  { key: 'haiku', label: 'Haiku' },
  { key: 'small_fast', label: 'Small / fast' },
];

export function UsageBar({ window }: { window: UsageWindow }) {
  const percent = window.usedPercent ?? 0;
  const color = percent > 80 ? 'bg-status-error' : percent > 60 ? 'bg-status-warning' : 'bg-accent-cyan';
  // Show the figure whenever it's known — 0% (full quota remaining) is a real
  // value, not missing data. Only a null usedPercent is "N/A".
  const display = window.usedPercent != null ? `${percent.toFixed(1)}%` : 'N/A';
  return (
    <div className="mt-2">
      <div className="flex justify-between text-base text-text-muted mb-1">
        <span>{window.label}</span>
        <span>{display}</span>
      </div>
      <div className="h-3 bg-bg-card rounded-full overflow-hidden">
        <div className={`h-full ${color} rounded-full`} style={{ width: `${Math.min(percent, 100)}%` }} />
      </div>
      {window.resetsAt && (
        <p className="text-sm text-text-muted mt-1">Resets: {new Date(window.resetsAt).toLocaleString()}</p>
      )}
    </div>
  );
}

/** Cash-balance view for a pay-as-you-go account (issue #537) — shown instead of
 *  percentage bars. */
export function BalanceCard({ balance }: { balance: BillingBalance }) {
  const fmt = (n: number) => `${balance.currency} ${n.toFixed(2)}`;
  return (
    <div className="mt-2 space-y-1">
      <div className="flex justify-between text-base">
        <span className="text-text-muted">Balance remaining</span>
        <span className="font-medium text-text-primary">{fmt(balance.remaining)}</span>
      </div>
      {balance.monthlySpend != null && (
        <div className="flex justify-between text-base">
          <span className="text-text-muted">Spent this month</span>
          <span className="text-text-primary">{fmt(balance.monthlySpend)}</span>
        </div>
      )}
    </div>
  );
}

/** One Model Provider on the Providers page: enable toggle, collapsible
 *  credential editor, and its Usage Meters. A provider may expose several meters
 *  at once (quota windows AND a cash balance), so all present meters render — not
 *  one-or-the-other by billing mode (issue #574, AC3). `usageTracked === false`
 *  marks a Generic provider Buildmesh can't fetch usage for (AC4). */
export function AccountCard({
  account,
  usage,
  usageTracked = true,
  usageLoading,
  onSave,
  onRemove,
}: {
  account: ProviderAccount;
  usage?: ProviderUsage;
  /** Whether Buildmesh has a usage fetcher for this provider; false → Generic
   *  provider, shown as an explicit "usage not tracked" state. Defaults true. */
  usageTracked?: boolean;
  usageLoading: boolean;
  onSave: (account: ProviderAccount) => Promise<boolean>;
  onRemove?: (id: string) => Promise<void>;
}) {
  const [draft, setDraft] = useState<ProviderAccount>(account);
  const [showCreds, setShowCreds] = useState(false);
  const [busy, setBusy] = useState(false);
  // Two-step remove: first click flips to a confirm pair, second click fires.
  // Reset whenever the card re-renders for a different account so a stale
  // confirm doesn't survive a save-and-reload.
  const [confirmingRemove, setConfirmingRemove] = useState(false);

  // Re-sync the editable draft when the parent reloads accounts (e.g. after save).
  useEffect(() => setDraft(account), [account]);
  useEffect(() => setConfirmingRemove(false), [account.id]);

  const isCustom = !BUILTIN_PROVIDER_IDS.includes(account.id);
  // Self-authenticating harnesses (Anthropic/Codex/Antigravity) hold no creds in
  // Buildmesh, so they show no credential or model-tier fields (#568a). Only
  // Claude-compatible keyed providers (MiniMax/Kimi/custom) do.
  const showCredentials = account.claude_compatible;
  // Keyed providers (MiniMax/Kimi/custom) authenticate with an API key entered
  // here; native providers (Anthropic/Codex/Antigravity) self-authenticate via
  // their own CLI. That split — not billing mode — drives the logged-out copy.
  const keyed = account.claude_compatible;

  const setTier = (key: keyof ProviderAccount['model_tiers'], value: string) =>
    setDraft({ ...draft, model_tiers: { ...draft.model_tiers, [key]: value || null } });

  const toggleEnabled = async (enabled: boolean) => {
    setBusy(true);
    try {
      await onSave({ ...account, enabled });
    } finally {
      setBusy(false);
    }
  };

  const saveDraft = async () => {
    setBusy(true);
    try {
      await onSave(draft);
    } finally {
      setBusy(false);
    }
  };

  // Two-step remove: the Yes click must gate on `busy` for the full duration
  // of the async onRemove, otherwise a fast double-click on Remove fires two
  // concurrent IPCs and races on setAccounts (the parent's handleRemoveAccount
  // never sets busy itself — keeping it card-local is simpler than threading
  // a "removing id" prop through from the modal). Awaiting the parent's
  // promise also means a failed remove flips the card back to its idle state
  // instead of leaving the confirming pair stuck open. The catch is a no-op
  // for state but necessary: without it, the async function's returned
  // promise rejects, which user.click does NOT rethrow, leaving an
  // unhandled-rejection warning in the console for every failure path.
  const confirmRemove = async () => {
    if (busy) return;
    setBusy(true);
    setConfirmingRemove(false);
    try {
      await onRemove?.(account.id);
    } catch {
      // The parent (handleRemoveAccount) shows the error toast via its own
      // catch — we only own busy-state here.
    } finally {
      setBusy(false);
    }
  };

  const renderUsage = () => {
    if (!account.enabled) return <p className="text-base text-text-muted">Disabled</p>;
    // Generic providers have no usage integration — say so explicitly rather than
    // showing an empty gauge or a misleading error (AC4).
    if (!usageTracked) {
      return (
        <div>
          <p className="text-base text-text-muted">Usage not tracked</p>
          <p className="text-sm text-text-muted mt-1">
            Buildmesh has no usage integration for {account.name}.
          </p>
        </div>
      );
    }
    if (!usage && !usageLoading) return <p className="text-base text-text-muted">Unable to load usage data</p>;
    if (!usage) return null;
    if (!usage.loggedIn) {
      return (
        <div>
          <p className="text-base text-status-warning">{keyed ? 'No API key' : 'Not logged in'}</p>
          <p className="text-sm text-text-muted mt-1">
            {keyed ? `Enter an API key for ${account.name} above` : `Run the ${account.name} CLI login first`}
          </p>
        </div>
      );
    }
    if (usage.error) return <p className="text-base text-status-error">{usage.error}</p>;
    // Render every Usage Meter the provider exposes — quota windows AND a cash
    // balance can both be present, so show all rather than choosing one by
    // billing mode (AC3). This also unhides MiniMax's quota bars, which the old
    // billing-mode XOR suppressed because its account is pay-as-you-go but its
    // fetcher returns percentage windows.
    const hasMeters = usage.windows.length > 0 || usage.balance != null;
    return (
      <div>
        {usage.windows.map(w => (
          <UsageBar key={w.label} window={w} />
        ))}
        {usage.balance && <BalanceCard balance={usage.balance} />}
        {!hasMeters && <p className="text-sm text-text-muted">No usage data</p>}
        {usage.detail && <p className="text-sm text-accent-cyan mt-2">{usage.detail}</p>}
      </div>
    );
  };

  return (
    <div className="border border-border-subtle rounded-lg p-5">
      <div className="flex items-center gap-3 mb-3">
        <ProviderIcon providerId={account.id} className="h-6 w-6" />
        <span className="text-lg font-medium text-text-primary">{account.name}</span>
        <div className="ml-auto flex items-center gap-3">
          {usageLoading && account.enabled && (
            <span className="text-base text-text-muted">Loading...</span>
          )}
          <label className="flex items-center gap-2 text-base text-text-secondary cursor-pointer">
            <input
              type="checkbox"
              checked={account.enabled}
              disabled={busy}
              onChange={e => toggleEnabled(e.target.checked)}
              className="accent-accent-cyan h-4 w-4 disabled:opacity-50"
              aria-label={`Enable ${account.name}`}
            />
            <span>Enabled</span>
          </label>
          {isCustom && onRemove && (confirmingRemove ? (
            <div className="flex items-center gap-2">
              <span className="text-sm text-status-error">Remove {account.name}?</span>
              <button
                onClick={confirmRemove}
                disabled={busy}
                className="px-3 py-1 bg-status-error text-white text-sm rounded hover:bg-status-error/90 disabled:opacity-50"
                aria-label={`Confirm remove ${account.name}`}
              >
                Yes
              </button>
              <button
                onClick={() => setConfirmingRemove(false)}
                disabled={busy}
                className="px-3 py-1 bg-bg-card text-text-secondary text-sm rounded hover:bg-bg-card/70 disabled:opacity-50"
                aria-label={`Cancel remove ${account.name}`}
              >
                No
              </button>
            </div>
          ) : (
            <button
              onClick={() => setConfirmingRemove(true)}
              disabled={busy}
              className="px-3 py-1 bg-status-error/15 text-status-error text-sm rounded hover:bg-status-error/25 disabled:opacity-50"
              aria-label={`Remove ${account.name}`}
              title={`Remove ${account.name}`}
            >
              Remove
            </button>
          ))}
        </div>
      </div>

      {renderUsage()}

      {showCredentials && (
        <button
          onClick={() => setShowCreds(v => !v)}
          className="mt-3 text-sm text-accent-cyan hover:text-accent-cyan/80"
        >
          {showCreds ? 'Hide credentials' : 'Edit credentials'}
        </button>
      )}

      {showCredentials && showCreds && (
        <div className="mt-3 space-y-3">
          <div>
            <label className="block text-sm text-text-muted mb-1">API key</label>
            <input
              type="password"
              value={draft.api_key ?? ''}
              onChange={e => setDraft({ ...draft, api_key: e.target.value || null })}
              placeholder="Enter API key..."
              className="w-full bg-bg-card border border-border-subtle rounded px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
              aria-label={`${account.name} API key`}
            />
          </div>
          <div>
            <label className="block text-sm text-text-muted mb-1">Base URL</label>
            <input
              type="text"
              value={draft.base_url ?? ''}
              onChange={e => setDraft({ ...draft, base_url: e.target.value || null })}
              placeholder="https://api.example.com/anthropic"
              className="w-full bg-bg-card border border-border-subtle rounded px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
              aria-label={`${account.name} base URL`}
            />
          </div>
          <div>
            <label className="block text-sm text-text-muted mb-1">Models</label>
            <p className="text-sm text-text-muted mb-2">
              Which model backs each Claude tier. Background tasks use small / fast.
            </p>
            <div className="space-y-2">
              {MODEL_TIER_FIELDS.map(({ key, label }) => (
                <div key={key} className="flex items-center gap-3">
                  <span className="w-28 shrink-0 text-sm text-text-muted">{label}</span>
                  <input
                    type="text"
                    value={draft.model_tiers[key] ?? ''}
                    onChange={e => setTier(key, e.target.value)}
                    placeholder="model id"
                    className="flex-1 min-w-0 bg-bg-card border border-border-subtle rounded px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
                    aria-label={`${account.name} ${label} model`}
                  />
                </div>
              ))}
            </div>
          </div>
          <div>
            <label className="block text-sm text-text-muted mb-1">Billing</label>
            <select
              value={draft.billing_mode}
              onChange={e => setDraft({ ...draft, billing_mode: e.target.value as ProviderAccount['billing_mode'] })}
              className="w-full bg-bg-card border border-border-subtle rounded px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
              aria-label={`${account.name} billing mode`}
            >
              <option value="plan">Plan / subscription (percentage)</option>
              <option value="pay_as_you_go">Pay-as-you-go (balance)</option>
            </select>
          </div>
          <div className="flex gap-3">
            <button
              onClick={saveDraft}
              disabled={busy}
              className="px-5 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded hover:bg-accent-cyan/30 disabled:opacity-50"
            >
              {busy ? 'Saving...' : 'Save'}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

/** Inline form to create a custom Claude-compatible provider (AC2). */
export function AddCustomProviderForm({
  onAdd,
  onCancel,
}: {
  onAdd: (name: string, baseUrl: string, apiKey: string) => Promise<void>;
  onCancel: () => void;
}) {
  const [name, setName] = useState('');
  const [baseUrl, setBaseUrl] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    setBusy(true);
    try {
      await onAdd(name, baseUrl, apiKey);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mt-4 border border-border-subtle rounded-lg p-5 space-y-3">
      <p className="text-base text-text-secondary">
        Custom Claude-compatible provider — pairs the <span className="font-mono">claude</span>{' '}
        harness with your endpoint (e.g. "DeepSeek via Claude Code").
      </p>
      <input
        type="text"
        value={name}
        onChange={e => setName(e.target.value)}
        placeholder="Name (e.g. DeepSeek via Claude Code)"
        className="w-full bg-bg-card border border-border-subtle rounded px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
        aria-label="Custom provider name"
      />
      <input
        type="text"
        value={baseUrl}
        onChange={e => setBaseUrl(e.target.value)}
        placeholder="Base URL (https://api.deepseek.com/anthropic)"
        className="w-full bg-bg-card border border-border-subtle rounded px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
        aria-label="Custom provider base URL"
      />
      <input
        type="password"
        value={apiKey}
        onChange={e => setApiKey(e.target.value)}
        placeholder="API key"
        className="w-full bg-bg-card border border-border-subtle rounded px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
        aria-label="Custom provider API key"
      />
      <div className="flex gap-3">
        <button
          onClick={submit}
          // A custom Claude-compatible provider needs all three: without a base
          // URL + key it can't reach its endpoint, and the spawn menu only shows
          // keyed providers — a keyless add would save a row that never appears.
          disabled={busy || !name.trim() || !baseUrl.trim() || !apiKey.trim()}
          className="px-5 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded hover:bg-accent-cyan/30 disabled:opacity-50"
        >
          {busy ? 'Adding...' : 'Add provider'}
        </button>
        <button
          onClick={onCancel}
          disabled={busy}
          className="px-5 py-2 text-base text-text-muted hover:text-text-secondary disabled:opacity-50"
        >
          Cancel
        </button>
      </div>
    </div>
  );
}

export function AppSettingsModal({ onClose }: AppSettingsModalProps) {
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [selected, setSelected] = useState<string>(NO_OVERRIDE);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [meters, setMeters] = useState<ProviderMeters[]>([]);
  const [usageLoading, setUsageLoading] = useState(false);
  const [accounts, setAccounts] = useState<ProviderAccount[]>([]);
  const [addingCustom, setAddingCustom] = useState(false);
  // Proxied Provider pairings (issue #576). `pairings` is the effective set
  // (derived defaults + stored extras) shown per harness; `storedPairingKeys`
  // marks which are user-stored (detachable) vs derived (managed on Providers
  // page). `compatibleByHarness` drives the surface-matched attach picker.
  const [pairings, setPairings] = useState<ProviderPairing[]>([]);
  const [storedPairingKeys, setStoredPairingKeys] = useState<Set<string>>(new Set());
  const [compatibleByHarness, setCompatibleByHarness] = useState<Record<string, ProviderAccount[]>>({});
  const [coordEnabled, setCoordEnabled] = useState(false);
  const [coordHasToken, setCoordHasToken] = useState(false);
  const [coordToken, setCoordToken] = useState<string | null>(null);
  const [coordBusy, setCoordBusy] = useState(false);
  const [coordCopied, setCoordCopied] = useState(false);
  const [devices, setDevices] = useState<DeviceSession[]>([]);
  const [confirmingRevokeId, setConfirmingRevokeId] = useState<number | null>(null);
  const [revokingId, setRevokingId] = useState<number | null>(null);
  const [lanEnabled, setLanEnabled] = useState(false);
  const [lanBusy, setLanBusy] = useState(false);
  // Realized exposure (issue #586). Mirrors `lanEnabled` (DB intent) until a
  // mismatch is detected — `lanEnabled=true` with no interfaces means the
  // toggle is on but the server is still loopback-only (TLS init failure, no
  // interface, per-interface bind error). The UI renders a warning so the
  // user knows their phone URL won't actually connect.
  const [tlsActive, setTlsActive] = useState(false);
  const [exposedInterfaces, setExposedInterfaces] = useState<RealizedBind[]>(
    [],
  );

  // Re-read network status after a toggle completes so the realized state
  // reflects the post-rebind listeners. Without this the user flips the
  // switch, the optimistic `lanEnabled` flips, but the realized fields stay
  // stale until the next modal open. We deliberately do NOT re-write
  // `lanEnabled` here — the optimistic value is the source of truth between
  // writes; clobbering it from a possibly-stale read would let a rapid
  // double-toggle race (toggle A's refresh can land after toggle B's
  // optimistic flip and revert B).
  const refreshNetworkStatus = async () => {
    try {
      const network = await api.getNetworkStatus();
      setTlsActive(network.tls_active);
      setExposedInterfaces(network.exposed_interfaces);
    } catch (e) {
      // Non-fatal — the toggle still reflects the optimistic value. Just log
      // so a future debugging session can correlate the gap.
      console.error('Failed to refresh network status:', e);
    }
  };

  useEffect(() => {
    const init = async () => {
      try {
        const [prefs, providerList, accountList, coord, deviceList, network] = await Promise.all([
          api.getAppPreferences(),
          api.listProviders(),
          api.getProviderAccounts(),
          api.getCoordinatorStatus(),
          api.listDeviceSessions(),
          api.getNetworkStatus(),
        ]);
        setProviders(providerList);
        setAccounts(accountList);
        loadPairingData(providerList);
        const stored = prefs.default_provider;
        setSelected(stored && stored.length > 0 ? stored : NO_OVERRIDE);
        setCoordEnabled(coord.enabled);
        setCoordHasToken(coord.has_token);
        setDevices(deviceList);
        setLanEnabled(network.lan_exposure_enabled);
        // Realized bind state from the live ServerListeners (issue #586).
        setTlsActive(network.tls_active);
        setExposedInterfaces(network.exposed_interfaces);
        setLoaded(true);
        fetchUsage();
      } catch (e) {
        setError(String(e));
        setLoaded(true);
      }
    };
    init();
  }, []);

  // Revoke a paired device: optimistically drop it from the list, then call the
  // backend (which deletes the row and force-closes any live socket it holds).
  // Roll back on failure so the list never lies about what's still authorized.
  const handleRevokeDevice = async (id: number) => {
    const previous = devices;
    setConfirmingRevokeId(null);
    setRevokingId(id);
    setError(null);
    setDevices(prev => prev.filter(d => d.id !== id));
    try {
      await api.revokeDeviceSession(id);
    } catch (e) {
      setDevices(previous);
      setError(String(e));
    } finally {
      setRevokingId(null);
    }
  };

  const fetchUsage = async (force = false) => {
    setUsageLoading(true);
    try {
      const data = await api.getProviderMeters(force);
      setMeters(data);
    } catch (e) {
      console.error('Failed to fetch usage:', e);
    } finally {
      setUsageLoading(false);
    }
  };

  const handleRefresh = () => fetchUsage(true);

  // Load the Proxied Provider pairing data (issue #576): the effective pairings
  // (derived + stored), the stored-key set (detachable vs derived), and the
  // surface-matched compatible-provider lists per native harness. Takes the
  // provider list explicitly so it can run from `init` before `providers` state
  // settles, and re-run after an attach/detach.
  const loadPairingData = async (providerList: ProviderInfo[]) => {
    try {
      const [effective, prefs] = await Promise.all([
        api.getProviderPairings(),
        api.getAppPreferences(),
      ]);
      // Defensive: the real backend always returns arrays, but a malformed
      // response shouldn't crash the settings modal.
      setPairings(Array.isArray(effective) ? effective : []);
      const stored = prefs.provider_pairings ?? [];
      setStoredPairingKeys(new Set(stored.map((p) => `${p.harness_id}:${p.provider_id}`)));
      const nativeHarnesses = providerList.filter((p) => !p.is_proxied && p.id !== 'terminal');
      const entries = await Promise.all(
        nativeHarnesses.map(async (h) => {
          const list = await api.compatibleProvidersForHarness(h.id);
          return [h.id, Array.isArray(list) ? list : []] as const;
        }),
      );
      setCompatibleByHarness(Object.fromEntries(entries));
    } catch (e) {
      console.error('Failed to load pairing data:', e);
    }
  };

  // Attach a provider to a harness (issue #576). The backend derives the
  // endpoint from the harness's surface and seeds the global key set-if-absent;
  // we then reload pairings + the merged accounts/providers so the new row and
  // any seeded key are reflected. Errors propagate to the card (which keeps its
  // form open) AND surface a toast here.
  const handleAttachProvider = async (
    harnessId: string,
    providerId: string,
    apiKey: string | null,
  ) => {
    setError(null);
    try {
      await api.attachProxiedProvider(harnessId, providerId, apiKey);
      const [providerList, accountList] = await Promise.all([
        api.listProviders(),
        api.getProviderAccounts(),
      ]);
      setProviders(providerList);
      setAccounts(accountList);
      await loadPairingData(providerList);
    } catch (e) {
      setError(String(e));
      throw e;
    }
  };

  const handleDetachProvider = async (harnessId: string, providerId: string) => {
    setError(null);
    try {
      await api.removeProviderPairing(harnessId, providerId);
      const providerList = await api.listProviders();
      setProviders(providerList);
      await loadPairingData(providerList);
    } catch (e) {
      setError(String(e));
      throw e;
    }
  };

  // Persist a new spawn-menu harness order (issue #573). Optimistically reorder
  // the local `providers` list to match — keeping any non-listed rows (Terminal)
  // appended at the end, exactly as the backend re-derives them — and roll back
  // on failure so the visible order never lies about what was stored. The
  // backend emits `provider-list-changed`, so the sidebar / probes re-read live.
  const handleReorderHarnesses = async (order: string[]) => {
    const previous = providers;
    const byId = new Map(providers.map(p => [p.id, p]));
    const reordered = [
      ...(order.map(id => byId.get(id)).filter(Boolean) as ProviderInfo[]),
      ...providers.filter(p => !order.includes(p.id)),
    ];
    setProviders(reordered);
    setError(null);
    try {
      await api.setHarnessOrder(order);
    } catch (e) {
      setProviders(previous);
      setError(String(e));
    }
  };

  const handleSave = async (newValue: string) => {
    const previous = selected;
    setSelected(newValue);
    setSaving(true);
    setError(null);
    try {
      const providerArg = newValue === NO_OVERRIDE ? null : newValue;
      await api.setAppDefaultProvider(providerArg);
    } catch (e) {
      setSelected(previous);
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  // Persist an account, then reload the merged list + usage so the card reflects
  // the new enabled/billing state. Rolls the local list back on failure so the
  // toggle never lies about what the backend stored.
  // Returns whether the save succeeded so callers (e.g. the add-custom form) can
  // keep their UI open on failure instead of dismissing over the error.
  const handleSaveAccount = async (account: ProviderAccount): Promise<boolean> => {
    const previous = accounts;
    setAccounts(prev => prev.map(a => (a.id === account.id ? account : a)));
    setError(null);
    try {
      await api.upsertProviderAccount(account);
      // Independent reads — fetch in parallel. The upsert may register a paired
      // harness profile, so re-read the provider catalogue too, keeping this
      // modal's default-provider dropdown in step with the new entry (#534).
      const [accountList, providerList] = await Promise.all([
        api.getProviderAccounts(),
        api.listProviders(),
      ]);
      setAccounts(accountList);
      setProviders(providerList);
      fetchUsage(true);
      return true;
    } catch (e) {
      setAccounts(previous);
      setError(String(e));
      return false;
    }
  };

  const handleRemoveAccount = async (id: string) => {
    setError(null);
    try {
      await api.removeProviderAccount(id);
      const [accountList, providerList] = await Promise.all([
        api.getProviderAccounts(),
        api.listProviders(),
      ]);
      setAccounts(accountList);
      setProviders(providerList);
      fetchUsage(true);
    } catch (e) {
      setError(String(e));
    }
  };

  // Create a custom Claude-compatible account (AC2). The backend also registers a
  // paired harness profile so it shows up in spawn menus. `id` is slugified from
  // the name; a blank or colliding name is rejected up front.
  const handleAddCustom = async (name: string, baseUrl: string, apiKey: string) => {
    const id = name.trim().toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '');
    if (!id) {
      setError('Custom provider needs a name');
      return;
    }
    if (accounts.some(a => a.id === id)) {
      setError(`A provider with id "${id}" already exists`);
      return;
    }
    const ok = await handleSaveAccount({
      id,
      name: name.trim(),
      enabled: true,
      // Backend re-derives this from the id; set it so the optimistic local row
      // shows credential + model-tier fields before the reload lands.
      claude_compatible: true,
      billing_mode: 'pay_as_you_go',
      api_key: apiKey.trim() || null,
      base_url: baseUrl.trim() || null,
      model_tiers: { default: null, small_fast: null, sonnet: null, opus: null, haiku: null },
      models: [],
    });
    // Keep the form open (with the user's entries) if the backend rejected it.
    if (ok) setAddingCustom(false);
  };

  // Flip the master kill-switch. Optimistic, with rollback on failure so the
  // toggle never lies about the backend's real state.
  const handleToggleCoordinator = async (enabled: boolean) => {
    const previous = coordEnabled;
    setCoordEnabled(enabled);
    setCoordBusy(true);
    setError(null);
    try {
      await api.setCoordinatorApiEnabled(enabled);
    } catch (e) {
      setCoordEnabled(previous);
      setError(String(e));
    } finally {
      setCoordBusy(false);
    }
  };

  // Flip LAN/VPN exposure. The backend rebinds the listeners live (loopback
  // plain HTTP ⇄ LAN interfaces over self-signed TLS), so we await the call and
  // roll back the toggle if it fails. `user.click` swallows a rejected async
  // onClick, so the try/catch/finally must live inside the handler. On success
  // we re-read the network status so the realized-state UI (`tls_active`,
  // `exposed_interfaces`) reflects the new bind (issue #586).
  const handleToggleLanExposure = async (enabled: boolean) => {
    const previous = lanEnabled;
    setLanEnabled(enabled);
    setLanBusy(true);
    setError(null);
    try {
      await api.setLanExposureEnabled(enabled);
      await refreshNetworkStatus();
    } catch (e) {
      setLanEnabled(previous);
      setError(String(e));
    } finally {
      setLanBusy(false);
    }
  };

  // Mint (or replace) the read token. The value is returned exactly once, here —
  // get_coordinator_status only ever reports whether one exists, never its value.
  const handleGenerateToken = async () => {
    setCoordBusy(true);
    setCoordCopied(false);
    setError(null);
    try {
      const token = await api.generateCoordinatorReadToken();
      setCoordToken(token);
      setCoordHasToken(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setCoordBusy(false);
    }
  };

  const handleCopyToken = async () => {
    if (!coordToken) return;
    try {
      await navigator.clipboard.writeText(coordToken);
      setCoordCopied(true);
      // Let the "Copied!" confirmation fade back so a second copy reads clearly.
      setTimeout(() => setCoordCopied(false), 2000);
    } catch (e) {
      setError(String(e));
    }
  };

  const accountById = (id: string) => accounts.find(a => a.id === id);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center" onClick={onClose}>
      <div className="absolute inset-0 bg-black/70" />

      <div
        className="relative bg-bg-overlay border border-border-default rounded-lg shadow-2xl p-10 max-w-4xl w-full max-h-[80vh] overflow-y-auto"
        onClick={e => e.stopPropagation()}
      >
        <button
          onClick={onClose}
          className="absolute top-5 right-5 text-text-muted hover:text-text-secondary text-3xl"
        >
          ×
        </button>

        <h2 className="text-2xl font-semibold text-text-primary mb-2">Settings</h2>
        <p className="text-base text-text-muted mb-6">
          Buildmesh-wide defaults. Per-mesh values in Mesh Properties take precedence.
        </p>

        <div className="space-y-4">
          <label className="block text-lg font-medium text-text-secondary">
            Default provider
          </label>
          <p className="text-base text-text-muted">
            Used when a mesh has no `default_provider` of its own.
          </p>
          <select
            value={selected}
            disabled={!loaded || saving}
            onChange={e => handleSave(e.target.value)}
            className="w-full bg-bg-card border border-border-subtle rounded px-4 py-2.5 text-base text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50"
          >
            <option value={NO_OVERRIDE}>Anthropic (built-in default)</option>
            {providers.map(p => (
              <option key={p.id} value={p.id}>{p.label}</option>
            ))}
          </select>
        </div>

        {error && (
          <div className="mt-4 text-status-error text-base">{error}</div>
        )}

        {providers.filter(p => p.id !== 'terminal').length >= 2 && (
          <div className="mt-8 pt-5 border-t border-border-subtle">
            <h3 className="text-xl font-semibold text-text-primary mb-2">Spawn menu order</h3>
            <p className="text-base text-text-muted mb-4">
              Drag to reorder how harnesses appear in every spawn menu. Terminal stays pinned last.
            </p>
            <HarnessOrderList providers={providers} onReorder={handleReorderHarnesses} />
          </div>
        )}

        <div className="mt-8 pt-5 border-t border-border-subtle">
          <h3 className="text-xl font-semibold text-text-primary mb-2">Harnesses & proxied providers</h3>
          <p className="text-base text-text-muted mb-4">
            Proxy a model provider through a harness over a compatible API surface
            (e.g. MiniMax via Claude Code over Anthropic, or via Codex over OpenAI).
            The provider's API key is global — set it once on the Providers page below.
          </p>
          <HarnessConfigList
            harnesses={
              providers
                .filter((p) => !p.is_proxied && p.id !== 'terminal')
                .map((p) => ({ id: p.id, label: p.label })) as ProxyHarness[]
            }
            compatibleByHarness={compatibleByHarness}
            pairings={pairings}
            storedKeys={storedPairingKeys}
            accounts={accounts}
            onAttach={handleAttachProvider}
            onDetach={handleDetachProvider}
          />
        </div>

        <div className="mt-8 pt-5 border-t border-border-subtle">
          <div className="flex items-center justify-between mb-4">
            <h3 className="text-xl font-semibold text-text-primary">Providers</h3>
            <button
              onClick={handleRefresh}
              disabled={usageLoading}
              className="text-base text-accent-cyan hover:text-accent-cyan/80 disabled:opacity-50"
            >
              Refresh
            </button>
          </div>

          <p className="text-base text-text-muted mb-4">
            Usage Meters appear for installed harnesses and providers with a key configured.
            Keys and URLs are stored locally in <span className="font-mono">preferences.json</span>.
          </p>

          {/* Render only the detection-gated rows the backend returns: a native
              harness's card appears only when it's installed, a keyed provider's
              only when enabled (AC1). Join the editable account by id. */}
          <div className="space-y-4">
            {meters.map(meter => {
              const account = accountById(meter.provider);
              if (!account) return null;
              return (
                <AccountCard
                  key={account.id}
                  account={account}
                  usage={meter.usage ?? undefined}
                  usageTracked={meter.usageTracked}
                  usageLoading={usageLoading}
                  onSave={handleSaveAccount}
                  onRemove={handleRemoveAccount}
                />
              );
            })}
          </div>

          {addingCustom ? (
            <AddCustomProviderForm onAdd={handleAddCustom} onCancel={() => setAddingCustom(false)} />
          ) : (
            <button
              onClick={() => setAddingCustom(true)}
              className="mt-4 text-base text-accent-cyan hover:text-accent-cyan/80"
            >
              + Add custom provider
            </button>
          )}
        </div>

        <div className="mt-8 pt-5 border-t border-border-subtle">
          <h3 className="text-xl font-semibold text-text-primary mb-2">LAN / VPN Exposure</h3>
          <p className="text-base text-text-muted mb-4">
            Off by default — the server is reachable only from this machine
            (loopback). Enable to let a phone on your LAN or VPN connect. Exposed
            interfaces are served over HTTPS/WSS with a <span className="font-medium">self-signed
            certificate</span>, so your browser will warn the first time you connect;
            loopback stays plain HTTP. The change applies immediately — any
            currently-connected LAN device must reconnect over HTTPS.
          </p>

          <label className="flex items-center gap-3 text-lg text-text-primary cursor-pointer">
            <input
              type="checkbox"
              checked={lanEnabled}
              disabled={!loaded || lanBusy}
              onChange={e => handleToggleLanExposure(e.target.checked)}
              className="accent-accent-cyan h-4 w-4 disabled:opacity-50"
            />
            <span>Expose to LAN / VPN over self-signed TLS</span>
          </label>

          {/* Realized exposure (issue #586). When the toggle is on but the
              server is still loopback-only (TLS init failed, no interface,
              or per-interface bind failed), warn so the user doesn't hand
              their phone a dead URL. When exposure is working, list the
              actually-bound addresses so the user knows what to type. */}
          {lanEnabled && loaded && (
            <div className="mt-4" data-testid="lan-realized-status">
              {exposedInterfaces.length === 0 ? (
                <div
                  className="flex items-start gap-2 bg-bg-card border border-yellow-500/40 rounded px-3 py-2"
                  data-testid="lan-exposure-warning"
                  role="alert"
                >
                  <span className="text-base text-yellow-200">
                    <span className="font-medium">No interfaces are actually exposed.</span>{' '}
                    The toggle is on, but the server didn’t bind any LAN address —
                    either this machine has no non-loopback interface, or the
                    self-signed certificate couldn’t be initialized. Check the
                    application log for details. The loopback listener is still
                    serving plain HTTP for this machine.
                  </span>
                </div>
              ) : (
                <div className="border border-border-subtle rounded-lg p-4">
                  <div className="text-base text-text-secondary mb-2">
                    Reach the hub from a phone on the same network at:
                  </div>
                  <ul className="font-mono text-base text-text-primary space-y-1">
                    {exposedInterfaces.map((bind) => (
                      <li key={bind.address} data-testid="lan-exposed-interface">
                        <span className="font-medium">{bind.address}</span>
                        <span className="ml-2 text-text-muted">
                          ({bind.tls ? 'HTTPS/WSS' : 'plain'})
                        </span>
                      </li>
                    ))}
                  </ul>
                  {!tlsActive && (
                    <div
                      className="mt-3 text-base text-yellow-200"
                      data-testid="lan-tls-warning"
                      role="alert"
                    >
                      TLS is not active on any exposed interface — connections
                      from your phone will not be encrypted. Check the
                      application log for the certificate initialization error.
                    </div>
                  )}
                </div>
              )}
            </div>
          )}
        </div>

        <div className="mt-8 pt-5 border-t border-border-subtle">
          <h3 className="text-xl font-semibold text-text-primary mb-2">Coordinator Read API</h3>
          <p className="text-base text-text-muted mb-4">
            A read-only HTTP view of every node's status for an external coordinator.
            Off by default. It binds to loopback and your LAN only — reaching it from
            anywhere else is your own tunnel (Tailscale, Cloudflare, WireGuard).
          </p>

          <label className="flex items-center gap-3 text-lg text-text-primary cursor-pointer">
            <input
              type="checkbox"
              checked={coordEnabled}
              disabled={!loaded || coordBusy}
              onChange={e => handleToggleCoordinator(e.target.checked)}
              className="accent-accent-cyan h-4 w-4 disabled:opacity-50"
            />
            <span>Enable coordinator read API</span>
          </label>

          {coordEnabled && (
            <div className="mt-4 border border-border-subtle rounded-lg p-5">
              <div className="flex items-start gap-2 bg-bg-card border border-accent-cyan/30 rounded px-3 py-2 mb-4">
                <span className="text-base text-text-secondary">
                  Anyone who can reach this machine on the LAN <span className="font-medium">and</span>{' '}
                  holds the token can read your node statuses. The token is shown once, when
                  minted — copy it now; regenerating invalidates the old one.
                </span>
              </div>

              <div className="flex gap-3">
                <input
                  type="text"
                  readOnly
                  value={coordToken ?? (coordHasToken ? '••••••••  (a token has already been minted)' : '')}
                  placeholder="No token yet — generate one to copy"
                  className="flex-1 bg-bg-card border border-border-subtle rounded px-4 py-2.5 text-base font-mono text-text-primary focus:outline-none focus:border-accent-cyan"
                />
                {coordToken && (
                  <button
                    onClick={handleCopyToken}
                    className="px-5 py-2.5 bg-accent-cyan/20 text-accent-cyan text-base rounded hover:bg-accent-cyan/30"
                  >
                    {coordCopied ? 'Copied!' : 'Copy'}
                  </button>
                )}
                <button
                  onClick={handleGenerateToken}
                  disabled={coordBusy}
                  className="px-5 py-2.5 bg-accent-cyan/20 text-accent-cyan text-base rounded hover:bg-accent-cyan/30 disabled:opacity-50 whitespace-nowrap"
                >
                  {coordBusy ? 'Working…' : coordHasToken ? 'Regenerate token' : 'Generate token'}
                </button>
              </div>
            </div>
          )}
        </div>

        <div className="mt-8 pt-5 border-t border-border-subtle">
          <h3 className="text-xl font-semibold text-text-primary mb-2">Authorized Devices</h3>
          <p className="text-base text-text-muted mb-4">
            Phones you've paired keep their own session token, so they stay
            connected as their network (and IP) changes. Revoke any device to cut
            it off immediately — its open connections drop and it must pair again
            with a fresh QR code.
          </p>

          {!loaded ? (
            <p className="text-base text-text-muted">Loading…</p>
          ) : devices.length === 0 ? (
            <p className="text-base text-text-muted italic">
              No paired devices yet. Scan the Remote Access QR code from a phone to pair one.
            </p>
          ) : (
            <ul className="flex flex-col gap-2">
              {devices.map(device => (
                <li
                  key={device.id}
                  className="flex items-center gap-4 border border-border-subtle rounded-lg px-4 py-3"
                >
                  <div className="min-w-0 flex-1">
                    <div className="text-base text-text-primary truncate">
                      {device.label ?? 'Unknown device'}
                    </div>
                    <div className="text-sm text-text-muted truncate">
                      {device.last_ip ?? 'IP unknown'} · last active {device.last_active_at}
                    </div>
                  </div>
                  {confirmingRevokeId === device.id ? (
                    <div className="flex gap-2 whitespace-nowrap">
                      <button
                        onClick={() => handleRevokeDevice(device.id)}
                        disabled={revokingId === device.id}
                        className="px-4 py-2 bg-status-error text-white text-base rounded hover:bg-status-error/90 disabled:opacity-50"
                      >
                        {revokingId === device.id ? 'Revoking…' : 'Confirm revoke'}
                      </button>
                      <button
                        onClick={() => setConfirmingRevokeId(null)}
                        className="px-4 py-2 bg-bg-card text-text-secondary text-base rounded hover:bg-border-subtle"
                      >
                        Cancel
                      </button>
                    </div>
                  ) : (
                    <button
                      onClick={() => setConfirmingRevokeId(device.id)}
                      className="px-4 py-2 bg-status-error/15 text-status-error text-base rounded hover:bg-status-error/25 whitespace-nowrap"
                    >
                      Revoke
                    </button>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>

        <div className="mt-8 pt-5 border-t border-border-subtle">
          <p className="text-base text-text-muted">
            Provider defaults are stored in your app data directory at{' '}
            <span className="font-mono">preferences.json</span>; coordinator settings and
            authorized devices live in the app database.
          </p>
        </div>
      </div>
    </div>
  );
}
