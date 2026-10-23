import { PROBE_TAB_DEFINITIONS } from '../../lib/probeContext';
import { useProbeResource, type ProbeLoadContext } from '../../lib/probeResource';
/**
 * UsageTab — the Probe Panel's "Usage" tab body (issue #601).
 *
 * The dedicated glanceable surface for Usage Meters. Lives outside the
 * Settings modal so the user can check quota / balance without opening
 * a config panel. Entry point is the Usage icon in the Probe Panel's
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
 *     tab never renders a bare meter without a name. Meters whose
 *     account is `enabled = false` are also dropped: the probe is the
 *     glanceable Usage surface and the user contract is "if a provider
 *     isn't enabled I don't want to see its meter here". The Settings-
 *     side AccountCard still keeps the disabled card with the enable
 *     toggle so the user can flip it back on.
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
 *   Warm-cache refresh rejection keeps the rows (PR #1488 review
 *   follow-ups, items 1–7): when prior rows are loaded and a refresh
 *   fails, the rows stay on screen and a `role="alert"` banner
 *   surfaces the failure *outside* the toolbar — see
 *   `probe-ui-checklist.md` §1.4 ("Errors render in a role='alert'
 *   region that is shrink-0 and outside the scroller"). The toolbar
 *   used to carry an inline `role="status"` chip in the count row,
 *   which competed with two other `shrink-0` chips for the dock's
 *   240px narrow width and pushed the Refresh button off-panel. The
 *   body falls back to `<ErrorState>` ONLY when no rows have ever
 *   loaded (cold-cache first-load rejection).
 *
 *   The shared Probe resource owns request sequencing and busy feedback.
 *   A background invalidation that supersedes a manual Refresh keeps its
 *   spinner until the newest read settles.
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
 * Issue #857 — fetch-failure error UI de-duplication (revised by #1488)
 * ---------------------------------------------------------------------
 * The pre-#857 tab rendered the IPC error message in *two* places when
 * the resource read rejected after rows had loaded: the inline red alert
 * banner above the rows region AND the body's `<ErrorState>`. The
 * duplicated copy is the exact pattern the shared vocabulary was
 * created to prevent (issue #813).
 *
 * PR #1508 (the follow-up to #1488) broke the inline alert chip out of
 * the toolbar into a separate `role="alert"` banner sibling — per
 * `probe-ui-checklist.md` §1.4 ("Errors render in a role='alert'
 * region that is shrink-0 and outside the scroller, so it can't
 * scroll out of sight"). PR #1488 itself had only crammed a
 * `role="status"` chip into the toolbar's count row (which
 * overflowed the 240px dock — review item #6 in the #1508 follow-up
 * patch). The body still renders a single `<ErrorState>` on a
 * cold-cache first-load rejection; the warm-cache refresh rejection
 * surfaces the alert banner while the rows stay visible. The
 * regression test (`warm-cache refresh rejection keeps the rows
 * visible and surfaces a role="alert" banner outside the toolbar`)
 * pins the new placement.
 *
 * Cache age wire gap (issue #857 follow-up — out of scope here)
 * -------------------------------------------------------------
 * The "Refreshed just now" / "Xs ago" indicator is stamped on the
 * React side at the moment the resource read resolves, NOT at the moment
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

import { useState, useEffect } from 'react';
import * as api from '../../lib/tauri';
import type { ProviderAccount, ProviderMeters } from '../../lib/tauri';
import { useProviderListInvalidation } from '../../hooks/useProviderListInvalidation';
import { useOpencodeAccountInvalidation } from '../../hooks/useOpencodeAccountInvalidation';
import { UsagePanel } from '../AppSettings/UsageRender';
import {
  EmptyState,
  ErrorState,
  LoadingState,
  RefreshControl,
} from '../shared/Spinner';
import { ProbeTabBody } from './ProbeTabBody';
import { ProbeToolbar } from './ProbeToolbar';
import { formatRelativeAge } from '../../lib/time';

/**
 * Self-ticking "Refreshed X ago" label (issue #1751). Owns its 1s
 * interval so the parent tab never re-renders on the clock: only this
 * span updates. `since` is fixed at the last successful load — a failed
 * refresh leaves the previous timestamp in place, so the label keeps
 * pointing at the last known-good moment. Cleared on unmount.
 */
export function RefreshedAgo({ since }: { since: Date }) {
  const [, setTick] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => setTick((n) => n + 1), 1000);
    return () => window.clearInterval(id);
  }, []);
  // Recomputed every render so the 1s tick above drives a re-render
  // with a fresh `now`. `formatRelativeAge` is pure. The 1s cadence is
  // the smallest grain displayed ("Ns ago"); coarser labels ("Xm ago")
  // tick on minute boundaries for free.
  const relative = formatRelativeAge(since, new Date(), { granularity: 'second' });
  const absolute = since.toLocaleTimeString();
  return (
    <span
      className="text-2xs text-text-muted shrink-0"
      data-testid="usage-last-refreshed"
    >
      <time
        dateTime={since.toISOString()}
        aria-label={`Last refreshed at ${absolute}`}
        title={absolute}
        className="cursor-default tabular-nums"
      >
        {`Refreshed ${relative}`}
      </time>
    </span>
  );
}

export async function loadUsage({ force }: ProbeLoadContext) {
  const [meters, accounts] = await Promise.all([
    api.getProviderMeters(force ?? false), api.getProviderAccounts(),
  ]);
  return { meters, accounts, refreshedAt: new Date() };
}

export function UsageTab() {
  const resource = useProbeResource(PROBE_TAB_DEFINITIONS.usage.load, { meshId: null }, 'host');
  const { error, attempted, refreshing: isRefreshing } = resource.state;
  const meters = resource.state.data?.meters ?? null;
  const accounts = resource.state.data?.accounts ?? [];
  const lastRefreshedAt = resource.state.data?.refreshedAt ?? null;

  // Cross-surface invalidation (issue #601 review): when the user enables,
  // disables, or removes a provider in `AppSettingsModal`, the Rust backend
  // emits `provider-list-changed`. Subscribing here means a toggled provider's
  // meter updates the next time the tab is opened (no manual Refresh click),
  // and a removed provider's row drops out immediately if the tab is already
  // mounted. Same hook every other Probe tab uses — module-scope event-name
  // constant lives in `useProviderListInvalidation.ts` (Rust+TS drift guard).
  useProviderListInvalidation(() => { void resource.reload(); });

  // OpenCode account surface — when the user signs in, signs out, or
  // switches workspaces, the `opencode-console-changed` event fires
  // from Rust (see `commands/opencode_oauth.rs::emit_opencode_console_changed`).
  // The Rust-side per-provider cache is also invalidated at the same
  // emit sites, so a `force=true` re-fetch here can't hit a stale
  // envelope. Without this, a freshly-signed-in user would see
  // outdated numbers for up to 5 minutes (the `CACHE_TTL`).
  useOpencodeAccountInvalidation(() => { void resource.reload({ force: true }); });

  // No tab-level ticker (issue #1751): the "Refreshed X ago" label
  // ticks inside `RefreshedAgo`, so the tab itself never re-renders on
  // the 1s cadence — the clock only re-renders the label span.

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

  // Join meter → account by id. Orphan meters (account deleted) and
  // disabled providers are dropped — see the data-flow comment above
  // for the rationale (glanceable surface; re-enable lives in Settings).
  const rows = meters
    ? meters
        .map(meter => ({ meter, account: accounts.find(a => a.id === meter.provider) }))
        .filter((row): row is { meter: ProviderMeters; account: ProviderAccount } =>
          row.account != null && row.account.enabled
        )
    : [];

  const handleRefresh = () => resource.reload({ force: true, manual: true });

  // The staleness indicator hides entirely until the first successful
  // load completes (the LoadingState placeholder is showing anyway, and
  // a first-load rejection has no trustworthy timestamp to point at).
  // `RefreshedAgo` owns the ticking from there (issue #1751).

  // Warm-cache refresh rejection keeps the rows. When prior rows exist
  // AND the latest fetch rejected, keep the rows on screen — the
  // alert banner below surfaces the failure without blanking the
  // body. When no rows have ever loaded AND the latest fetch
  // rejected, fall back to the body's `<ErrorState>` so the user sees
  // something other than an empty placeholder. The toolbar stays a
  // count / refresh / staleness strip; the alert lives in its own
  // row outside the scroller (probe-ui-checklist.md §1.4), which
  // fixes the previous design's 240px dock overflow (review item #6
  // — four `shrink-0` chips in the toolbar were pushing the Refresh
  // button off-panel at the narrowest dock width).
  const hasRows = rows.length > 0;
  const showInlineError = !hasRows && error !== null;

  return (
    <div className="flex flex-col h-full">
      <ProbeToolbar
        trailing={
          <RefreshControl
            onRefresh={handleRefresh}
            isRefreshing={isRefreshing}
            ariaLabel="Refresh usage"
          />
        }
      >
        {hasRows && (
          <span className="text-xs text-text-muted shrink-0">
            {`${rows.length} provider${rows.length === 1 ? '' : 's'} tracked`}
          </span>
        )}
        {lastRefreshedAt !== null && <RefreshedAgo since={lastRefreshedAt} />}
      </ProbeToolbar>

      {/* Refresh-failure alert. Lives OUTSIDE the toolbar and OUTSIDE
          the body's scroller (probe-ui-checklist.md §1.4: "Errors
          render in a role='alert' region that is shrink-0 and outside
          the scroller, so it can't scroll out of sight"). The
          previous design put this chip in the toolbar's count row,
          where it competed with two other `shrink-0` chips for the
          dock's 240px narrow width and pushed the Refresh button
          off-panel — review item #6. The actual error text is
          rendered IN the banner (NOT just inside `title={error}`),
          so it's reachable to touch users and announced by AT in
          full — probe-ui-checklist.md §5 ("A failure is never only
          behind a disclosure"). `break-words` lets long error text
          wrap (checklist §2.1: wrap, don't truncate, anything
          unbounded; the tail carries the diagnosis). Suppressed
          during `isRefreshing` to avoid double-signalling the
          in-flight refresh. */}
      {error !== null && hasRows && !isRefreshing && (
        <div
          role="alert"
          data-testid="usage-refresh-error"
          className="px-3 py-1 text-xs text-status-error shrink-0 break-words"
          title={error}
        >
          <span className="font-semibold">Refresh failed:</span>{' '}
          <span className="break-words">{error}</span>
          {' — showing last known data'}
        </div>
      )}

      <ProbeTabBody
        data-testid="usage-rows"
        aria-busy={isRefreshing}
        className={`space-y-3 transition-opacity ${
          isRefreshing && hasRows ? 'opacity-60' : ''
        }`}
      >
        {showInlineError ? (
          <ErrorState title="Failed to load usage" detail={error} />
        ) : rows.length === 0 ? (
          <EmptyState
            label="No usage meters available."
            hint="Add credentials for a provider in Settings (API key for MiniMax/Kimi/OpenRouter, or log in to Claude/Codex/Antigravity's CLI) to see its quota or balance here."
          />
        ) : (
          rows.map(({ meter, account }) => (
            <UsagePanel key={meter.provider} account={account} meter={meter} />
          ))
        )}
      </ProbeTabBody>
    </div>
  );
}
