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
 * Hook that fetches and caches git summary for a given path.
 *
 * Returns `summary: null` when the summary has no changes (total === 0) —
 * the consuming panel hides itself in that case. A `summary` with a positive
 * `total` is the only "visible" state.
 *
 * Auto-refetched on `GIT_CHANGED` for the watched path (via the shared
 * primitive in `pathInvalidatedCache.ts`).
 */
export function useGitSummary(gitPath: string | null): {
  summary: GitSummary | null;
  loading: boolean;
  refresh: () => void;
} {
  const { data, loading, refresh } = usePathInvalidatedQuery(
    summaryClient,
    gitPath,
  );
  // Match the original hook's public shape: hide zero-total summaries as
  // `null` so consumers can render a single `summary ? <Panel /> : null`.
  const summary = data && data.total > 0 ? data : null;
  return { summary, loading, refresh };
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
