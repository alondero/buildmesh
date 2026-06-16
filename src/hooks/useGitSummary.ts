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
