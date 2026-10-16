import { createPathKeyedCache } from '../lib/pathInvalidatedCache';
import { usePathInvalidatedQuery } from './usePathInvalidatedQuery';
import { getGitSummary, type GitSummary } from '../lib/tauri';

// Module-level shared client — one process, one cache, one subscription map
// per hook family. The `useGitSummary` consumer is a single component, but
// multiple instances of it (one per mesh/node) share this client and dedup
// their fetches via the primitive's pending map.
const summaryClient = createPathKeyedCache<GitSummary>({
  fetcher: getGitSummary,
  name: 'useGitSummary',
  // The fetcher is a full-repo `git status` walk. While an agent streams
  // edits, the backend coalescer fires GIT_CHANGED up to ~2/s per watched
  // node; refetching the walk at that rate (times every visible panel) is
  // part of the steady-state load that made the app feel sluggish. 2s
  // bounds the rate; the primitive's trailing refetch still lands the
  // settled state after a burst.
  minRefetchIntervalMs: 2_000,
});

/**
 * Outcome of one git-summary observation (issue #2328).
 */
export interface GitSummaryResult {
  /**
   * Last successful summary with at least one change, or `null`. A repo
   * that was observed clean yields `null` exactly like one that was never
   * observed or whose fetch failed — `error` is what separates those three,
   * so a consumer must not read `summary === null` as "clean".
   */
  summary: GitSummary | null;
  loading: boolean;
  /** Most recent failure for this path, or `null`. Cleared by any success. */
  error: Error | null;
  /**
   * `true` when the fetch failed and there is no summary to show. That covers
   * two cases a compact surface must not let look alike: the status was never
   * observed, and it was observed clean and then a refresh broke. Both render
   * nothing, so the surface says "changes unavailable" instead of staying
   * silent and letting the silence read as "clean".
   */
  unavailable: boolean;
  /**
   * `true` when the fetch failed but an earlier summary is still being shown.
   * Those counts are real but may be out of date. `false` whenever
   * `summary` is `null` — there is nothing on screen to be stale.
   */
  stale: boolean;
  refresh: () => void;
}

/**
 * Hook that fetches and caches git summary for a given path.
 *
 * Returns `summary: null` when the summary has no changes (total === 0) —
 * the consuming panel hides itself in that case. A `summary` with a positive
 * `total` is the only "visible" state.
 *
 * Issue #2328 adds `error` / `unavailable` / `stale` so that hidden-because-
 * clean and hidden-because-broken stop looking identical. `summary` keeps its
 * old shape, so `summary ? <Panel /> : null` call sites still compile — they
 * just have to decide what the other two states look like.
 *
 * Auto-refetched on `GIT_CHANGED` for the watched path (via the shared
 * primitive in `pathInvalidatedCache.ts`).
 */
export function useGitSummary(gitPath: string | null): GitSummaryResult {
  const { data, loading, error, refresh } = usePathInvalidatedQuery(
    summaryClient,
    gitPath,
  );
  // Match the original hook's public shape: hide zero-total summaries as
  // `null` so consumers can render a single `summary ? <Panel /> : null`.
  const summary = data && data.total > 0 ? data : null;
  // Both flags are derived from `summary`, not from `data`. `summary` is what
  // a consumer actually renders, so deriving from it keeps the flags honest
  // about that. Deriving from `data` opened a hole on review of #2389: an
  // observed-clean repo keeps `data = { total: 0 }` after a failed refresh, so
  // `data`-based `unavailable` was false while the zero-total projection had
  // already collapsed `summary` to `null` — the node header then rendered
  // nothing at all, showing a broken status identically to a healthy clean one,
  // which is the exact confusion issue #2328 exists to remove.
  const broken = error !== null;
  return {
    summary,
    loading,
    error,
    unavailable: broken && summary === null,
    stale: broken && summary !== null,
    refresh,
  };
}

/**
 * Drop the cached git summary for `gitPath` and drive an immediate refetch
 * in every mounted subscriber of that path, bypassing the 2s
 * `minRefetchIntervalMs` freshness window.
 *
 * `invalidate` covers subscribers that are NOT mounted (their stale entry
 * would otherwise be served on the next mount, freshness stamp intact);
 * `notifyByPath` covers the mounted ones, which re-fetch exactly as they
 * would on a `GIT_CHANGED` event. Mirrors `refreshOpenPrByPath` in
 * `useOpenPr.ts`.
 *
 * Callers go through `invalidateNodeCaches` — see that module for why a
 * fresh spawn needs this.
 */
export function invalidateGitSummaryForPath(gitPath: string): void {
  summaryClient.invalidate(gitPath);
  summaryClient.notifyByPath(gitPath);
}
