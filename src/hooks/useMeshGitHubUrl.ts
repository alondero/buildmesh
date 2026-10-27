/**
 * `useMeshGitHubUrl` — per-mesh dual-key cached resolver for a mesh's
 * `origin` remote, returning the `https://github.com/{owner}/{repo}`
 * web URL or `null` when the origin isn't GitHub / no origin is set.
 * `null` is an expected value (the consumers conditionally render
 * around it; `SafeLink` falls back to an inert `<span>`).
 *
 * Modeled on `useOpenPr` (dual-key cache, refetches on `GIT_CHANGED`
 * for the subscribed path). N `MeshItem`s + the Issues / PRs probes
 * for the same meshId dedupe on a single IPC via the module-level
 * client; the dual-key split keeps the GIT_CHANGED subscription
 * path-derived without abandoning the meshId-keyed cache.
 */

import { createDualKeyCache } from '../lib/pathInvalidatedCache';
import { usePathInvalidatedQuery } from './usePathInvalidatedQuery';
import { getGitHubUrlForMesh } from '../lib/tauri';
import { isMeshFolderMissing } from '../lib/meshFolder';

const urlClient = createDualKeyCache<number, string | null>({
  fetcher: getGitHubUrlForMesh,
  name: 'useMeshGitHubUrl',
  // Issue #2602: a mesh whose folder was deleted fails the origin reads.
  // The mesh's worktree container usually survives, so its sibling
  // worktrees keep firing GIT_CHANGED — each one used to be a fresh
  // failing IPC call. Stop refetching until the mesh is re-pointed or the
  // user asks for a refresh.
  stopOnError: isMeshFolderMissing,
});

/**
 * `null` short-circuit on `meshId === null` (and `meshPath === null`
 * for the GIT_CHANGED subscription) is handled by the underlying
 * query primitive — no fetch, no subscription, `url` stays `null`.
 */
export function useMeshGitHubUrl(meshId: number | null, meshPath: string | null): {
  url: string | null;
  loading: boolean;
  refresh: () => void;
} {
  const { data, loading, refresh } = usePathInvalidatedQuery(
    urlClient,
    meshId,
    meshPath,
  );
  // Coerce non-string returns to `null` at the hook boundary. The
  // Rust contract is `Result<Option<String>, String>`, so production
  // data is always a string-or-null; a non-string arriving here is
  // either a test stub (e.g. `mockResolvedValue({})` for an
  // unhandled command in `mockBackend()`) or a future refactor
  // mistake. Either way, `null` is the right "no URL" sentinel — the
  // `SafeLink` fallback (renders inert `<span>`) and the menu's
  // conditional render both key on it.
  const url = typeof data === 'string' ? data : null;
  return { url, loading, refresh };
}
