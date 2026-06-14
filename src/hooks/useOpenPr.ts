import { createPathInvalidatedCache } from '../lib/pathInvalidatedCache';
import { usePathInvalidatedQuery } from './usePathInvalidatedQuery';
import { getOpenPrForNode, type OpenPr } from '../lib/tauri';

// Module-level shared client (keyed by nodeId). Multiple `useOpenPr`
// instances for the same nodeId dedup their fetches via the primitive's
// pending map; instances for different nodeIds each have their own cache
// entry, sharing the bus-level GIT_CHANGED subscription.
const prClient = createPathInvalidatedCache<number, OpenPr>({
  fetcher: getOpenPrForNode,
  name: 'useOpenPr',
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
