import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface AppSettingsModalProps {
  onClose: () => void;
}

interface AppPreferences {
  default_provider: string | null;
  minimax_api_key: string | null;
}

interface ProviderInfo {
  id: string;
  label: string;
}

interface UsageWindow {
  label: string;
  usedPercent: number | null;
  resetsAt: string | null;
}

interface ProviderUsage {
  provider: string;
  loggedIn: boolean;
  windows: UsageWindow[];
  detail: string | null;
  error: string | null;
}

const NO_OVERRIDE = '__no_override__';

export function UsageBar({ window }: { window: UsageWindow }) {
  const percent = window.usedPercent ?? 0;
  const color = percent > 80 ? 'bg-status-error' : percent > 60 ? 'bg-status-warning' : 'bg-accent-cyan';
  // Show the figure whenever it's known — 0% (full quota remaining) is a real
  // value, not missing data. Only a null usedPercent is "N/A".
  const display = window.usedPercent != null ? `${percent.toFixed(1)}%` : 'N/A';
  return (
    <div className="mt-1">
      <div className="flex justify-between text-[10px] text-text-muted mb-0.5">
        <span>{window.label}</span>
        <span>{display}</span>
      </div>
      <div className="h-1.5 bg-bg-card rounded-full overflow-hidden">
        <div className={`h-full ${color} rounded-full`} style={{ width: `${Math.min(percent, 100)}%` }} />
      </div>
      {window.resetsAt && (
        <p className="text-[9px] text-text-muted mt-0.5">Resets: {new Date(window.resetsAt).toLocaleString()}</p>
      )}
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
  const [minimaxKey, setMinimaxKey] = useState('');
  const [minimaxKeySaving, setMinimaxKeySaving] = useState(false);

  useEffect(() => {
    const init = async () => {
      try {
        const [prefs, providerList] = await Promise.all([
          invoke<AppPreferences>('get_app_preferences'),
          invoke<ProviderInfo[]>('list_providers'),
        ]);
        setProviders(providerList);
        const stored = prefs.default_provider;
        setSelected(stored && stored.length > 0 ? stored : NO_OVERRIDE);
        setMinimaxKey(prefs.minimax_api_key || '');
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
      const data = await invoke<ProviderUsage[]>('get_all_provider_usage', { forceRefresh: force });
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
      await invoke('set_app_default_provider', { provider: providerArg });
    } catch (e) {
      setSelected(previous);
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const handleSaveMinimaxKey = async () => {
    setMinimaxKeySaving(true);
    try {
      const key = minimaxKey.trim() || null;
      await invoke('set_minimax_api_key', { key });
      fetchUsage(true);
    } catch (e) {
      console.error('Failed to save MiniMax key:', e);
    } finally {
      setMinimaxKeySaving(false);
    }
  };

  const getUsageForProvider = (providerId: string) =>
    usageData.find(u => u.provider === providerId);

  const ProviderIcon = ({ id }: { id: string }) => {
    const icons: Record<string, string> = {
      anthropic: '🧠',
      codex: '⚡',
      minimax: '🔷',
      agy: '🟢',
    };
    return <span className="text-sm">{icons[id] || '❓'}</span>;
  };

  const ProviderCard = ({ providerId, label }: { providerId: string; label: string }) => {
    const usage = getUsageForProvider(providerId);

    return (
      <div className="border border-border-subtle rounded p-3">
        <div className="flex items-center gap-2 mb-2">
          <ProviderIcon id={providerId} />
          <span className="text-xs font-medium text-text-primary">{label}</span>
          {usageLoading && <span className="ml-auto text-[10px] text-text-muted">Loading...</span>}
        </div>

        {!usage && !usageLoading && (
          <p className="text-[11px] text-text-muted">Unable to load usage data</p>
        )}

        {usage && !usage.loggedIn && (
          <div>
            <p className="text-[11px] text-status-warning">Not logged in</p>
            <p className="text-[10px] text-text-muted mt-0.5">
              Run the {label} CLI login first
            </p>
          </div>
        )}

        {usage && usage.loggedIn && usage.error && (
          <p className="text-[11px] text-status-error">{usage.error}</p>
        )}

        {usage && usage.loggedIn && !usage.error && (
          <div>
            {usage.windows.map(w => (
              <UsageBar key={w.label} window={w} />
            ))}
            {usage.detail && (
              <p className="text-[10px] text-accent-cyan mt-1">{usage.detail}</p>
            )}
          </div>
        )}
      </div>
    );
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center" onClick={onClose}>
      <div className="absolute inset-0 bg-black/70" />

      <div
        className="relative bg-bg-overlay border border-border-default rounded-lg shadow-2xl p-6 max-w-md w-full max-h-[80vh] overflow-y-auto"
        onClick={e => e.stopPropagation()}
      >
        <button
          onClick={onClose}
          className="absolute top-3 right-3 text-text-muted hover:text-text-secondary text-lg"
        >
          ×
        </button>

        <h2 className="text-sm font-semibold text-text-primary mb-1">Settings</h2>
        <p className="text-xs text-text-muted mb-5">
          Buildmesh-wide defaults. Per-mesh values in Mesh Properties take precedence.
        </p>

        <div className="space-y-2">
          <label className="block text-xs font-medium text-text-secondary">
            Default provider
          </label>
          <p className="text-[11px] text-text-muted">
            Used when a mesh has no `default_provider` of its own.
          </p>
          <select
            value={selected}
            disabled={!loaded || saving}
            onChange={e => handleSave(e.target.value)}
            className="w-full bg-bg-card border border-border-subtle rounded px-2 py-1.5 text-xs text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50"
          >
            <option value={NO_OVERRIDE}>Anthropic (built-in default)</option>
            {providers.map(p => (
              <option key={p.id} value={p.id}>{p.label}</option>
            ))}
          </select>
        </div>

        {error && (
          <div className="mt-3 text-status-error text-xs">{error}</div>
        )}

        <div className="mt-5 pt-3 border-t border-border-subtle">
          <div className="flex items-center justify-between mb-3">
            <h3 className="text-xs font-semibold text-text-primary">Accounts & Usage</h3>
            <button
              onClick={handleRefresh}
              disabled={usageLoading}
              className="text-[10px] text-accent-cyan hover:text-accent-cyan/80 disabled:opacity-50"
            >
              Refresh
            </button>
          </div>

          <div className="space-y-3">
            <ProviderCard providerId="anthropic" label="Anthropic / Claude" />
            <ProviderCard providerId="agy" label="Google / Antigravity" />
            <ProviderCard providerId="codex" label="OpenAI / Codex" />
            <ProviderCard providerId="minimax" label="MiniMax" />
          </div>

          <div className="mt-3 border border-border-subtle rounded p-3">
            <div className="flex items-center gap-2 mb-2">
              <ProviderIcon id="minimax" />
              <span className="text-xs font-medium text-text-primary">MiniMax API Key</span>
            </div>
            <p className="text-[10px] text-text-muted mb-2">
              Stored locally in preferences.json
            </p>
            <div className="flex gap-2">
              <input
                type="password"
                value={minimaxKey}
                onChange={e => setMinimaxKey(e.target.value)}
                placeholder="Enter API key..."
                className="flex-1 bg-bg-card border border-border-subtle rounded px-2 py-1 text-xs text-text-primary focus:outline-none focus:border-accent-cyan"
              />
              <button
                onClick={handleSaveMinimaxKey}
                disabled={minimaxKeySaving}
                className="px-3 py-1 bg-accent-cyan/20 text-accent-cyan text-xs rounded hover:bg-accent-cyan/30 disabled:opacity-50"
              >
                {minimaxKeySaving ? 'Saving...' : 'Save'}
              </button>
            </div>
          </div>
        </div>

        <div className="mt-5 pt-3 border-t border-border-subtle">
          <p className="text-[10px] text-text-muted">
            Stored in your app data directory at <span className="font-mono">preferences.json</span>.
          </p>
        </div>
      </div>
    </div>
  );
}
