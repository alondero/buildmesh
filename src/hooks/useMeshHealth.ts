import { createDualKeyCache } from '../lib/pathInvalidatedCache';
import { usePathInvalidatedQuery } from './usePathInvalidatedQuery';
import { getMeshHealth, type MeshHealth } from '../lib/tauri';

// Module-level shared client (keyed by meshId, with a separate git path
// the GIT_CHANGED subscription matches). The sidebar `!` badge and the
// 🌳 Worktree Manager tab's HealthBlock both read from this hook; they
// share one cache + one GIT_CHANGED subscription through the primitive.
const healthClient = createDualKeyCache<number, MeshHealth>({
  fetcher: getMeshHealth,
  name: 'useMeshHealth',
});

/**
 * Tracks the Mesh Git-health snapshot (issue #231) for a given mesh.
 *
 * The snapshot includes drift detection (HEAD not on the Base Ref's branch),
 * the base-branch hostage (a worktree holding the Base Ref's branch checked
 * out), unpushed-ahead count, and the dirty working-tree flag. The sidebar
 * `!` badge and the `WorktreeManagerTab` health block both read from
 * this hook so they cannot disagree about the mesh's state.
 *
 * Refetches on mount, when the file watcher reports a change for the mesh
 * path or any of its worktrees (via the shared primitive), and when the
 * window regains focus (the user may have run `git checkout` in a terminal
 * while away).
 *
 * `meshId`/`meshPath` may be null during early render so the hook can
 * mount before the mesh is known.
 */
export function useMeshHealth(
  meshId: number | null,
  meshPath: string | null,
): { health: MeshHealth | null; refresh: () => void } {
  // `refetchOnFocus` covers the "user ran git checkout in a terminal while
  // away" case; GIT_CHANGED (file-watcher driven) is covered by the primitive.
  const { data, refresh } = usePathInvalidatedQuery(healthClient, meshId, meshPath, {
    refetchOnFocus: true,
  });

  return { health: data, refresh };
}
