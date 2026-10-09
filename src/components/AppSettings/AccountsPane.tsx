/**
 * `AccountsPane` — provider credentials (issue #1880): the per-account
 * cards plus the add-provider flow.
 *
 * This pane owns the account mutation handlers. Each one is optimistic —
 * the local list is updated first so the card reflects the intent
 * immediately, then rolled back if the IPC rejects, so a toggle never
 * lies about what the backend stored. Every post-mutation *refresh* goes
 * through the named resource loaders rather than a raw `api.list*` call,
 * so a refresh failure surfaces in the per-resource banner instead of
 * silently leaving stale data behind a green "loaded" status (issue #1534).
 *
 * Account *state* itself is shared with the Harnesses pane (which resolves
 * account names for proxied rows), so it lives in `SettingsDataContext`
 * rather than here. What is genuinely pane-local is the add-form's open /
 * closed state.
 */
import { useState } from 'react';
import * as api from '../../lib/tauri';
import { formatError } from '../../lib/errorUtils';
import { isSelfAuthId, KEYED_FIRST_CLASS_IDS } from '../../lib/providerClassification';
import { AccountCard } from './AccountCard';
import { AddProviderForm } from './AddProviderForm';
import { OpenCodeAccountCard } from './OpenCodeAccountCard';
import { SettingsSection } from './SettingsRow';
import { ResourceLoadStatus } from './ResourceLoadStatus';
import { useSettingsData } from './SettingsDataContext';
import type { ProviderAccount } from '../../lib/tauri';

export function AccountsPane() {
  const {
    resources,
    prefsLoaded,
    accounts,
    setAccounts,
    keyedCatalog,
    keysInPreferences,
    loadRouting,
    loadAccounts,
    loadProviders,
    retryResource,
    setError,
    siteDirtyChange,
  } = useSettingsData();

  const [addingProvider, setAddingProvider] = useState(false);
  const accountsLoaded = resources.accounts.status === 'loaded';

  /** Refresh everything the accounts list feeds: the list itself, the
   *  keyed catalog that travels with it, and the derived provider menu +
   *  routing dropdowns. Routed through the loaders so a failure lands in
   *  the matching banner rather than as silent staleness. */
  const refreshAccountsAndCatalog = async () => {
    await Promise.all([loadRouting(), loadAccounts(), loadProviders()]);
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
      // Issue #1534 (review round 2) — refresh through the loaders
      // so any failure surfaces in the resource banner instead of
      // silently leaving the optimistic data stale. The optimistic
      // roll-back below catches the *mutation* failure; the loader
      // catches the *refresh* failure.
      await refreshAccountsAndCatalog();
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
      // Route through the loaders (issue #1534 review round 2) so
      // the providers/accounts resource states reflect the post-
      // remove snapshot. A refresh failure surfaces in the per-
      // resource banners, not as silent staleness.
      await refreshAccountsAndCatalog();
    } catch (e) {
      setError(formatError(e));
    }
  };

  /** Materialise a keyed first-class template from the catalog (ADR-0025). */
  const handleAddCatalogProvider = async (template: ProviderAccount) => {
    setError(null);
    try {
      await api.upsertProviderAccount({ ...template, enabled: true });
      siteDirtyChange('add-custom-form', false);
      setAddingProvider(false);
      await refreshAccountsAndCatalog();
    } catch (e) {
      setError(formatError(e));
    }
  };

  /** Create a generic provider — name + API key only (endpoint on attach). */
  const handleAddGeneric = async (name: string, apiKey: string) => {
    const id = name.trim().toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '');
    if (!id) {
      setError('Custom provider needs a name');
      return;
    }
    if (accounts.some(a => a.id === id) || isSelfAuthId(id) || KEYED_FIRST_CLASS_IDS.includes(id)) {
      setError(`A provider with id "${id}" already exists`);
      return;
    }
    const ok = await handleSaveAccount({
      id,
      name: name.trim(),
      enabled: true,
      claude_compatible: true,
      billing_mode: 'pay_as_you_go',
      api_key: apiKey.trim() || null,
    });
    // Issue #1534 (review round 5) — `handleSaveAccount` already
    // refreshes accounts via `loadAccounts`, which (since the
    // round-4 catalog fix) also fetches the catalog best-effort.
    // Re-fetching the catalog here was a duplicate IPC round-trip
    // and could overwrite the loader's fresh state with stale data
    // if the loader's refresh was still in flight. Just dismiss
    // the form; the post-save loader-driven refresh covers both.
    if (ok) {
      siteDirtyChange('add-custom-form', false);
      setAddingProvider(false);
    }
  };

  return (
    <SettingsSection
      title="Accounts"
      description={
        <>
          Self-auth brands always appear (enable + billing). Add keyed first-class
          or generic providers for API keys. Base URL and model names are
          configured when you attach a provider under Harnesses. Usage Meters live
          on the <span className="font-medium">Usage</span> tab in the side panel.
        </>
      }
    >
      {/* Issue #1534 — replace the empty-cards state with an
          explicit error when the accounts fetch failed. Without
          this the pane would silently show "no providers" and the
          + Add provider button as if the catalogue were simply
          empty, when in reality we don't know. OpenCode lives in
          its own sub-section that talks to its own commands, so it
          renders unconditionally below. */}
      {!accountsLoaded ? (
        <ResourceLoadStatus
          resource="accounts"
          state={resources.accounts}
          onRetry={() => retryResource('accounts')}
        />
      ) : (
        <div className="space-y-4">
          {accounts.map(account => (
            <AccountCard
              key={account.id}
              account={account}
              keyInPreferences={keysInPreferences.has(account.id)}
              onSave={handleSaveAccount}
              onRemove={handleRemoveAccount}
              onDirtyChange={d => siteDirtyChange(`account-${account.id}`, d)}
              disabled={!prefsLoaded}
            />
          ))}
        </div>
      )}

      <div className="pt-6 border-t border-border-subtle space-y-4">
        <OpenCodeAccountCard />
      </div>

      {addingProvider ? (
        <AddProviderForm
          catalog={keyedCatalog.filter((t) => !accounts.some((a) => a.id === t.id))}
          onAddCatalog={handleAddCatalogProvider}
          onAddGeneric={handleAddGeneric}
          onCancel={() => {
            siteDirtyChange('add-custom-form', false);
            setAddingProvider(false);
          }}
          onDirtyChange={d => siteDirtyChange('add-custom-form', d)}
          disabled={!prefsLoaded}
        />
      ) : (
        <button
          onClick={() => setAddingProvider(true)}
          disabled={!prefsLoaded}
          className="mt-4 text-base text-text-secondary hover:text-text-primary disabled:opacity-50"
        >
          + Add provider
        </button>
      )}
    </SettingsSection>
  );
}