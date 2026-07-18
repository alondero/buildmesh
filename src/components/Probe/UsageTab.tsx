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
 * Cache staleness indicator (issue #856):
 *   The header renders a "Refreshed X ago" label next to the count +
 *   Refresh button. The backend's `get_provider_meters` has a 5-minute
 *   TTL (#574) so a fresh read may be served from cache without
 *   re-hitting each provider — the indicator tells the user how stale
 *   their view could possibly be. It's set only on a SUCCESSFUL load
 *   (initial mount, cross-surface invalidation, manual Refresh); a
 *   failed refresh leaves the previous timestamp in place so the label
 *   continues to refer to the last known-good moment.
 *
 *   OPEN FOLLOW-UP (issue #857, deferred — see "Cache age wire gap"
 *   below): the indicator currently labels every successful read
 *   "Refreshed X ago" even when the Rust 5-minute cache served the
 *   response without contacting any vendor. The wire shape needs
 *   `cachedAt: Option<i64>` on `ProviderMeters` so the React side can
 *   distinguish "fresh fetch" from "cache hit" — see the comment block
 *   for the proposed design.
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
 *
 * Issue #857 — fetch-failure error UI de-duplication
 * --------------------------------------------------
 * Pre-#857 the tab rendered the IPC error message in *two* places when
 * `loadMeters` rejected after rows had loaded: the inline red alert
 * banner above the rows region AND the body's `<ErrorState>`. The
 * duplicated copy is the exact pattern the shared vocabulary was
 * created to prevent (issue #813). The fix drops the inline banner
 * entirely so the body renders a single `<ErrorState>` — matching
 * GitIssuesTab / GitPullRequestsTab / ArchivedNodesTab. The
 * previously-existing test had to use `findAllByText` + an explicit
 * "presence-not-uniqueness" comment to dodge the duplication; that
 * test now uses `findByText` (uniqueness) and is pinned by a new
 * regression test (`renders exactly one error element on a forced-
 * refresh rejection after rows have loaded`).
 *
 * Cache age wire gap (issue #857 follow-up — out of scope here)
 * -------------------------------------------------------------
 * The "Refreshed just now" / "Xs ago" indicator is stamped on the
 * React side at the moment `loadMeters` resolves, NOT at the moment
 * each provider's vendor endpoint returned. The Rust cache
 * (`services/usage.rs::CACHE_TTL`, 5 min) means a request can come
 * back fast without any vendor being contacted — the indicator
 * then mislabels a cache hit as a fresh fetch. The fix is a wire-
 * shape change:
 *
 *   1. Add `cachedAt: Option<i64>` (epoch ms) to `ProviderMeters`.
 *      `None` = freshly fetched on this call; `Some(ms)` = served
 *      from the in-process cache at that instant.
 *   2. `services::usage::get_cached_usage` already keeps the
 *      `Instant` next to the cached `ProviderUsage`; expose it
 *      alongside the usage (e.g. `Option<(ProviderUsage, Instant)>`)
 *      and have `cached_or_fetch` thread the Optional instant through
 *      to `assemble_meters`, which stamps `cachedAt` on each row.
 *   3. `cargo test` regenerates `src/types/generated/ProviderMeters.ts`
 *      with `cachedAt: number | null` per the project's ts-rs gate.
 *   4. The React side computes a single display timestamp: the
 *      `cachedAt` if every row carried one (pure cache hit, label
 *      switches to "Cached Xs ago"), otherwise `Date.now()` (at
 *      least one row is fresh, keep "Refreshed Xs ago").
 *
 * Deferred to its own PR per issue #857 (the body flags the
 * cross-cutting consequences — Rust struct, ts-rs regen, mixed-cache
 * UI semantics — as warranting a separate commit).
 */

import { formatError } from '../../lib/errorUtils';
import { useState, useCallback, useEffect } from 'react';
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
import { ProbeTabBody } from './ProbeTabBody';
import { formatRelativeAge } from '../../lib/time';

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
  // Wall-clock timestamp of the last successful load. Updated only when
  // `loadMeters` resolves without throwing — a failed refresh leaves the
  // previous timestamp in place so the staleness indicator continues
  // to point at the last known-good moment. `null` until the very
  // first successful load completes (the header hides the indicator
  // during the initial loading state and after a first-load rejection).
  const [lastRefreshedAt, setLastRefreshedAt] = useState<Date | null>(null);

  const loadMeters = useCallback(async (force: boolean) => {
    try {
      const [meterRows, accountRows] = await Promise.all([
        api.getProviderMeters(force),
        api.getProviderAccounts(),
      ]);
      // Stamp the indicator FIRST so the timestamp describes the data
      // we're about to set. JS is single-threaded through the await
      // resolve + these setStates, so there is no race — either all
      // three setStates commit (and the indicator correctly reflects
      // them) or the `Promise.all` rejects and none of them do (and
      // the previous timestamp stays in place).
      setLastRefreshedAt(new Date());
      setMeters(meterRows);
      setAccounts(accountRows);
      setError(null);
    } catch (e) {
      console.error('Failed to load usage:', e);
      setError(formatError(e));
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

  // Tick once a second so the "Refreshed X ago" label updates without
  // a re-fetch. Cheap (one no-op render), and only mounted while the
  // tab is alive — the cleanup drops the interval when the tab
  // unmounts. `now` is the only piece of state this drives; `formatRelativeAge`
  // is pure, so we don't need to track `lastRefreshedAt` in a ref. The
  // 1s cadence is the smallest grain we display ("Ns ago"); the
  // higher-grain labels ("Xm ago") tick on minute boundaries for free
  // because the render recomputes from the fresh `Date.now()`.
  const [, setTicker] = useState(0);
  useEffect(() => {
    if (lastRefreshedAt === null) return;
    const id = window.setInterval(() => setTicker((n) => n + 1), 1000);
    return () => window.clearInterval(id);
  }, [lastRefreshedAt]);

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

  // Staleness indicator values. Computed every render so the 1s tick
  // effect can drive a re-render with a fresh `now`. Both are `null`
  // until the first successful load completes — the header hides the
  // indicator entirely in that window (the LoadingState placeholder
  // is showing anyway, and a first-load rejection has no trustworthy
  // timestamp to point at).
  const refreshedRelative = lastRefreshedAt
    ? formatRelativeAge(lastRefreshedAt, new Date(), { granularity: 'second' })
    : null;
  const refreshedAbsolute = lastRefreshedAt
    ? lastRefreshedAt.toLocaleTimeString()
    : null;

  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center justify-between gap-2 px-3 py-2 border-b border-border-subtle">
        <div className="flex items-center gap-2 min-w-0">
          {rows.length > 0 && (
            <span className="text-xs text-text-muted shrink-0">
              {`${rows.length} provider${rows.length === 1 ? '' : 's'} tracked`}
            </span>
          )}
          {refreshedRelative !== null && (
            <span
              className="text-2xs text-text-muted/80 shrink-0"
              data-testid="usage-last-refreshed"
            >
              <time
                dateTime={lastRefreshedAt!.toISOString()}
                aria-label={`Last refreshed at ${refreshedAbsolute}`}
                title={refreshedAbsolute!}
                className="cursor-default"
              >
                {`Refreshed ${refreshedRelative}`}
              </time>
            </span>
          )}
        </div>
        <RefreshControl
          onRefresh={handleRefresh}
          isRefreshing={isRefreshing}
          ariaLabel="Refresh usage"
        />
      </div>

      <ProbeTabBody
        data-testid="usage-rows"
        aria-busy={isRefreshing}
        className={`space-y-3 transition-opacity ${isRefreshing ? 'opacity-60' : ''}`}
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
      </ProbeTabBody>
    </div>
  );
}
