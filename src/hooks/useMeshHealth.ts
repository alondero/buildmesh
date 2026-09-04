import { createDualKeyCache } from '../lib/pathInvalidatedCache';
import { usePathInvalidatedQuery } from './usePathInvalidatedQuery';
import { useWorktreeEffectiveDir } from './useWorktreeEffectiveDir';
import { getMeshHealth, type MeshHealth } from '../lib/tauri';

// Module-level shared client (keyed by meshId, with a separate git path
// the GIT_CHANGED subscription matches). The sidebar `!` badge and the
// 🌳 Worktree Manager tab's HealthBlock both read from this hook; they
// share one cache + one GIT_CHANGED subscription through the primitive.
const healthClient = createDualKeyCache<number, MeshHealth>({
  fetcher: getMeshHealth,
  name: 'useMeshHealth',
  // The health snapshot walks the mesh root AND every worktree (drift,
  // hostage, dirty, ahead checks) — the most expensive GIT_CHANGED consumer
  // by far, feeding a sidebar badge that doesn't need sub-second updates.
  // 5s caps it; the trailing refetch keeps the badge converging on the
  // settled state after an edit burst.
  minRefetchIntervalMs: 5_000,
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
  // `extraPaths` (issue #1519) keeps the badge converging when the mesh uses
  // a custom worktree directory: events under the effective container
  // (relative-inside-root or absolute-outside) invalidate like legacy
  // `.claude/worktrees/` events do.
  const effectiveDir = useWorktreeEffectiveDir(meshId);
  const { data, refresh } = usePathInvalidatedQuery(healthClient, meshId, meshPath, {
    refetchOnFocus: true,
    extraPaths: effectiveDir ? [effectiveDir] : undefined,
  });

  return { health: data, refresh };
}
