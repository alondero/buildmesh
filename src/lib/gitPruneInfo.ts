import { createDualKeyCache } from './pathInvalidatedCache';
import { getGitPruneInfo } from './tauri/git';
import type { GitRepoPruneInfo } from '../types/generated/GitRepoPruneInfo';
import type { ProbeLoadContext } from './probeResource';

/**
 * Shared cache for the Probe's Repository destination read (issue #2580).
 *
 * The prune read is expensive: it enumerates every local branch and every
 * worktree of the mesh's repository. It used to bypass the shared
 * `pathInvalidatedCache` primitive and go straight through `invoke`, so a
 * return visit to the destination re-ran the whole walk from scratch.
 *
 * Routing it through the primitive gives the destination three things the
 * raw `invoke` did not:
 *
 * - **One fetch per mesh, not one per visit.** The Probe's speculative read
 *   and the destination's own read now land on the same cache entry instead
 *   of racing each other.
 * - **Git-driven invalidation.** `GIT_CHANGED` evicts the entry, so the
 *   destination refreshes when the repository actually changes.
 * - **A rate window.** `minRefetchIntervalMs` collapses a burst of
 *   `GIT_CHANGED` events into a single trailing refetch instead of stacking
 *   a full walk per event.
 *
 * The mesh id is the key and the mesh root is the subscription path, which is
 * the dual-key shape `useMeshHealth` already uses for the health block of the
 * same destination.
 */
export const gitPruneInfoClient = createDualKeyCache<number, GitRepoPruneInfo[]>({
  fetcher: getGitPruneInfo,
  name: 'gitPruneInfo',
  // The walk is expensive enough that a burst of git events must not queue a
  // walk per event. One trailing refetch after the window closes is enough.
  minRefetchIntervalMs: 5_000,
});

/**
 * The Probe loader for the Repository destination.
 *
 * Serves from the shared cache when it can, so the destination's own read
 * and this speculative read are the same fetch. A rejection resolves to
 * `null` here — the destination reads the real error from the cache's
 * per-key error slot, so a failure is not silently reported as "no
 * repositories".
 */
export async function readGitPruneInfo({
  meshId,
}: ProbeLoadContext): Promise<GitRepoPruneInfo[] | null> {
  if (meshId == null) return null;
  const cached = gitPruneInfoClient.read(meshId);
  if (cached !== undefined) return cached;
  return gitPruneInfoClient.refresh(meshId);
}
