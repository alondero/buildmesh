import { createPathKeyedCache } from '../lib/pathInvalidatedCache';
import { usePathInvalidatedQuery } from './usePathInvalidatedQuery';
import { getGitBranchStatus, type GitBranchStatus } from '../lib/tauri';

// Module-level shared client, keyed by repo path. `MeshItem` (mesh root) and
// the 🌳 Worktree Manager tab (node path) both read through this hook, so
// two views of the same path dedupe onto one fetch + one GIT_CHANGED
// subscription.
const branchStatusClient = createPathKeyedCache<GitBranchStatus>({
  fetcher: getGitBranchStatus,
  name: 'gitBranchStatus',
});

/**
 * Tracks the current Git branch (name, ahead/behind, short SHA) of an arbitrary
 * repository path — mesh root, agent worktree, etc.
 *
 * Refetches on mount, on GIT_CHANGED for this path (file-watcher driven), and
 * on window focus (the user may have pulled in a terminal while away) — all via
 * the path-invalidated cache primitive, which also handles the worktree-subdir +
 * WUNC path matching and in-flight dedupe. Returns null branch status for
 * non-git paths or before the first fetch.
 */
export function useGitBranchStatus(gitPath: string | null): {
  branchStatus: GitBranchStatus | null;
  refresh: () => void;
} {
  const { data, refresh } = usePathInvalidatedQuery(
    branchStatusClient,
    gitPath,
    { refetchOnFocus: true },
  );
  return { branchStatus: data, refresh };
}
