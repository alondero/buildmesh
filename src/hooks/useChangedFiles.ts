import { PROBE_TAB_DEFINITIONS } from '../lib/probeContext';
import { createPathKeyedCache } from '../lib/pathInvalidatedCache';
import { usePathInvalidatedQuery } from './usePathInvalidatedQuery';
import { type GitStatus } from '../lib/tauri';

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
  fetcher: activePath => PROBE_TAB_DEFINITIONS.files.load({ meshId: null, activePath }, new AbortController().signal),
  name: 'gitStatus',
  // Full `git status` walk shared by three consumers. Bound the
  // GIT_CHANGED-driven refetch rate while an agent streams edits (the
  // backend coalescer already caps events at ~2/s; this caps the walks at
  // one per 2s). The primitive's trailing refetch lands the settled state.
  minRefetchIntervalMs: 2_000,
});

/**
 * Outcome of one repository-status observation, in the terms the Git
 * surfaces need (issue #2328). Kept as a flat record rather than a union
 * so a consumer that only cares about the rows can read `files` alone.
 */
export interface ChangedFilesResult {
  /**
   * Last successful uncommitted file list for this path, or `null` when no
   * successful observation exists yet. Deliberately NOT defaulted to `[]`:
   * an empty array is the "clean repo" answer, and this issue exists
   * because that answer was being rendered for a fetch that never
   * succeeded. `files === null` means "nothing is known", which the Git
   * surfaces label as unavailable instead of clean.
   */
  files: GitStatus[] | null;
  /** A fetch is in flight. Pairs with `files` to express "refreshing". */
  loading: boolean;
  /** Most recent failure for this path, or `null`. Cleared by any success. */
  error: Error | null;
  /**
   * `true` when the last fetch failed but an earlier successful list is
   * still being shown. The rows are real but may be out of date, so the
   * surface must say so rather than presenting them as current.
   */
  stale: boolean;
  /** Retry. Drives an immediate refetch of this path. */
  refresh: () => void;
}

/**
 * The repo's uncommitted file list for `rootPath`, kept fresh on GIT_CHANGED
 * via the shared `gitStatusClient`. Pass `null` to disable (non-git context or
 * before the path is known). Replaces the hand-rolled `getGitStatus` +
 * `cancelled`-flag fetch that `ChangedFilesSection` and `FileTree` each carried
 * (neither of which refreshed on GIT_CHANGED — they went stale until remount).
 *
 * Issue #2328 widened the contract so every consumer can tell a clean repo
 * from a broken one. The shared primitive already tracked the error and the
 * retry; this hook stopped discarding both, and stopped collapsing "no result
 * yet" into an empty list.
 */
export function useChangedFiles(rootPath: string | null): ChangedFilesResult {
  const { data, loading, error, refresh } = usePathInvalidatedQuery(
    gitStatusClient,
    rootPath,
  );
  return { files: data, loading, error, stale: error !== null && data !== null, refresh };
}
