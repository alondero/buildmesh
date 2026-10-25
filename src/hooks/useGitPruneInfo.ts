import { useMemo } from 'react';
import { gitPruneInfoClient } from '../lib/gitPruneInfo';
import { usePathInvalidatedQuery } from './usePathInvalidatedQuery';
import type { GitRepoPruneInfo } from '../types/generated/GitRepoPruneInfo';

/**
 * The Repository destination's read of the project's branches and worktrees.
 *
 * Backed by the shared `pathInvalidatedCache` client, so the read is cached
 * per mesh, invalidated by `GIT_CHANGED`, and rate-limited. The destination
 * used to re-run the whole walk on a return visit (issue #2580).
 */
export interface UseGitPruneInfoResult {
  /** The repositories of the current mesh, with their branches and worktrees. */
  repos: GitRepoPruneInfo[];
  /** True while the read for this mesh is in flight. */
  loading: boolean;
  /** The most recent read failure, if the read failed. */
  error: Error | null;
  /** Re-read the mesh's repository state. */
  refresh: () => void;
  /**
   * True when the backend gave up classifying branches before it ran out of
   * its wall-clock budget. Those branches carry an *unknown* merge status,
   * and the destination says so rather than rendering a silently missing
   * "merged" badge (issue #2580).
   */
  mergeScanTruncated: boolean;
}

export function useGitPruneInfo(
  meshId: number | null,
  meshPath: string | null,
): UseGitPruneInfoResult {
  const { data, loading, error, refresh } = usePathInvalidatedQuery(
    gitPruneInfoClient,
    meshId,
    meshPath,
  );
  const repos = useMemo(() => data ?? [], [data]);
  const mergeScanTruncated = useMemo(
    () => repos.some(repo => repo.merge_scan_truncated),
    [repos],
  );
  return { repos, loading, error, refresh, mergeScanTruncated };
}
