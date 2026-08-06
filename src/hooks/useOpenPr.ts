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
  // Manual `refresh()`/`invalidate()` are not gated, so buildmesh's own
  // spawn / create / merge flows force an immediate re-fetch (issues #780,
  // #1004).
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
 * path. The `refresh()` handle is exposed for manual invalidation, and
 * buildmesh's own PR-state write-paths force a refetch without a mounted
 * hook via `refreshOpenPrByPath` (merge, issue #780) /
 * `invalidateOpenPrForNode` (spawn + autopilot wrap-up, issue #1004).
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

/**
 * Force-invalidate the Open PR cache for every subscriber whose git
 * path matches `gitPath`, bypassing the 60s `minRefetchIntervalMs`
 * freshness window on bus-driven refetch.
 *
 * Use after buildmesh's own write-paths that change a node's PR state
 * (the Probe Panel's `merge_pr` flow is the current caller — issue
 * #780). Without this, the Open PR chip in `GridNodeHeader` would lag
 * up to 60s waiting for the freshness window to expire before
 * re-fetching, even though GitHub's response has already flipped to
 * `null`.
 *
 * The match-by-path is sufficient because each `useOpenPr` instance
 * subscribes to its own node's resolved `gitPath` (`getNodeGitPath`).
 * The Probe Panel passes the merged PR's head-ref-owning node's
 * `getNodeGitPath(node)` to scope the invalidation to exactly that
 * chip. Callback-only subscribers on the same path (from
 * `subscribeGitPathInvalidation`) also fire.
 *
 * Symmetric to the hook's `refresh()` — both drop the freshness stamp
 * + trigger a re-fetch — but doesn't require a hook instance to be
 * mounted. Caller only needs to know the node's `gitPath`, which the
 * Probe Panel derives from `agentNodes` filtered by branch match.
 */
export function refreshOpenPrByPath(gitPath: string): void {
  prClient.notifyByPath(gitPath);
}

/**
 * Same as `refreshOpenPrByPath`, plus an explicit `invalidate(nodeId)` so
 * the node's cache entry is dropped even when no `useOpenPr` instance for
 * it is currently mounted (`notifyByPath` only reaches live subscribers, so
 * on its own it would leave a stale entry to be served — freshness stamp
 * intact — the next time the header mounts).
 *
 * Callers go through `invalidateNodeCaches` — see that module for why a
 * fresh spawn and an autopilot wrap-up both need this.
 */
export function invalidateOpenPrForNode(nodeId: number, gitPath: string): void {
  prClient.invalidate(nodeId);
  prClient.notifyByPath(gitPath);
}
