import { useEffect } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { createPathInvalidatedCache } from '../lib/pathInvalidatedCache';
import { usePathInvalidatedQuery } from './usePathInvalidatedQuery';
import { getMeshHealth, type MeshHealth } from '../lib/tauri';

// Module-level shared client (keyed by meshId). The sidebar `!` badge and
// the `BranchesWorktreesSection` health block both read from this hook;
// they share one cache + one GIT_CHANGED subscription through the primitive.
const healthClient = createPathInvalidatedCache<number, MeshHealth>({
  fetcher: getMeshHealth,
  name: 'useMeshHealth',
});

/**
 * Tracks the Mesh Git-health snapshot (issue #231) for a given mesh.
 *
 * The snapshot includes drift detection (HEAD not on the Base Ref's branch),
 * the base-branch hostage (a worktree holding the Base Ref's branch checked
 * out), unpushed-ahead count, and the dirty working-tree flag. The sidebar
 * `!` badge and the `BranchesWorktreesSection` health block both read from
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
  const { data, refresh } = usePathInvalidatedQuery(healthClient, meshId, meshPath);

  // Refetch when the window regains focus. The primitive already covers
  // GIT_CHANGED (file-watcher driven), so this is the only hook-specific
  // subscription left in this file.
  useEffect(() => {
    if (meshId == null) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        if (focused) refresh();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      });
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, [meshId, refresh]);

  return { health: data, refresh };
}
