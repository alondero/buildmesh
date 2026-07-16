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
 *
 * Issue #813 — error colour + empty-state convergence
 * ---------------------------------------------------
 * The pre-#813 tab had three off-spec treatments:
 *   1. The IPC-error banner used `status-warning` (yellow) instead of
 *      the project's standard `status-error` (red). Every other
 *      "fetch failed" indicator in the probe tabs renders in red, so
 *      the yellow here read as "soft warning" rather than "fetch failed"
 *      — flipped to `status-error` so the iconography matches the
 *      Git Issues / PRs / Archive tab treatment.
 *   2. The first-load placeholders used `LoadingState` from day one
 *      (already converged); this commit routes the Refresh button
 *      through the shared `<RefreshControl>` primitive so the spinner
 *      colour, placement, and `aria-busy` semantics match Usage →
 *      Issues / PRs / Archive.
 *   3. The "no meters" case used to render *two* empty messages:
 *      a "No meters to display" header counter AND a longer
 *      onboarding hint in the body. Issue #813 called out the
 *      redundancy; the header counter is now suppressed when
 *      `rows.length === 0`, leaving the body's onboarding copy as
 *      the single voice. The body copy is wrapped in `<EmptyState>`
 *      so its i-icon pairs with `LoadingState` and `ErrorState`
 *      exactly like the Git Issues/PRs/Archive tabs.
 */

import { useState, useCallback } from 'react';
import * as api from '../../lib/tauri';
import type { ProviderAccount, ProviderMeters } from '../../lib/tauri';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { useProviderListInvalidation } from '../../hooks/useProviderListInvalidation';
import { UsagePanel } from '../AppSettings/UsageRender';
import {
  EmptyState,
  ErrorState,
  LoadingState,
  RefreshControl,
} from '../shared/Spinner';

export function UsageTab() {
  const [meters, setMeters] = useState<ProviderMeters[] | null>(null);
  const [accounts, setAccounts] = useState<ProviderAccount[]>([]);
  const [error, setError] = useState<string | null>(null);
  // Flips true once the first load attempt has resolved (success or
  // failure). Distinguishes "first fetch is still in flight" from
  // "first fetch rejected" — the prior `meters === null` early-return
  // hid a first-load rejection behind `<LoadingState>` indefinitely,
  // leaving the user staring at a spinner even though `loadMeters`
  // had already populated `error`. Issue #813 review caught this.
  const [attempted, setAttempted] = useState(false);
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
      console.error('Failed to load usage:', e);
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setAttempted(true);
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

  // First-load placeholder. Only renders before the very first IPC
  // settles — after that, the body renders either the error banner or
  // the loaded rows, even on a refresh (the in-flight Refresh paints
  // its busy state on the rows container rather than blanking the
  // surface — mirrors GitIssuesTab / GitPullRequestsTab).
  if (!attempted) {
    return (
      <div className="flex items-center justify-center h-full p-6">
        <LoadingState label="Loading usage…" />
      </div>
    );
  }

  // Join meter → account by id. Accounts may have been removed since
  // the meter was fetched (custom provider deleted); those rows are
  // silently dropped (issue #601: a bare meter without a name is
  // meaningless on a glance surface).
  const rows = meters
    ? meters
        .map(meter => ({ meter, account: accounts.find(a => a.id === meter.provider) }))
        .filter((row): row is { meter: ProviderMeters; account: ProviderAccount } => row.account != null)
    : [];

  // CLAUDE.md "user.click swallows async onClick rejections": `loadMeters`
  // rejects on backend failure; safety-net catch lands any escaped
  // throw into the error banner instead of an unhandled rejection.
  // `finally` resets `isRefreshing` so a rejected refresh can't leave
  // the button stuck disabled.
  const handleRefresh = async () => {
    setIsRefreshing(true);
    try {
      await loadMeters(true);
    } catch {
      /* loadMeters already updates the error banner */
    } finally {
      setIsRefreshing(false);
    }
  };

  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center justify-between px-3 py-2 border-b border-border-subtle">
        {rows.length > 0 && (
          <span className="text-xs text-text-muted">
            {`${rows.length} provider${rows.length === 1 ? '' : 's'} tracked`}
          </span>
        )}
        <RefreshControl
          onRefresh={handleRefresh}
          isRefreshing={isRefreshing}
          ariaLabel="Refresh usage"
        />
      </div>

      {error && (
        <div
          role="alert"
          className="mx-3 mt-2 px-3 py-2 bg-bg-card border border-status-error/40 rounded-md text-xs text-status-error"
        >
          {error}
        </div>
      )}

      <div
        data-testid="usage-rows"
        aria-busy={isRefreshing}
        className={`flex-1 overflow-y-auto p-3 space-y-3 transition-opacity ${isRefreshing ? 'opacity-60' : ''}`}
      >
        {rows.length === 0 && !error ? (
          <EmptyState
            label="No usage meters available."
            hint="Add credentials for a provider in Settings (API key for MiniMax/Kimi/OpenRouter, or log in to Claude/Codex/Antigravity's CLI) to see its quota or balance here."
          />
        ) : error ? (
          <ErrorState title="Failed to load usage" detail={error} />
        ) : (
          rows.map(({ meter, account }) => (
            <UsagePanel key={meter.provider} account={account} meter={meter} />
          ))
        )}
      </div>
    </div>
  );
}
