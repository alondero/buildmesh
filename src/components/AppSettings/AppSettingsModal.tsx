import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface AppSettingsModalProps {
  onClose: () => void;
}

interface AppPreferences {
  default_provider: string | null;
}

interface ProviderInfo {
  id: string;
  label: string;
}

// Special sentinel for "no override — use Anthropic fallback".
// Avoids representing it as empty-string in the <select>, which collides with
// the placeholder semantics React assigns to empty-string option values.
const NO_OVERRIDE = '__no_override__';

export function AppSettingsModal({ onClose }: AppSettingsModalProps) {
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [selected, setSelected] = useState<string>(NO_OVERRIDE);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    const init = async () => {
      try {
        const [prefs, providerList] = await Promise.all([
          invoke<AppPreferences>('get_app_preferences'),
          invoke<ProviderInfo[]>('list_providers'),
        ]);
        setProviders(providerList);
        // Treat null AND empty-string as "no override" so a hand-edited
        // preferences.json containing `"default_provider": ""` doesn't leave
        // the <select> bound to a value that matches no <option>.
        const stored = prefs.default_provider;
        setSelected(stored && stored.length > 0 ? stored : NO_OVERRIDE);
        setLoaded(true);
      } catch (e) {
        setError(String(e));
        setLoaded(true);
      }
    };
    init();
  }, []);

  const handleSave = async (newValue: string) => {
    const previous = selected;
    setSelected(newValue);
    setSaving(true);
    setError(null);
    try {
      const providerArg = newValue === NO_OVERRIDE ? null : newValue;
      await invoke('set_app_default_provider', { provider: providerArg });
    } catch (e) {
      // Roll back so the UI never claims the change persisted when it didn't.
      setSelected(previous);
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center" onClick={onClose}>
      <div className="absolute inset-0 bg-black/70" />

      <div
        className="relative bg-bg-overlay border border-border-default rounded-lg shadow-2xl p-6 max-w-md w-full"
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
          <p className="text-[10px] text-text-muted">
            Stored in your app data directory at <span className="font-mono">preferences.json</span>.
          </p>
        </div>
      </div>
    </div>
  );
}
