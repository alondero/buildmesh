/**
 * `AddProviderForm` — add a keyed first-class provider from the catalog,
 * or a custom API with its credential, route and launch configuration.
 * Extracted from `AppSettingsModal`
 * in issue #1880: this was the most self-contained piece of the god-modal
 * (a form with no coupling to the modal's
 * resource state machine), so it became its own module first.
 *
 * The form owns its dirty reporting the same way `AccountCard` does, with
 * one difference worth noting: this form has no unmount cleanup, so the
 * parent MUST clear the site explicitly before unmounting it (the Cancel
 * handler does). See issue #730's code-review catch.
 */
import { useEffect, useRef, useState } from 'react';
import { ProviderIcon } from '../Providers/ProviderIcon';
import type { ApiSurface, ProviderAccount } from '../../lib/tauri';

export function AddProviderForm({
  catalog,
  onAddCatalog,
  onAddGeneric,
  onConfigure,
  onCancel,
  onDirtyChange,
  disabled = false,
}: {
  catalog: ProviderAccount[];
  onAddCatalog: (template: ProviderAccount) => Promise<void>;
  onAddGeneric: (name: string, apiKey: string) => Promise<void>;
  onConfigure: (name: string, apiKey: string, surface: ApiSurface, baseUrl: string, model: string) => Promise<void>;
  onCancel: () => void;
  onDirtyChange?: (dirty: boolean) => void;
  /** Issue #1523 — adding a provider writes `provider_accounts`, which the
   * backend refuses while the file is corrupt. */
  disabled?: boolean;
}) {
  const [mode, setMode] = useState<'pick' | 'generic'>('pick');
  const [name, setName] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [surface, setSurface] = useState<ApiSurface | 'later'>('openai');
  const [baseUrl, setBaseUrl] = useState('');
  const [model, setModel] = useState('');
  // Issue #1523 — see `AccountCard`: folding the gate into `busy` keeps
  // every existing `disabled={busy}` correct.
  const [submitting, setBusy] = useState(false);
  const busy = submitting || disabled;

  const isDirty =
    mode === 'generic'
      ? name.trim() !== '' || apiKey.trim() !== '' || baseUrl.trim() !== '' || model.trim() !== ''
      : false;
  const lastReportedDirtyRef = useRef<boolean>(false);
  useEffect(() => {
    if (lastReportedDirtyRef.current === isDirty) return;
    onDirtyChange?.(isDirty);
    lastReportedDirtyRef.current = isDirty;
  }, [isDirty, onDirtyChange]);

  const addCatalog = async (template: ProviderAccount) => {
    setBusy(true);
    try {
      await onAddCatalog(template);
    } finally {
      setBusy(false);
    }
  };

  const submitGeneric = async () => {
    setBusy(true);
    try {
      if (surface === 'later') {
        await onAddGeneric(name, apiKey);
      } else {
        await onConfigure(name, apiKey, surface, baseUrl, model);
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mt-4 border border-border-subtle rounded-lg p-5 space-y-3" data-testid="add-provider-form">
      {mode === 'pick' ? (
        <>
          <p className="text-base text-text-secondary">
            Choose a provider, or add any compatible API with its endpoint, model and key.
          </p>
          {catalog.length > 0 && (
            <ul className="space-y-2">
              {catalog.map((t) => (
                <li key={t.id}>
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => addCatalog(t)}
                    className="w-full flex items-center gap-3 border border-border-subtle rounded-md px-4 py-3 text-left hover:border-accent-cyan disabled:opacity-50"
                    aria-label={`Add ${t.name}`}
                  >
                    <ProviderIcon providerId={t.id} className="h-5 w-5" />
                    <span className="text-base text-text-primary">{t.name}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          <button
            type="button"
            disabled={busy}
            onClick={() => setMode('generic')}
            className="text-base text-text-secondary hover:text-text-primary disabled:opacity-50"
          >
            Other / custom…
          </button>
          <div>
            <button
              onClick={onCancel}
              disabled={busy}
              className="px-5 py-2 text-base text-text-secondary border border-border-strong rounded-md hover:bg-bg-card-hover hover:text-text-primary disabled:opacity-50"
            >
              Cancel
            </button>
          </div>
        </>
      ) : (
        <>
          <p className="text-base text-text-secondary">
            {surface === 'later'
              ? 'Save this credential, then attach the provider under Launch Configurations when ready.'
              : 'Add a compatible API and save a launch configuration for it.'}
          </p>
          <fieldset disabled={busy} className="space-y-3">
          <input
            type="text"
            value={name}
            onChange={e => setName(e.target.value)}
            placeholder="Name (e.g. DeepSeek)"
            className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
            aria-label="Custom provider name"
          />
          <input
            type="password"
            value={apiKey}
            onChange={e => setApiKey(e.target.value)}
            placeholder="API key"
            className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
            aria-label="Custom provider API key"
          />
          <label className="block text-sm text-text-secondary">
            API type
            <select
              value={surface}
              onChange={e => setSurface(e.target.value as ApiSurface | 'later')}
              className="mt-1 w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary"
              aria-label="Custom provider API type"
            >
              <option value="openai">OpenAI Responses — Codex</option>
              <option value="anthropic">Anthropic Messages — Claude Code</option>
              <option value="later">Credentials only — configure later</option>
            </select>
          </label>
          {surface !== 'later' && <>
            <p className="text-sm text-text-secondary" id="custom-provider-api-help">
              {surface === 'openai'
                ? 'Requires /responses with streaming and tool calls. Chat Completions alone is not supported by Codex.'
                : 'Requires the Anthropic Messages API with streaming and tool calls. This model will be used for all Claude tiers.'}
            </p>
            <label className="block text-sm text-text-secondary">
              API base URL
              <input
                type="url"
                value={baseUrl}
                onChange={e => setBaseUrl(e.target.value)}
                placeholder={surface === 'openai' ? 'https://api.example.com/v1' : 'https://api.example.com/anthropic'}
                aria-label="Custom provider base URL"
                aria-describedby="custom-provider-api-help"
                className="mt-1 w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary"
              />
            </label>
            <p className="text-sm text-text-secondary">Use the provider's API base URL, without /responses or /messages.</p>
            <label className="block text-sm text-text-secondary">
              Model ID
              <input
                value={model}
                onChange={e => setModel(e.target.value)}
                placeholder="Exact model ID from your provider"
                aria-label="Custom provider model ID"
                className="mt-1 w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary"
              />
            </label>
            <p className="text-sm text-text-secondary">
              After saving, find this configuration in the {surface === 'openai' ? 'Codex' : 'Claude Code'} spawn menu.
              {surface === 'openai' && ' A background check tests the endpoint; you can launch while it runs.'}
            </p>
          </>}
          </fieldset>
          <div className="flex gap-3">
            <button
              onClick={submitGeneric}
              disabled={busy || !name.trim() || !apiKey.trim() || (surface !== 'later' && (!baseUrl.trim() || !model.trim()))}
              className="px-5 py-2 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-base rounded-md hover:bg-bg-card-hover disabled:opacity-50"
            >
              {submitting ? 'Adding...' : 'Add provider'}
            </button>
            <button
              onClick={() => {
                setMode('pick');
                setName('');
                setApiKey('');
                setBaseUrl('');
                setModel('');
                setSurface('openai');
              }}
              disabled={busy}
              className="px-5 py-2 text-base text-text-secondary border border-border-strong rounded-md hover:bg-bg-card-hover hover:text-text-primary disabled:opacity-50"
            >
              Back
            </button>
            <button
              onClick={onCancel}
              disabled={busy}
              className="px-5 py-2 text-base text-text-secondary border border-border-strong rounded-md hover:bg-bg-card-hover hover:text-text-primary disabled:opacity-50"
            >
              Cancel
            </button>
          </div>
        </>
      )}
    </div>
  );
}
