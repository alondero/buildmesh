import { useState, useEffect } from 'react';
import { ProviderIcon } from '../Providers/ProviderIcon';
import * as api from '../../lib/tauri';
import type {
  ProviderInfo,
  ProviderUsage,
  UsageWindow,
  ProviderAccount,
  BillingBalance,
} from '../../lib/tauri';

interface AppSettingsModalProps {
  onClose: () => void;
}

const NO_OVERRIDE = '__no_override__';

// Built-ins can only be disabled, never removed (a "remove" just reverts them to
// the code default), so we hide the Remove action for these ids. Kept in sync with
// `preferences::default_provider_accounts`.
const BUILTIN_PROVIDER_IDS = ['anthropic', 'codex', 'agy', 'minimax'];

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

/** One model-provider account: enable toggle, collapsible credential editor, and
 *  a usage view chosen by billing mode (percentage bars vs cash balance). */
export function AccountCard({
  account,
  usage,
  usageLoading,
  onSave,
  onRemove,
}: {
  account: ProviderAccount;
  usage?: ProviderUsage;
  usageLoading: boolean;
  onSave: (account: ProviderAccount) => Promise<boolean>;
  onRemove?: (id: string) => Promise<void>;
}) {
  const [draft, setDraft] = useState<ProviderAccount>(account);
  const [showCreds, setShowCreds] = useState(false);
  const [busy, setBusy] = useState(false);

  // Re-sync the editable draft when the parent reloads accounts (e.g. after save).
  useEffect(() => setDraft(account), [account]);

  const isCustom = !BUILTIN_PROVIDER_IDS.includes(account.id);
  const payg = account.billing_mode === 'pay_as_you_go';

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

  const renderUsage = () => {
    if (!account.enabled) return <p className="text-base text-text-muted">Disabled</p>;
    if (!usage && !usageLoading) return <p className="text-base text-text-muted">Unable to load usage data</p>;
    if (!usage) return null;
    if (!usage.loggedIn) {
      return (
        <div>
          <p className="text-base text-status-warning">{payg ? 'No API key' : 'Not logged in'}</p>
          <p className="text-sm text-text-muted mt-1">
            {payg ? `Enter an API key for ${account.name} above` : `Run the ${account.name} CLI login first`}
          </p>
        </div>
      );
    }
    if (usage.error) return <p className="text-base text-status-error">{usage.error}</p>;
    if (payg) {
      return usage.balance ? (
        <BalanceCard balance={usage.balance} />
      ) : (
        <p className="text-sm text-text-muted">Balance unavailable</p>
      );
    }
    return (
      <div>
        {usage.windows.map(w => (
          <UsageBar key={w.label} window={w} />
        ))}
        {usage.detail && <p className="text-sm text-accent-cyan mt-2">{usage.detail}</p>}
      </div>
    );
  };

  return (
    <div className="border border-border-subtle rounded-lg p-5">
      <div className="flex items-center gap-3 mb-3">
        <ProviderIcon providerId={account.id} className="h-6 w-6" />
        <span className="text-lg font-medium text-text-primary">{account.name}</span>
        {usageLoading && account.enabled && (
          <span className="ml-auto text-base text-text-muted">Loading...</span>
        )}
        <label className={`flex items-center gap-2 text-base text-text-secondary cursor-pointer ${usageLoading && account.enabled ? '' : 'ml-auto'}`}>
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
      </div>

      {renderUsage()}

      <button
        onClick={() => setShowCreds(v => !v)}
        className="mt-3 text-sm text-accent-cyan hover:text-accent-cyan/80"
      >
        {showCreds ? 'Hide credentials' : 'Edit credentials'}
      </button>

      {showCreds && (
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
              placeholder="https://api.example.com/v1"
              className="w-full bg-bg-card border border-border-subtle rounded px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
              aria-label={`${account.name} base URL`}
            />
          </div>
          <div>
            <label className="block text-sm text-text-muted mb-1">Custom models (comma-separated)</label>
            <input
              type="text"
              value={draft.models.join(', ')}
              onChange={e =>
                setDraft({
                  ...draft,
                  models: e.target.value.split(',').map(s => s.trim()).filter(Boolean),
                })
              }
              placeholder="model-a, model-b"
              className="w-full bg-bg-card border border-border-subtle rounded px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
              aria-label={`${account.name} custom models`}
            />
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
            {isCustom && onRemove && (
              <button
                onClick={() => onRemove(account.id)}
                disabled={busy}
                className="px-5 py-2 bg-status-error/15 text-status-error text-base rounded hover:bg-status-error/25 disabled:opacity-50"
              >
                Remove
              </button>
            )}
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
          disabled={busy || !name.trim()}
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
  const [usageData, setUsageData] = useState<ProviderUsage[]>([]);
  const [usageLoading, setUsageLoading] = useState(false);
  const [accounts, setAccounts] = useState<ProviderAccount[]>([]);
  const [addingCustom, setAddingCustom] = useState(false);
  const [coordEnabled, setCoordEnabled] = useState(false);
  const [coordHasToken, setCoordHasToken] = useState(false);
  const [coordToken, setCoordToken] = useState<string | null>(null);
  const [coordBusy, setCoordBusy] = useState(false);
  const [coordCopied, setCoordCopied] = useState(false);

  useEffect(() => {
    const init = async () => {
      try {
        const [prefs, providerList, accountList, coord] = await Promise.all([
          api.getAppPreferences(),
          api.listProviders(),
          api.getProviderAccounts(),
          api.getCoordinatorStatus(),
        ]);
        setProviders(providerList);
        setAccounts(accountList);
        const stored = prefs.default_provider;
        setSelected(stored && stored.length > 0 ? stored : NO_OVERRIDE);
        setCoordEnabled(coord.enabled);
        setCoordHasToken(coord.has_token);
        setLoaded(true);
        fetchUsage();
      } catch (e) {
        setError(String(e));
        setLoaded(true);
      }
    };
    init();
  }, []);

  const fetchUsage = async (force = false) => {
    setUsageLoading(true);
    try {
      const data = await api.getAllProviderUsage(force);
      setUsageData(data);
    } catch (e) {
      console.error('Failed to fetch usage:', e);
    } finally {
      setUsageLoading(false);
    }
  };

  const handleRefresh = () => fetchUsage(true);

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
      setAccounts(await api.getProviderAccounts());
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
      setAccounts(await api.getProviderAccounts());
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
      billing_mode: 'pay_as_you_go',
      api_key: apiKey.trim() || null,
      base_url: baseUrl.trim() || null,
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

  const usageFor = (providerId: string) => usageData.find(u => u.provider === providerId);

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

        <div className="mt-8 pt-5 border-t border-border-subtle">
          <div className="flex items-center justify-between mb-4">
            <h3 className="text-xl font-semibold text-text-primary">Accounts & Usage</h3>
            <button
              onClick={handleRefresh}
              disabled={usageLoading}
              className="text-base text-accent-cyan hover:text-accent-cyan/80 disabled:opacity-50"
            >
              Refresh
            </button>
          </div>

          <p className="text-base text-text-muted mb-4">
            Enable the accounts you use; only enabled providers are polled. Keys and URLs are
            stored locally in <span className="font-mono">preferences.json</span>.
          </p>

          <div className="space-y-4">
            {accounts.map(account => (
              <AccountCard
                key={account.id}
                account={account}
                usage={usageFor(account.id)}
                usageLoading={usageLoading}
                onSave={handleSaveAccount}
                onRemove={handleRemoveAccount}
              />
            ))}
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
          <p className="text-base text-text-muted">
            Provider defaults are stored in your app data directory at{' '}
            <span className="font-mono">preferences.json</span>; coordinator settings live in
            the app database.
          </p>
        </div>
      </div>
    </div>
  );
}
