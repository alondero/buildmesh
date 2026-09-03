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
 *   Request sequencing (review item #7): `loadMeters` tags each
 *   in-flight call with a monotonic id and drops stale resolves /
 *   rejections. Without this, a Refresh click racing an invalidation
 *   re-fetch could overwrite newer state with older data and clear
 *   the spinner mid-flight.
 *
 *   `isRefreshing` is owned by `handleRefresh` only — the mount-time
 *   and invalidation paths don't flip it (the cold-cache early-return
 *   hides the toolbar/body anyway, and invalidations are background).
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
 * `loadMeters` rejected after rows had loaded: the inline red alert
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
import { useState, useCallback, useEffect, useRef } from 'react';
import * as api from '../../lib/tauri';
import type { ProviderAccount, ProviderMeters } from '../../lib/tauri';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
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
  // Owned by `handleRefresh` only (review item #3 — the previous
  // "mount-time wiring" of this flag was a placebo; the cold-cache
  // `<LoadingState>` early-return wins before the toolbar / body is
  // ever rendered). Set synchronously before the await so React
  // renders the busy state before the IPC roundtrip; cleared in
  // `handleRefresh`'s `finally` so a rejected fetch can't leave
  // the button stuck disabled.
  const [isRefreshing, setIsRefreshing] = useState(false);
  // Wall-clock timestamp of the last successful load. Updated only when
  // `loadMeters` resolves without throwing — a failed refresh leaves the
  // previous timestamp in place so the staleness indicator continues
  // to point at the last known-good moment. `null` until the very
  // first successful load completes (the header hides the indicator
  // during the initial loading state and after a first-load rejection).
  const [lastRefreshedAt, setLastRefreshedAt] = useState<Date | null>(null);

  // Request sequencing (#1488 review item #7). When a Refresh click
  // races an invalidation hook, the older promise resolving last
  // would otherwise overwrite newer state with stale data and clear
  // `isRefreshing` mid-flight. Each in-flight call tags itself with a
  // monotonic id; resolves / rejections whose tag no longer matches
  // the latest id are dropped before they touch React state. A ref is
  // correct here — the id is render-irrelevant.
  const requestIdRef = useRef(0);

  // Tracks the latest user-initiated Refresh's id (review #1508 item
  // #2). `handleRefresh`'s `finally` only clears `isRefreshing` if
  // THIS call is still the latest — without this guard, an older
  // Refresh click whose load was dropped as stale (because a newer
  // click or an invalidation hook fired a competing load) would
  // clear the spinner mid-flight. A second counter — separate from
  // `requestIdRef` — means the user-driven lifecycle can be reasoned
  // about independently of the data-staleness guard.
  const refreshIdRef = useRef(0);

  const loadMeters = useCallback(async (force: boolean) => {
    const requestId = ++requestIdRef.current;
    try {
      const [meterRows, accountRows] = await Promise.all([
        api.getProviderMeters(force),
        api.getProviderAccounts(),
      ]);
      // Idempotent — first settled request flips `attempted` for good;
      // later stale settles can't un-flip it. Sequencing below prevents
      // a stale resolve from overwriting newer state.
      setAttempted(true);
      if (requestId !== requestIdRef.current) return;
      setLastRefreshedAt(new Date());
      setMeters(meterRows);
      setAccounts(accountRows);
      setError(null);
    } catch (e) {
      setAttempted(true);
      if (requestId !== requestIdRef.current) return;
      console.error('Failed to load usage:', e);
      setError(formatError(e));
    }
  }, []);

  // Initial load — non-forced. The cold-cache first load renders
  // `<LoadingState>` (see the early-return below); `isRefreshing` is
  // intentionally NOT flipped here because nothing on screen carries
  // the affordance until the early-return is gone (review item #3 —
  // the previous "mount-time wiring" was a placebo).
  useAsyncEffect(() => {
    void loadMeters(false);
  }, [loadMeters]);

  // Cross-surface invalidation (issue #601 review): when the user enables,
  // disables, or removes a provider in `AppSettingsModal`, the Rust backend
  // emits `provider-list-changed`. Subscribing here means a toggled provider's
  // meter updates the next time the tab is opened (no manual Refresh click),
  // and a removed provider's row drops out immediately if the tab is already
  // mounted. Same hook every other Probe tab uses — module-scope event-name
  // constant lives in `useProviderListInvalidation.ts` (Rust+TS drift guard).
  useProviderListInvalidation(() => { void loadMeters(false); });

  // OpenCode account surface — when the user signs in, signs out, or
  // switches workspaces, the `opencode-console-changed` event fires
  // from Rust (see `commands/opencode_oauth.rs::emit_opencode_console_changed`).
  // The Rust-side per-provider cache is also invalidated at the same
  // emit sites, so a `force=true` re-fetch here can't hit a stale
  // envelope. Without this, a freshly-signed-in user would see
  // outdated numbers for up to 5 minutes (the `CACHE_TTL`).
  useOpencodeAccountInvalidation(() => { void loadMeters(true); });

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

  // User-driven Refresh. Owns the `isRefreshing` lifecycle (the mount
  // and invalidation paths don't flip it — the cold-cache early-return
  // hides the surface, and invalidations are background). `loadMeters`
  // swallows rejections internally, so there's no try/catch here
  // either — it would never fire (review item #5). We `await` rather
  // than fire-and-forget so the button's spinner stays up until the
  // IPC settles; `finally` clears the flag ONLY if both (a) THIS call
  // is still the latest user-initiated refresh (no second click
  // overtook us), AND (b) no other `loadMeters` — from any source
  // including background invalidations — has fired since ours
  // (review #1508 item #2). Two ref-based snapshots: `refreshIdRef`
  // catches user-clicks; the post-increment value of `requestIdRef`
  // catches everything.
  const handleRefresh = async () => {
    const myRefreshId = ++refreshIdRef.current;
    const myLoadId = requestIdRef.current + 1;
    setIsRefreshing(true);
    try {
      await loadMeters(true);
    } finally {
      if (
        refreshIdRef.current === myRefreshId &&
        requestIdRef.current === myLoadId
      ) {
        setIsRefreshing(false);
      }
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
        {refreshedRelative !== null && (
          <span
            className="text-2xs text-text-muted/80 shrink-0"
            data-testid="usage-last-refreshed"
          >
            <time
              dateTime={lastRefreshedAt!.toISOString()}
              aria-label={`Last refreshed at ${refreshedAbsolute}`}
              title={refreshedAbsolute!}
              className="cursor-default tabular-nums"
            >
              {`Refreshed ${refreshedRelative}`}
            </time>
          </span>
        )}
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
