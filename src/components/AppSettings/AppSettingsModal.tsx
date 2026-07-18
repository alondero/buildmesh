import { formatError } from '../../lib/errorUtils';
import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { ProviderIcon } from '../Providers/ProviderIcon';
import { HarnessOrderList } from './HarnessOrderList';
import { HarnessConfigList, type ProxyHarness } from './HarnessConfigList';
import * as api from '../../lib/tauri';
import type {
  ProviderInfo,
  ProviderAccount,
  ProviderPairing,
  DeviceSession,
  RealizedBind,
} from '../../lib/tauri';
import { optimisticToggle } from '../../lib/optimisticToggle';
import { Modal, ModalCloseButton } from '../shared/Modal';
import { currentTheme, setTheme, type ThemeName } from '../../lib/theme';

interface AppSettingsModalProps {
  onClose: () => void;
}

const NO_OVERRIDE = '__no_override__';

/** The Settings sub-panes. One long scroll of unrelated sections outgrew
 *  itself; each pane groups settings by concern (behaviour defaults /
 *  provider credentials / spawn-menu composition / network reachability).
 *  All panes stay MOUNTED (inactive ones get the `hidden` attribute) — the
 *  modal's dirty tracking (issue #730) lives in child component state, so
 *  unmounting a pane on tab-switch would destroy half-typed credentials
 *  while the modal still reports itself dirty. */
const SETTINGS_TABS = [
  { id: 'general', label: 'General' },
  { id: 'providers', label: 'Providers' },
  { id: 'harnesses', label: 'Harnesses' },
  { id: 'remote', label: 'Remote Access' },
] as const;
type SettingsTabId = (typeof SETTINGS_TABS)[number]['id'];

/** Which pane a dirty site belongs to, so its nav item can show the
 *  unsaved-changes dot. Site keys: `autopilot-pool` (General), `harness-*`
 *  (Harnesses, prefixed where the modal wires HarnessConfigList), and
 *  `account-*` / `add-custom-form` (Providers). */
function paneForDirtySite(site: string): SettingsTabId {
  if (site === 'autopilot-pool') return 'general';
  if (site.startsWith('harness-')) return 'harnesses';
  return 'providers';
}

// Built-ins can only be disabled, never removed (a "remove" just reverts them to
// the code default), so we hide the Remove action for these ids. Kept in sync with
// `preferences::default_provider_accounts`.
const BUILTIN_PROVIDER_IDS = ['anthropic', 'codex', 'agy', 'grok', 'minimax', 'kimi', 'openrouter'];

// The Claude model aliases a Claude-compatible provider can pin (issue #567).
// `key` is the ProviderAccount.model_tiers field; `label` is the UI caption.
const MODEL_TIER_FIELDS: { key: keyof ProviderAccount['model_tiers']; label: string }[] = [
  { key: 'default', label: 'Default model' },
  { key: 'fable', label: 'Fable' },
  { key: 'opus', label: 'Opus' },
  { key: 'sonnet', label: 'Sonnet' },
  { key: 'haiku', label: 'Haiku' },
  { key: 'small_fast', label: 'Small / fast' },
];

export function AccountCard({
  account,
  onSave,
  onRemove,
  onDirtyChange,
}: {
  account: ProviderAccount;
  onSave: (account: ProviderAccount) => Promise<boolean>;
  onRemove?: (id: string) => Promise<void>;
  /**
   * Fires with `true` when the editable draft diverges from the saved
   * `account`, and `false` when they match again (or when the card
   * unmounts). The parent aggregates these signals across the modal so a
   * stray backdrop click can prompt before destroying half-typed credentials
   * (issue #730).
   */
  onDirtyChange?: (dirty: boolean) => void;
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

  // Dirty = any editable field diverges from the saved account. The card is
  // the only place that knows what was edited; the parent just sees a
  // boolean. We compare each tier individually rather than JSON.stringify
  // the whole record — cheaper and clearer about which field is dirty.
  const isDirty = useMemo(() => {
    if (draft.api_key !== account.api_key) return true;
    if (draft.base_url !== account.base_url) return true;
    if (draft.billing_mode !== account.billing_mode) return true;
    if (draft.enabled !== account.enabled) return true;
    for (const k of Object.keys(account.model_tiers) as (keyof ProviderAccount['model_tiers'])[]) {
      if (draft.model_tiers[k] !== account.model_tiers[k]) return true;
    }
    return false;
  }, [draft, account]);
  // Track the last reported value so we only fire onDirtyChange when the
  // dirty flag actually flips. The parent re-renders a lot, which would
  // otherwise re-fire the effect (onDirtyChange is a new inline arrow on
  // each render) and cause spurious setDirtySites calls.
  const lastReportedDirtyRef = useRef<boolean>(false);
  useEffect(() => {
    if (lastReportedDirtyRef.current === isDirty) return;
    onDirtyChange?.(isDirty);
    lastReportedDirtyRef.current = isDirty;
  }, [isDirty, onDirtyChange]);

  const isCustom = !BUILTIN_PROVIDER_IDS.includes(account.id);
  // Self-authenticating harnesses (Anthropic/Codex/Antigravity) hold no creds in
  // Buildmesh, so they show no credential or model-tier fields (#568a). Only
  // Claude-compatible keyed providers (MiniMax/Kimi/custom) do.
  const showCredentials = account.claude_compatible;

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

  return (
    <div className="border border-border-subtle rounded-lg p-5">
      <div className="flex items-center gap-3 mb-3">
        <ProviderIcon providerId={account.id} className="h-6 w-6" />
        <span className="text-lg font-medium text-text-primary">{account.name}</span>
        <div className="ml-auto flex items-center gap-3">
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
                className="px-3 py-1 bg-status-error text-white text-sm rounded-md hover:bg-status-error/90 disabled:opacity-50"
                aria-label={`Confirm remove ${account.name}`}
              >
                Yes
              </button>
              <button
                onClick={() => setConfirmingRemove(false)}
                disabled={busy}
                className="px-3 py-1 bg-bg-card text-text-secondary text-sm rounded-md hover:bg-bg-card/70 disabled:opacity-50"
                aria-label={`Cancel remove ${account.name}`}
              >
                No
              </button>
            </div>
          ) : (
            <button
              onClick={() => setConfirmingRemove(true)}
              disabled={busy}
              className="px-3 py-1 bg-status-error/15 text-status-error text-sm rounded-md hover:bg-status-error/25 disabled:opacity-50"
              aria-label={`Remove ${account.name}`}
              title={`Remove ${account.name}`}
            >
              Remove
            </button>
          ))}
        </div>
      </div>

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
              className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
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
              className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
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
                    className="flex-1 min-w-0 bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
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
              className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
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
              className="px-5 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30 disabled:opacity-50"
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
  onDirtyChange,
}: {
  onAdd: (name: string, baseUrl: string, apiKey: string) => Promise<void>;
  onCancel: () => void;
  /** Dirty = any of name / baseUrl / apiKey is non-empty. See issue #730. */
  onDirtyChange?: (dirty: boolean) => void;
}) {
  const [name, setName] = useState('');
  const [baseUrl, setBaseUrl] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [busy, setBusy] = useState(false);

  const isDirty = name.trim() !== '' || baseUrl.trim() !== '' || apiKey.trim() !== '';
  // Only fire on dirty-state FLIPS, not on every parent re-render (see
  // AccountCard's matching comment for the rationale).
  const lastReportedDirtyRef = useRef<boolean>(false);
  useEffect(() => {
    if (lastReportedDirtyRef.current === isDirty) return;
    onDirtyChange?.(isDirty);
    lastReportedDirtyRef.current = isDirty;
  }, [isDirty, onDirtyChange]);

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
        className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
        aria-label="Custom provider name"
      />
      <input
        type="text"
        value={baseUrl}
        onChange={e => setBaseUrl(e.target.value)}
        placeholder="Base URL (https://api.deepseek.com/anthropic)"
        className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
        aria-label="Custom provider base URL"
      />
      <input
        type="password"
        value={apiKey}
        onChange={e => setApiKey(e.target.value)}
        placeholder="API key"
        className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
        aria-label="Custom provider API key"
      />
      <div className="flex gap-3">
        <button
          onClick={submit}
          // A custom Claude-compatible provider needs all three: without a base
          // URL + key it can't reach its endpoint, and the spawn menu only shows
          // keyed providers — a keyless add would save a row that never appears.
          disabled={busy || !name.trim() || !baseUrl.trim() || !apiKey.trim()}
          className="px-5 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30 disabled:opacity-50"
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
  // Note (issue #601): Usage Meters moved off the Settings modal to the
  // Probe Panel's "Usage" tab. The Settings surface is now credentials +
  // harness config + LAN/Coordinator/Device toggles only. The meters
  // fetch, the meter-section JSX, and the meters-only Refresh button are
  // gone from here.
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
  // Issue #824: the user-configured rename backend. `null` means
  // auto-naming is OFF (the post-v2 default). Distinct from `selected`
  // above (default provider for spawn), since rename runs frequently on
  // trivial content and shouldn't inherit the node's model.
  const [namingProvider, setNamingProvider] = useState<string | null>(null);
  const [namingSaving, setNamingSaving] = useState(false);
  const [activeTab, setActiveTab] = useState<SettingsTabId>('general');
  // Autopilot pool size (app-wide cap on concurrent autopilot nodes). The
  // draft is a string so the input can hold a cleared/in-progress value;
  // `''` means "no global cap". Committed on blur / Enter rather than per
  // keystroke — a half-typed "1" of "10" must not briefly cap the pool at 1.
  const [poolDraft, setPoolDraft] = useState('');
  const [poolSaving, setPoolSaving] = useState(false);
  // Last value confirmed saved (canonical string form), for rollback and
  // dirty comparison. A ref for the same closure-staleness reason as
  // `selectedRef` (issue #581).
  const poolSavedRef = useRef('');
  // Issue #734: theme toggle. `themeDraft` mirrors currentTheme() so the
  // radio reflects the active value on modal open; flipping it calls
  // setTheme(), which writes localStorage, updates <html data-theme>,
  // AND fires the module-level pub/sub that every ThemeManager listens
  // to — so agent terminals and build-run terminals flip in lockstep
  // without the modal touching either registry directly. No rollback
  // path — setTheme is synchronous and writes to a synchronous
  // localStorage key, so a "failed save" isn't possible.
  const [themeDraft, setThemeDraft] = useState<ThemeName>(currentTheme);
  // `loaded` flag carries over from the existing hydration logic below;
  // mirrored here so the rename picker only enables after the
  // preferences load resolves.
  // Realized exposure (issue #586). Mirrors `lanEnabled` (DB intent) until a
  // mismatch is detected — `lanEnabled=true` with no interfaces means the
  // toggle is on but the server is still loopback-only (TLS init failure, no
  // interface, per-interface bind error). The UI renders a warning so the
  // user knows their phone URL won't actually connect.
  const [tlsActive, setTlsActive] = useState(false);
  const [exposedInterfaces, setExposedInterfaces] = useState<RealizedBind[]>(
    [],
  );

  // Modal-wide dirty aggregator (issue #730). Every child that can be edited
  // (AccountCard, AddCustomProviderForm, HarnessConfigList) reports via
  // `onDirtyChange(site, dirty)`; the Set's size feeds the Modal's `dirty`
  // prop so a stray Escape or backdrop click is intercepted by the inline
  // "Discard unsaved changes?" banner. The function-form setDirtySites
  // bails out when the site is already in the right state, so re-fires from
  // a non-memoised child callback are cheap.
  const [dirtySites, setDirtySites] = useState<Set<string>>(new Set());
  const siteDirtyChange = useCallback((site: string, dirty: boolean) => {
    setDirtySites(prev => {
      if (dirty) {
        if (prev.has(site)) return prev;
        const next = new Set(prev);
        next.add(site);
        return next;
      }
      if (!prev.has(site)) return prev;
      const next = new Set(prev);
      next.delete(site);
      return next;
    });
  }, []);

  // Which panes hold unsaved edits — drives the amber dot on the nav rail so
  // a dirty pane stays discoverable after the user tabs away from it.
  const dirtyPanes = useMemo(
    () => new Set([...dirtySites].map(paneForDirtySite)),
    [dirtySites],
  );

  // Issue #581: mirror `providers` and `selected` into refs so the
  // optimistic rollback handlers below read the *latest* committed value
  // instead of whichever value the render that created the closure
  // happened to hold. A closure-captured `previous = providers` goes stale
  // the moment a re-render commits a new `providers`, and two reorders
  // fired in quick succession would both roll back to the same stale
  // snapshot. Mirrors `optimisticToggle`'s "explicit current argument"
  // pattern (#587), generalised via refs for the non-toggling state.
  const providersRef = useRef<ProviderInfo[]>([]);
  useEffect(() => {
    providersRef.current = providers;
  }, [providers]);
  const selectedRef = useRef<string>(NO_OVERRIDE);
  // Mirror of `namingProvider` for the same closure-rollback reason as
  // `selectedRef` (issue #581): a rapid second change rolls back to
  // the value as of its own selection, not to a stale render snapshot.
  const namingRef = useRef<string | null>(null);
  useEffect(() => {
    selectedRef.current = selected;
  }, [selected]);

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
        // Issue #824: the rename-backend picker reads from the same
        // `AppPreferences` snapshot. `null` here is intentional — that's
        // the default (auto-naming off until the user opts in). Empty
        // strings are normalised to `null` so the UI treats a frontend
        // "" clear the same as the explicit None the backend accepts.
        const storedNaming = prefs.naming_provider;
        setNamingProvider(storedNaming && storedNaming.length > 0 ? storedNaming : null);
        const storedPool = prefs.autopilot_pool_size == null ? '' : String(prefs.autopilot_pool_size);
        setPoolDraft(storedPool);
        poolSavedRef.current = storedPool;
        setCoordEnabled(coord.enabled);
        setCoordHasToken(coord.has_token);
        setDevices(deviceList);
        setLanEnabled(network.lan_exposure_enabled);
        // Realized bind state from the live ServerListeners (issue #586).
        setTlsActive(network.tls_active);
        setExposedInterfaces(network.exposed_interfaces);
        setLoaded(true);
      } catch (e) {
        setError(formatError(e));
        setLoaded(true);
      }
    };
    init();
  }, []);

  // Revoke a paired device: optimistically drop it from the list, then call the
  // backend (which deletes the row and force-closes any live socket it holds).
  // Roll back on failure so the list never lies about what's still authorized.
  // After a successful revoke, re-fetch the list so the panel stays authoritative
  // — a device may have re-paired, or `last_active_at` may have ticked up, while
  // the user was staring at the modal (issue #595). The refresh is best-effort:
  // a list-fetch failure after a successful revoke must NOT roll back the row,
  // because the revoke did happen — re-showing it would be a worse lie than
  // briefly stale metadata. Mirrors `refreshNetworkStatus`'s pattern (#586).
  const handleRevokeDevice = async (id: number) => {
    const previous = devices;
    setConfirmingRevokeId(null);
    setRevokingId(id);
    setError(null);
    setDevices(prev => prev.filter(d => d.id !== id));
    try {
      await api.revokeDeviceSession(id);
      try {
        setDevices(await api.listDeviceSessions());
      } catch (refreshErr) {
        console.error('Failed to refresh device list after revoke:', refreshErr);
      }
    } catch (e) {
      setDevices(previous);
      setError(formatError(e));
    } finally {
      setRevokingId(null);
    }
  };

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
      setError(formatError(e));
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
      setError(formatError(e));
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
  // pattern as `handleReorderHarnesses`.
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
  // the local `providers` list to match — keeping any non-listed rows (Terminal)
  // appended at the end, exactly as the backend re-derives them — and roll back
  // on failure so the visible order never lies about what was stored. The
  // backend emits `provider-list-changed`, so the sidebar / probes re-read live.
  // `previous` is read from `providersRef` (issue #581) so a handler running
  // against a stale closure still rolls back to the latest committed state.
  const handleReorderHarnesses = async (order: string[]) => {
    const previous = providersRef.current;
    const byId = new Map(previous.map(p => [p.id, p]));
    const reordered = [
      ...(order.map(id => byId.get(id)).filter(Boolean) as ProviderInfo[]),
      ...previous.filter(p => !order.includes(p.id)),
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

  // Persist the default-provider dropdown. Reads `previous` from `selectedRef`
  // (issue #581) so a rapid second change rolls back to the value as of its
  // own selection, not to a snapshot from the render that captured the first
  // change's closure.
  const handleSave = async (newValue: string) => {
    const previous = selectedRef.current;
    setSelected(newValue);
    setSaving(true);
    setError(null);
    try {
      const providerArg = newValue === NO_OVERRIDE ? null : newValue;
      await api.setAppDefaultProvider(providerArg);
    } catch (e) {
      setSelected(previous);
      setError(formatError(e));
    } finally {
      setSaving(false);
    }
  };

  // Issue #824: persist the rename backend. Distinct from `handleSave`
  // above — auto-naming runs frequently on trivial content, so it lives
  // on its own picker with its own optimistic-rollback ref. Empty
  // string is normalised to `null` so the picker value reads as
  // "auto-naming off" rather than as some bizarre empty id.
  const handleSaveNaming = async (newValue: string | null) => {
    const previous = namingRef.current;
    const next = newValue && newValue.length > 0 ? newValue : null;
    namingRef.current = next;
    setNamingProvider(next);
    setNamingSaving(true);
    setError(null);
    try {
      await api.setAppNamingProvider(next);
    } catch (e) {
      namingRef.current = previous;
      setNamingProvider(previous);
      setError(formatError(e));
    } finally {
      setNamingSaving(false);
    }
  };

  // Issue #734: persist the theme choice. `setTheme` is the single
  // entry point — it writes localStorage, sets/clears <html data-theme>,
  // and fires the module-level pub/sub that BOTH registries'
  // ThemeManager instances subscribe to. So one call here updates the
  // agent terminal AND the build/run terminal in lockstep. No rollback:
//   localStorage writes are synchronous and the DOM/xterm flips are
//   in-memory. The dirty-tracker is intentionally NOT involved — a
//   theme flip is an instant visual change with no half-saved state,
//   so a "Discard unsaved changes?" prompt would be more confusing
//   than helpful.
  const handleSaveTheme = (next: ThemeName) => {
    if (next === themeDraft) return;
    setThemeDraft(next);
    setTheme(next);
  };

  // Commit the autopilot pool-size draft (blur / Enter). `''` clears the
  // global cap; anything else is clamped to a non-negative integer (0 =
  // pause new autopilot spawns). Optimistic with rollback, mirroring the
  // other settings writes; the dirty site clears optimistically too so a
  // successful save never leaves a phantom discard banner.
  const commitPoolSize = async () => {
    const trimmed = poolDraft.trim();
    const numeric = Number(trimmed);
    if (trimmed !== '' && Number.isNaN(numeric)) {
      // Unreachable via the DOM (type=number sanitises non-numeric input to
      // ''), but guards the IPC from ever carrying NaN if the input type
      // changes: revert to the saved value rather than sending garbage.
      setPoolDraft(poolSavedRef.current);
      siteDirtyChange('autopilot-pool', false);
      return;
    }
    const parsed = trimmed === '' ? null : Math.max(0, Math.floor(numeric));
    const canonical = parsed === null ? '' : String(parsed);
    setPoolDraft(canonical);
    if (canonical === poolSavedRef.current) {
      siteDirtyChange('autopilot-pool', false);
      return;
    }
    const previous = poolSavedRef.current;
    poolSavedRef.current = canonical;
    siteDirtyChange('autopilot-pool', false);
    setPoolSaving(true);
    setError(null);
    try {
      await api.setAppAutopilotPoolSize(parsed);
    } catch (e) {
      poolSavedRef.current = previous;
      setPoolDraft(previous);
      setError(formatError(e));
    } finally {
      setPoolSaving(false);
    }
  };

  // Persist an account, then reload the merged list so the card reflects the
  // new enabled/billing state. Rolls the local list back on failure so the
  // toggle never lies about what the backend stored.
  // Returns whether the save succeeded so callers (e.g. the add-custom form) can
  // keep their UI open on failure instead of dismissing over the error.
  //
  // Issue #601: previously also re-fetched `get_provider_meters` here so the
  // card's bars updated after a toggle. The meters no longer live on this
  // surface — they live on the Probe Panel's "Usage" tab — so this function
  // is purely an account catalogue refresh now.
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
      return true;
    } catch (e) {
      setAccounts(previous);
      setError(formatError(e));
      return false;
    }
  };

  const handleRemoveAccount = async (id: string) => {
    setError(null);
    // Clear the dirty site for the card being removed BEFORE the await —
    // the form's own useEffect has no unmount cleanup, so if we wait for
    // the network round-trip the user could trigger a backdrop click that
    // surfaces the discard banner over a now-empty modal. Issue #730
    // code-review catch.
    siteDirtyChange(`account-${id}`, false);
    try {
      await api.removeProviderAccount(id);
      const [accountList, providerList] = await Promise.all([
        api.getProviderAccounts(),
        api.listProviders(),
      ]);
      setAccounts(accountList);
      setProviders(providerList);
    } catch (e) {
      setError(formatError(e));
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
      model_tiers: { default: null, small_fast: null, sonnet: null, opus: null, fable: null, haiku: null },
      models: [],
    });
    // Keep the form open (with the user's entries) if the backend rejected it.
    if (ok) {
      // Clear the dirty site before unmounting — the form's useEffect has no
      // unmount cleanup, so without this the modal would stay in dirty mode
      // for the rest of the session (issue #730 code-review catch).
      siteDirtyChange('add-custom-form', false);
      setAddingCustom(false);
    }
  };

  // Flip the master kill-switch. Optimistic, with rollback on failure so the
  // toggle never lies about the backend's real state. The shared
  // `optimisticToggle` helper (issue #587) factors out the previous/set/try/
  // catch/finally shape that's repeated by every settings toggle.
  const handleToggleCoordinator = (enabled: boolean) =>
    optimisticToggle({
      current: coordEnabled,
      next: enabled,
      setValue: setCoordEnabled,
      setBusy: setCoordBusy,
      setError,
      // The async wrapper turns `_invoke`'s `Promise<unknown>` into
      // `Promise<void>` so it satisfies the helper's contract.
      mutation: async () => {
        await api.setCoordinatorApiEnabled(enabled);
      },
    });

  // Flip LAN/VPN exposure. The backend rebinds the listeners live (loopback
  // plain HTTP ⇄ LAN interfaces over self-signed TLS), so we await the call
  // and roll back the toggle if it fails. On success we re-read the network
  // status so the realized-state UI (`tls_active`, `exposed_interfaces`)
  // reflects the new bind (issue #586). Same `optimisticToggle` helper as
  // the coordinator toggle — the only delta is the post-success
  // `refreshNetworkStatus` hook (issue #587).
  const handleToggleLanExposure = (enabled: boolean) =>
    optimisticToggle({
      current: lanEnabled,
      next: enabled,
      setValue: setLanEnabled,
      setBusy: setLanBusy,
      setError,
      // The async wrapper turns `_invoke`'s `Promise<unknown>` into
      // `Promise<void>` so it satisfies the helper's contract.
      mutation: async () => {
        await api.setLanExposureEnabled(enabled);
      },
      onSuccess: refreshNetworkStatus,
    });

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
      setError(formatError(e));
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
      setError(formatError(e));
    }
  };

  return (
    <Modal
      onClose={onClose}
      labelledBy="app-settings-title"
      maxWidth="max-w-4xl"
      className="p-0 max-h-[85vh] flex flex-col overflow-hidden"
      dirty={dirtySites.size > 0}
      dirtyMessage="Discard unsaved changes to your settings?"
    >
      {/* Non-scrolling header: title + close stay reachable no matter how far
          the settings body is scrolled. */}
      <div className="shrink-0 flex items-start justify-between gap-4 px-10 pt-8 pb-4 border-b border-border-subtle">
        <div>
          <h2 id="app-settings-title" className="text-2xl font-semibold text-text-primary mb-1">Settings</h2>
          <p className="text-base text-text-muted">
            Buildmesh-wide defaults. Per-mesh values in Mesh Properties take precedence.
          </p>
        </div>
        <ModalCloseButton onClose={onClose} label="Close settings" />
      </div>

      <div className="flex-1 flex min-h-0">
        <nav
          role="tablist"
          aria-orientation="vertical"
          aria-label="Settings sections"
          className="w-44 shrink-0 border-r border-border-subtle py-5 px-3 space-y-1 overflow-y-auto"
        >
          {SETTINGS_TABS.map(tab => (
            <button
              key={tab.id}
              type="button"
              role="tab"
              aria-selected={activeTab === tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={`w-full flex items-center justify-between text-left px-3 py-2 rounded-md text-base ${
                activeTab === tab.id
                  ? 'bg-bg-card text-accent-cyan font-medium'
                  : 'text-text-secondary hover:bg-bg-card/60 hover:text-text-primary'
              }`}
            >
              <span>{tab.label}</span>
              {dirtyPanes.has(tab.id) && (
                <span
                  className="h-1.5 w-1.5 rounded-full bg-status-warning"
                  title="Unsaved changes"
                  data-testid={`settings-tab-dirty-${tab.id}`}
                />
              )}
            </button>
          ))}
        </nav>

        <div className="flex-1 overflow-y-auto px-8 pb-10 pt-6">
        {/* Shared error surface — outside the panes so a failed save is
            visible no matter which pane the user is looking at. */}
        {error && (
          <div className="mb-4 text-status-error text-base">{error}</div>
        )}

        <section
          role="tabpanel"
          aria-label="General"
          hidden={activeTab !== 'general'}
          className="space-y-8"
        >
        <div className="space-y-4">
          <label className="block text-lg font-medium text-text-secondary">
            Default provider
          </label>
          <p className="text-base text-text-muted">
            Used when a mesh has no `default_provider` of its own.
          </p>
          <select
            aria-label="Default provider"
            value={selected}
            disabled={!loaded || saving}
            onChange={e => handleSave(e.target.value)}
            className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2.5 text-base text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50"
          >
            <option value={NO_OVERRIDE}>Anthropic (built-in default)</option>
            {providers.map(p => (
              <option key={p.id} value={p.id}>{p.label}</option>
            ))}
          </select>
        </div>

        {/* Issue #824: Auto-naming. Distinct from default-provider above.
            Auto-naming runs frequently on trivial content, so the user
            explicitly opts in via this picker. The helper pins a cheap
            haiku tier when "anthropic" is picked so the user's main
            subscription default is never silently inherited. Empty /
            "Disabled" leaves nodes with their random adj-adj-noun
            slugs. */}
        <div className="pt-6 border-t border-border-subtle space-y-4">
          <label className="block text-lg font-medium text-text-secondary">
            Auto-naming
          </label>
          <p className="text-base text-text-muted">
            When a node finishes a turn, Buildmesh can ask a small LLM to summarise
            the work into a slug (e.g. <code>fix-auth-flow</code>) instead of the
            default <code>bold-keen-brook</code>. Auto-naming runs frequently on
            trivial content — pick a cheap backend so an Opus-class node doesn't
            burn tokens on every rename.
          </p>
          <select
            value={namingProvider ?? ''}
            disabled={!loaded || namingSaving}
            onChange={e => handleSaveNaming(e.target.value || null)}
            className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2.5 text-base text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50"
          >
            <option value="">Disabled (auto-naming off)</option>
            {providers
              .filter((p) => p.id !== 'terminal')
              .map((p) => (
                <option key={p.id} value={p.id}>{p.label}</option>
              ))}
          </select>
          {namingProvider === 'anthropic' && (
            <p className="text-sm text-text-muted">
              Built-in Anthropic is pinned to a haiku tier so the rename doesn't
              inherit your main subscription default.
            </p>
          )}
          {namingProvider === null && (
            <p className="text-sm text-text-muted">
              Auto-naming is off. New nodes keep random adjective-adjective-noun
              slugs. You can always rename manually from the sidebar.
            </p>
          )}
        </div>

        <div className="pt-6 border-t border-border-subtle space-y-4">
          <label
            htmlFor="autopilot-pool-size"
            className="block text-lg font-medium text-text-secondary"
          >
            Autopilot pool size
          </label>
          <p className="text-base text-text-muted">
            The most autopilot nodes allowed to run at once across{' '}
            <span className="font-medium">all</span> meshes. Each mesh still
            respects its own concurrency limit (set in Mesh Properties) — this
            caps the total, so ten meshes with two slots each can't put twenty
            agents on your machine. Leave empty for no global cap; 0 pauses new
            autopilot spawns. Running nodes are never stopped — lowering the cap
            just holds new spawns until slots free up.
          </p>
          <input
            id="autopilot-pool-size"
            type="number"
            min={0}
            step={1}
            inputMode="numeric"
            aria-label="Autopilot pool size"
            placeholder="No global cap"
            value={poolDraft}
            disabled={!loaded || poolSaving}
            onChange={e => {
              setPoolDraft(e.target.value);
              siteDirtyChange('autopilot-pool', e.target.value.trim() !== poolSavedRef.current);
            }}
            onBlur={commitPoolSize}
            onKeyDown={e => {
              if (e.key === 'Enter') commitPoolSize();
            }}
            className="w-48 bg-bg-card border border-border-subtle rounded-md px-4 py-2.5 text-base text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50"
          />
        </div>

        <div className="pt-6 border-t border-border-subtle space-y-4">
          <label
            htmlFor="theme-radio-group"
            className="block text-lg font-medium text-text-secondary"
          >
            Appearance
          </label>
          <p className="text-base text-text-muted">
            Pick the colour theme. Dark is the default; light inverts the
            surface and text tokens while keeping the accent palette intact.
            The choice is saved per machine — xterm.js terminals flip with
            the rest of the app.
          </p>
          <fieldset
            id="theme-radio-group"
            aria-label="Theme"
            className="flex flex-wrap gap-2"
          >
            {(['dark', 'light'] as const).map((name) => (
              <label
                key={name}
                className={`flex items-center gap-2 px-4 py-2 rounded-md text-base cursor-pointer border transition-colors ${
                  themeDraft === name
                    ? 'bg-bg-card border-accent-cyan text-text-primary'
                    : 'bg-bg-card border-border-subtle text-text-secondary hover:border-border-default'
                }`}
              >
                <input
                  type="radio"
                  name="theme"
                  value={name}
                  checked={themeDraft === name}
                  // Controlled radio: the picker is always in step with the
                  // active theme (setTheme is synchronous). Each click
                  // commits immediately — no "Save" button, no dirty site,
                  // no rollback. The visual transition is the persistence.
                  onChange={() => handleSaveTheme(name)}
                  className="accent-accent-cyan"
                  data-testid={`theme-radio-${name}`}
                />
                <span className="capitalize">{name}</span>
              </label>
            ))}
          </fieldset>
        </div>

        <div className="pt-6 border-t border-border-subtle">
          <p className="text-base text-text-muted">
            Provider defaults are stored in your app data directory at{' '}
            <span className="font-mono">preferences.json</span>; coordinator settings and
            authorized devices live in the app database.
          </p>
        </div>
        </section>

        <section
          role="tabpanel"
          aria-label="Harnesses"
          hidden={activeTab !== 'harnesses'}
          className="space-y-8"
        >
        {providers.filter(p => p.id !== 'terminal').length >= 2 && (
          <div className="pt-6 border-t border-border-subtle first:pt-0 first:border-t-0">
            <h3 className="text-xl font-semibold text-text-primary mb-2">Spawn menu order</h3>
            <p className="text-base text-text-muted mb-4">
              Drag to reorder how harnesses appear in every spawn menu. Terminal stays pinned last.
            </p>
            <HarnessOrderList providers={providers} onReorder={handleReorderHarnesses} />
          </div>
        )}

        <div className="pt-6 border-t border-border-subtle first:pt-0 first:border-t-0">
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
            // Issue #577 — per-harness child reorder handler.
            onReorderProxied={handleReorderProxiedProviders}
            // Prefixed so `paneForDirtySite` can route a harness card's
            // unsaved edits to the Harnesses nav dot (the list reports raw
            // harness ids like "claude").
            onDirtyChange={(site, d) => siteDirtyChange(`harness-${site}`, d)}
          />
        </div>
        </section>

        <section
          role="tabpanel"
          aria-label="Providers"
          hidden={activeTab !== 'providers'}
        >
        <div>
          <h3 className="text-xl font-semibold text-text-primary mb-2">Providers</h3>
          <p className="text-base text-text-muted mb-4">
            Enable / disable each model provider and edit its credentials.
            Usage Meters live on the <span className="font-medium">Usage</span> tab
            in the side panel — open it from the meter icon in the sidebar header.
            Keys and URLs are stored locally in <span className="font-mono">preferences.json</span>.
          </p>

          {/* Every configured account renders a card here so the user can edit
              credentials / toggle enable / remove a custom one. The card is a
              pure config surface — the read-only meters moved to the Usage
              tab (issue #601). Detection gating (issue #574) only applies to
              the meters, NOT to the card list: a user must be able to see +
              configure an undetectable native harness's card before they
              install it. */}
          <div className="space-y-4">
            {accounts.map(account => (
              <AccountCard
                key={account.id}
                account={account}
                onSave={handleSaveAccount}
                onRemove={handleRemoveAccount}
                onDirtyChange={d => siteDirtyChange(`account-${account.id}`, d)}
              />
            ))}
          </div>

          {addingCustom ? (
            <AddCustomProviderForm
              onAdd={handleAddCustom}
              onCancel={() => {
                // The form's useEffect has no unmount cleanup, so the parent
                // has to clear the dirty site before unmounting the form.
                // Otherwise the modal stays in dirty mode for the rest of
                // the session. Issue #730 code-review catch.
                siteDirtyChange('add-custom-form', false);
                setAddingCustom(false);
              }}
              onDirtyChange={d => siteDirtyChange('add-custom-form', d)}
            />
          ) : (
            <button
              onClick={() => setAddingCustom(true)}
              className="mt-4 text-base text-accent-cyan hover:text-accent-cyan/80"
            >
              + Add custom provider
            </button>
          )}
        </div>
        </section>

        <section
          role="tabpanel"
          aria-label="Remote Access"
          hidden={activeTab !== 'remote'}
          className="space-y-8"
        >
        <div>
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
                  className="flex items-start gap-2 bg-bg-card border border-status-warning/40 rounded-md px-3 py-2"
                  data-testid="lan-exposure-warning"
                  role="alert"
                >
                  <span className="text-base text-status-warning">
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
                      className="mt-3 text-base text-status-warning"
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

        <div className="pt-6 border-t border-border-subtle">
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
              <div className="flex items-start gap-2 bg-bg-card border border-accent-cyan/30 rounded-md px-3 py-2 mb-4">
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
                  className="flex-1 bg-bg-card border border-border-subtle rounded-md px-4 py-2.5 text-base font-mono text-text-primary focus:outline-none focus:border-accent-cyan"
                />
                {coordToken && (
                  <button
                    onClick={handleCopyToken}
                    className="px-5 py-2.5 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30"
                  >
                    {coordCopied ? 'Copied!' : 'Copy'}
                  </button>
                )}
                <button
                  onClick={handleGenerateToken}
                  disabled={coordBusy}
                  className="px-5 py-2.5 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30 disabled:opacity-50 whitespace-nowrap"
                >
                  {coordBusy ? 'Working…' : coordHasToken ? 'Regenerate token' : 'Generate token'}
                </button>
              </div>
            </div>
          )}
        </div>

        <div className="pt-6 border-t border-border-subtle">
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
                        className="px-4 py-2 bg-status-error text-white text-base rounded-md hover:bg-status-error/90 disabled:opacity-50"
                      >
                        {revokingId === device.id ? 'Revoking…' : 'Confirm revoke'}
                      </button>
                      <button
                        onClick={() => setConfirmingRevokeId(null)}
                        className="px-4 py-2 bg-bg-card text-text-secondary text-base rounded-md hover:bg-border-subtle"
                      >
                        Cancel
                      </button>
                    </div>
                  ) : (
                    <button
                      onClick={() => setConfirmingRevokeId(device.id)}
                      className="px-4 py-2 bg-status-error/15 text-status-error text-base rounded-md hover:bg-status-error/25 whitespace-nowrap"
                    >
                      Revoke
                    </button>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
        </section>
        </div>
      </div>
    </Modal>
  );
}
