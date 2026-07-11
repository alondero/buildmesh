import { createPathKeyedCache } from '../lib/pathInvalidatedCache';
import { usePathInvalidatedQuery } from './usePathInvalidatedQuery';
import { getGitStatus, type GitStatus } from '../lib/tauri';

/**
 * Shared client for `getGitStatus(path)`, keyed by repo path. There is exactly
 * ONE instance so every git-status consumer — `useMeshGitStatus` (the panel's
 * repo check), `ChangedFilesSection` (the uncommitted list), and `FileTree`
 * (the badges) — dedupes the fetch and shares one GIT_CHANGED subscription
 * through the primitive. Two panels rendering the same Mesh no longer hit
 * `get_git_status` twice.
 *
 * Uses `createPathKeyedCache` (issue #347's split) — the key IS the path
 * the bus matches, so the single-key factory is the right fit.
 */
export const gitStatusClient = createPathKeyedCache<GitStatus[]>({
  fetcher: getGitStatus,
  name: 'gitStatus',
  // Full `git status` walk shared by three consumers. Bound the
  // GIT_CHANGED-driven refetch rate while an agent streams edits (the
  // backend coalescer already caps events at ~2/s; this caps the walks at
  // one per 2s). The primitive's trailing refetch lands the settled state.
  minRefetchIntervalMs: 2_000,
});

/**
 * The repo's uncommitted file list for `rootPath`, kept fresh on GIT_CHANGED
 * via the shared `gitStatusClient`. Pass `null` to disable (non-git context or
 * before the path is known). Replaces the hand-rolled `getGitStatus` +
 * `cancelled`-flag fetch that `ChangedFilesSection` and `FileTree` each carried
 * (neither of which refreshed on GIT_CHANGED — they went stale until remount).
 */
export function useChangedFiles(rootPath: string | null): {
  files: GitStatus[];
  loading: boolean;
} {
  const { data, loading } = usePathInvalidatedQuery(
    gitStatusClient,
    rootPath,
  );
  return { files: data ?? [], loading };
}
