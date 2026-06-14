import { createPathInvalidatedCache } from '../lib/pathInvalidatedCache';
import { usePathInvalidatedQuery } from './usePathInvalidatedQuery';
import { getGitStatus, type GitStatus } from '../lib/tauri';

/**
 * Shared client for `getGitStatus(path)`, keyed by repo path. There is exactly
 * ONE instance so every git-status consumer — `useMeshGitStatus` (the panel's
 * repo check), `ChangedFilesSection` (the uncommitted list), and `FileTree`
 * (the badges) — dedupes the fetch and shares one GIT_CHANGED subscription
 * through the primitive. Two panels rendering the same Mesh no longer hit
 * `get_git_status` twice.
 */
export const gitStatusClient = createPathInvalidatedCache<string, GitStatus[]>({
  fetcher: getGitStatus,
  name: 'gitStatus',
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
    rootPath,
  );
  return { files: data ?? [], loading };
}
