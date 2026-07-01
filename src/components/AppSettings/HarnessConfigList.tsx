import { useState } from 'react';
import { ProviderIcon } from '../Providers/ProviderIcon';
import type { ProviderAccount, ProviderPairing, ApiSurface } from '../../lib/tauri';

/** A harness that can host Proxied Providers (speaks a Compatible API surface). */
export interface ProxyHarness {
  id: string;
  label: string;
}

const SURFACE_LABEL: Record<ApiSurface, string> = {
  anthropic: 'Anthropic',
  openai: 'OpenAI',
};

/** Stable key for a (harness, provider) pairing — matches the backend
 *  `(harness_id, provider_id)` identity used for detach. */
const pairKey = (harnessId: string, providerId: string) => `${harnessId}:${providerId}`;

/**
 * The harness-centric **config page** (ADR-0016 §5, issue #576). For each
 * proxy-capable **Agent Harness** it lists the **Proxied Providers** attached to
 * it (each with its **Compatible API surface** + endpoint) and offers
 * "Add proxied provider", which only surfaces providers whose surface that
 * harness speaks (surface-matched, AC3).
 *
 * The **API key is global** to the provider and edited on the Providers page;
 * the attach flow only seeds it *if absent* (so a provider with no key yet shows
 * an inline key field). The default Anthropic pairing for a keyed provider is
 * derived backend-side and shown as managed-on-Providers (not detachable here);
 * extra surfaces/harnesses the user attaches are detachable.
 */
export function HarnessConfigList({
  harnesses,
  compatibleByHarness,
  pairings,
  storedKeys,
  accounts,
  onAttach,
  onDetach,
}: {
  harnesses: ProxyHarness[];
  compatibleByHarness: Record<string, ProviderAccount[]>;
  pairings: ProviderPairing[];
  /** `${harness_id}:${provider_id}` of pairings that are user-stored (detachable). */
  storedKeys: Set<string>;
  accounts: ProviderAccount[];
  onAttach: (harnessId: string, providerId: string, apiKey: string | null) => Promise<void>;
  onDetach: (harnessId: string, providerId: string) => Promise<void>;
}) {
  const accountName = (id: string) => accounts.find((a) => a.id === id)?.name ?? id;
  const isKeyed = (id: string) => {
    const k = accounts.find((a) => a.id === id)?.api_key;
    return !!k && k.length > 0;
  };

  // Only render harnesses that can actually host a provider (have ≥1 compatible
  // provider) or already have one attached — a native-only harness (Terminal,
  // Antigravity) has no proxy section.
  const visible = harnesses.filter(
    (h) =>
      (compatibleByHarness[h.id]?.length ?? 0) > 0 ||
      pairings.some((p) => p.harness_id === h.id),
  );

  if (visible.length === 0) {
    return (
      <p className="text-base text-text-muted">
        No harnesses on this host can host a proxied provider yet.
      </p>
    );
  }

  return (
    <div className="space-y-4" data-testid="harness-config-list">
      {visible.map((harness) => (
        <HarnessCard
          key={harness.id}
          harness={harness}
          compatible={compatibleByHarness[harness.id] ?? []}
          pairings={pairings.filter((p) => p.harness_id === harness.id)}
          storedKeys={storedKeys}
          accountName={accountName}
          isKeyed={isKeyed}
          onAttach={onAttach}
          onDetach={onDetach}
        />
      ))}
    </div>
  );
}

function HarnessCard({
  harness,
  compatible,
  pairings,
  storedKeys,
  accountName,
  isKeyed,
  onAttach,
  onDetach,
}: {
  harness: ProxyHarness;
  compatible: ProviderAccount[];
  pairings: ProviderPairing[];
  storedKeys: Set<string>;
  accountName: (id: string) => string;
  isKeyed: (id: string) => boolean;
  onAttach: (harnessId: string, providerId: string, apiKey: string | null) => Promise<void>;
  onDetach: (harnessId: string, providerId: string) => Promise<void>;
}) {
  const [adding, setAdding] = useState(false);
  const [selected, setSelected] = useState('');
  const [key, setKey] = useState('');
  const [busy, setBusy] = useState(false);

  const attachedIds = new Set(pairings.map((p) => p.provider_id));
  // Offer only compatible providers not already attached to this harness.
  const offerable = compatible.filter((a) => !attachedIds.has(a.id));
  const needsKey = selected !== '' && !isKeyed(selected);

  const reset = () => {
    setAdding(false);
    setSelected('');
    setKey('');
  };

  const submitAttach = async () => {
    if (!selected) return;
    setBusy(true);
    try {
      await onAttach(harness.id, selected, needsKey ? key.trim() || null : null);
      reset();
    } catch {
      // Parent surfaces the error toast; keep the form open so the user can retry.
    } finally {
      setBusy(false);
    }
  };

  const detach = async (providerId: string) => {
    setBusy(true);
    try {
      await onDetach(harness.id, providerId);
    } catch {
      // Parent surfaces the error.
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="border border-border-subtle rounded-lg p-5" data-testid={`harness-${harness.id}`}>
      <div className="flex items-center gap-3 mb-3">
        <ProviderIcon providerId={harness.id} className="h-6 w-6" />
        <span className="text-lg font-medium text-text-primary">{harness.label}</span>
      </div>

      {pairings.length === 0 ? (
        <p className="text-base text-text-muted">No proxied providers attached.</p>
      ) : (
        <ul className="flex flex-col gap-2">
          {pairings.map((p) => {
            const detachable = storedKeys.has(pairKey(p.harness_id, p.provider_id));
            return (
              <li
                key={p.provider_id}
                className="flex items-center gap-3 border border-border-subtle rounded-md px-3 py-2"
                data-testid={`pairing-${p.harness_id}-${p.provider_id}`}
              >
                <ProviderIcon providerId={p.provider_id} className="h-5 w-5" />
                <div className="min-w-0 flex-1">
                  <div className="text-base text-text-primary truncate">
                    {accountName(p.provider_id)}
                    <span className="ml-2 text-sm text-accent-cyan">{SURFACE_LABEL[p.surface]}</span>
                  </div>
                  {p.base_url && (
                    <div className="text-sm text-text-muted truncate font-mono">{p.base_url}</div>
                  )}
                </div>
                {detachable ? (
                  <button
                    onClick={() => detach(p.provider_id)}
                    disabled={busy}
                    className="px-3 py-1 bg-status-error/15 text-status-error text-sm rounded-md hover:bg-status-error/25 disabled:opacity-50"
                    aria-label={`Detach ${accountName(p.provider_id)} from ${harness.label}`}
                  >
                    Detach
                  </button>
                ) : (
                  <span className="text-sm text-text-muted whitespace-nowrap" title="The global key is managed on the Providers page">
                    Default · key on Providers
                  </span>
                )}
              </li>
            );
          })}
        </ul>
      )}

      {adding ? (
        <div className="mt-3 space-y-3 border-t border-border-subtle pt-3">
          {offerable.length === 0 ? (
            <p className="text-base text-text-muted">
              No more compatible providers to attach. Add one on the Providers page first.
            </p>
          ) : (
            <>
              <select
                value={selected}
                onChange={(e) => setSelected(e.target.value)}
                className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
                aria-label={`Provider to attach to ${harness.label}`}
              >
                <option value="">Select a provider…</option>
                {offerable.map((a) => (
                  <option key={a.id} value={a.id}>
                    {a.name}
                  </option>
                ))}
              </select>
              {needsKey && (
                <div>
                  <label className="block text-sm text-text-muted mb-1">
                    API key for {accountName(selected)} (saved globally, set once)
                  </label>
                  <input
                    type="password"
                    value={key}
                    onChange={(e) => setKey(e.target.value)}
                    placeholder="Enter API key…"
                    className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
                    aria-label={`${accountName(selected)} API key`}
                  />
                </div>
              )}
              <div className="flex gap-3">
                <button
                  onClick={submitAttach}
                  // A keyless provider needs a key to reach its endpoint, so gate
                  // Attach on the inline key when the provider has none stored.
                  disabled={busy || !selected || (needsKey && !key.trim())}
                  className="px-5 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30 disabled:opacity-50"
                >
                  {busy ? 'Attaching…' : 'Attach'}
                </button>
                <button
                  onClick={reset}
                  disabled={busy}
                  className="px-5 py-2 text-base text-text-muted hover:text-text-secondary disabled:opacity-50"
                >
                  Cancel
                </button>
              </div>
            </>
          )}
        </div>
      ) : (
        <button
          onClick={() => setAdding(true)}
          className="mt-3 text-base text-accent-cyan hover:text-accent-cyan/80"
        >
          + Add proxied provider
        </button>
      )}
    </div>
  );
}
