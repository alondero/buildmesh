import { createDualKeyCache } from '../lib/pathInvalidatedCache';
import { usePathInvalidatedQuery } from './usePathInvalidatedQuery';
import { getOpenPrForNode, type OpenPr } from '../lib/tauri';

// Module-level shared client (keyed by nodeId, with a separate git path
// the GIT_CHANGED subscription matches). Multiple `useOpenPr` instances
// for the same nodeId dedup their fetches via the primitive's pending
// map; instances for different nodeIds each have their own cache entry,
// sharing the bus-level GIT_CHANGED subscription.
const prClient = createDualKeyCache<number, OpenPr>({
  fetcher: getOpenPrForNode,
  name: 'useOpenPr',
  // Every agent file-write fires GIT_CHANGED, but the fetcher is a live
  // GitHub API call — without a freshness window a busy agent burns
  // thousands of requests/hour into the rate limit re-fetching a PR state
  // that changes rarely. 60s caps that at one request per node per minute.
  // Manual `refresh()` is not gated, so create/merge flows can force an
  // immediate re-fetch when they wire it up (the v1.1 noted below).
  minRefetchIntervalMs: 60_000,
});

/**
 * Hook that fetches the open PR for an agent node's branch, if any.
 *
 * Returns `null` for the common cases (no PR, no auth, not a git repo,
 * branch unborn, non-GitHub origin) — the chip is hidden whenever
 * `pr === null`.
 *
 * Cached per nodeId; auto-refetched on `GIT_CHANGED` for the node's git
 * path. The `refresh()` handle is exposed for manual invalidation
 * (e.g. after a PR is created/merged via buildmesh's own flow — wiring
 * that up is a v1.1).
 */
export function useOpenPr(nodeId: number, gitPath: string | null): {
  pr: OpenPr | null;
  loading: boolean;
  refresh: () => void;
} {
  const { data, loading, refresh } = usePathInvalidatedQuery(
    prClient,
    nodeId,
    gitPath,
  );
  return { pr: data, loading, refresh };
}
