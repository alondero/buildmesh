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
 * `createPathKeyedCache` is the right factory here: the IPC is
 * `getGitStatus(path: string)`, so the key IS the path — there's no entity
 * id separate from the subscription path. Issue #347 split this single-key
 * shape out from the old single `createPathInvalidatedCache` factory.
 */
export const gitStatusClient = createPathKeyedCache<GitStatus[]>({
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
  );
  return { files: data ?? [], loading };
}
