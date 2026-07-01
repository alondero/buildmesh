/**
 * UsageTab — the Probe Panel's "Usage" tab body (issue #601).
 *
 * The dedicated glanceable surface for Usage Meters. Lives outside the
 * Settings modal so the user can check quota / balance without opening
 * a config panel. Entry point is the ⏱ icon in the Probe Panel's
 * always-visible activity bar (which stays visible even when the panel
 * body is collapsed — the canonical sidebar affordance for #601).
 *
 * Data flow:
 *   - `get_provider_meters` returns detection-gated rows (issue #574):
 *     a native harness's subscription meter only when the harness is
 *     installed, a keyed provider's meter when its account is enabled.
 *   - `get_provider_accounts` returns the full editable account list
 *     (the join is for the name + icon; the tab is read-only).
 *   - The tab joins each meter to its account by id and renders one
 *     `<UsagePanel>` per pair. Meters whose account no longer exists
 *     (e.g. a custom provider was removed) are silently dropped — the
 *     tab never renders a bare meter without a name.
 *   - `useProviderListInvalidation` re-fetches when the Rust backend
 *     emits `provider-list-changed` on upsert/remove so a toggled or
 *     removed provider's meter updates without a manual Refresh click.
 *
 * Read-only by design (issue #601): the tab has no Edit-credentials /
 * enable-toggle / Remove affordance. Those live on the Settings-side
 * AccountCard, where the user goes to actually change something.
 */

import { useState, useCallback } from 'react';
import * as api from '../../lib/tauri';
import type { ProviderAccount, ProviderMeters } from '../../lib/tauri';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { useProviderListInvalidation } from '../../hooks/useProviderListInvalidation';
import { UsagePanel } from '../AppSettings/UsageRender';

export function UsageTab() {
  const [meters, setMeters] = useState<ProviderMeters[] | null>(null);
  const [accounts, setAccounts] = useState<ProviderAccount[]>([]);
  const [error, setError] = useState<string | null>(null);
  // Reset in `finally` so a rejected fetch can't leave the button stuck
  // disabled. The flag is set synchronously before the await so React
  // renders the busy state before the IPC roundtrip.
  const [isRefreshing, setIsRefreshing] = useState(false);

  const loadMeters = useCallback(async (force: boolean) => {
    try {
      const [meterRows, accountRows] = await Promise.all([
        api.getProviderMeters(force),
        api.getProviderAccounts(),
      ]);
      setMeters(meterRows);
      setAccounts(accountRows);
      setError(null);
    } catch (e) {
      // Non-fatal: the tab keeps the previous rows so a transient IPC
      // failure doesn't blank the glance surface. The error surfaces
      // as a small banner above the rows.
      console.error('Failed to load usage:', e);
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  // Initial load — non-forced read so the backend's 5-minute cache
  // (#574) can short-circuit if another tab just refreshed.
  useAsyncEffect(() => {
    loadMeters(false);
  }, [loadMeters]);

  // Cross-surface invalidation (issue #601 review): when the user enables,
  // disables, or removes a provider in `AppSettingsModal`, the Rust backend
  // emits `provider-list-changed`. Subscribing here means a toggled provider's
  // meter updates the next time the tab is opened (no manual Refresh click),
  // and a removed provider's row drops out immediately if the tab is already
  // mounted. Same hook every other Probe tab uses — module-scope event-name
  // constant lives in `useProviderListInvalidation.ts` (Rust+TS drift guard).
  useProviderListInvalidation(() => { void loadMeters(false); });

  // Loading state: the tab has not yet received its first response.
  // Render a neutral placeholder rather than an empty list, so a
  // returning user doesn't read "0 meters" as "no providers
  // configured".
  if (meters === null) {
    return (
      <div className="flex items-center justify-center h-full p-6 text-text-muted">
        <span className="text-sm">Loading usage…</span>
      </div>
    );
  }

  // Join meter → account by id. Accounts may have been removed since
  // the meter was fetched (custom provider deleted); those rows are
  // silently dropped (issue #601: a bare meter without a name is
  // meaningless on a glance surface).
  const rows = meters
    .map(meter => ({ meter, account: accounts.find(a => a.id === meter.provider) }))
    .filter((row): row is { meter: ProviderMeters; account: ProviderAccount } => row.account != null);

  // CLAUDE.md "user.click swallows async onClick rejections": `loadMeters` is
  // async and rejects on backend failure. Wrap the Refresh click in a
  // try/catch so any uncaught throw (e.g. a future refactor that escapes the
  // internal try/catch) lands in the error banner instead of becoming an
  // unhandled rejection that fails the test suite. `finally` resets
  // `isRefreshing` so a rejected fetch can't leave the button stuck
  // disabled (the safety-net catch on its own would silently swallow).
  const handleRefresh = async () => {
    setIsRefreshing(true);
    try {
      await loadMeters(true);
    } catch {
      // loadMeters already updates the error banner; this catch is just
      // a safety net for anything that escapes its internal try/catch.
    } finally {
      setIsRefreshing(false);
    }
  };

  return (
    <div className="flex flex-col h-full">
      {/* Refresh + summary header. Stays light — the tab is a glance
          surface, not a config one. The Refresh button forces a
          refetch bypassing the backend's 5-min cache. */}
      <div className="flex items-center justify-between px-3 py-2 border-b border-border-subtle">
        <span className="text-xs text-text-muted">
          {rows.length === 0
            ? 'No meters to display'
            : `${rows.length} provider${rows.length === 1 ? '' : 's'} tracked`}
        </span>
        <button
          type="button"
          onClick={handleRefresh}
          disabled={isRefreshing}
          aria-busy={isRefreshing}
          aria-label="Refresh usage"
          className="text-xs text-accent-cyan hover:text-accent-cyan/80 disabled:cursor-not-allowed disabled:hover:text-accent-cyan inline-flex items-center gap-1.5"
        >
          {isRefreshing && (
            <span
              className="inline-block h-3 w-3 animate-spin rounded-full border border-current border-t-transparent"
              aria-hidden="true"
            />
          )}
          Refresh
        </button>
      </div>

      {error && (
        <div
          role="alert"
          className="mx-3 mt-2 px-3 py-2 bg-bg-card border border-status-warning/40 rounded-md text-xs text-status-warning"
        >
          {error}
        </div>
      )}

      <div
        data-testid="usage-rows"
        aria-busy={isRefreshing}
        className={`flex-1 overflow-y-auto p-3 space-y-3 transition-opacity ${isRefreshing ? 'opacity-60' : ''}`}
      >
        {rows.length === 0 ? (
          <div className="text-center py-8 text-text-muted text-xs">
            No usage meters available. Enable a provider in Settings to see
            its quota or balance here.
          </div>
        ) : (
          rows.map(({ meter, account }) => (
            <UsagePanel key={meter.provider} account={account} meter={meter} />
          ))
        )}
      </div>
    </div>
  );
}
