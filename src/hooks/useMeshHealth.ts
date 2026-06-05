import { useEffect, useState, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { getMeshHealth, type MeshHealth } from '../lib/tauri';
import { GIT_CHANGED } from '../lib/events';

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
 * path or any of its worktrees, and when the window regains focus.
 *
 * `meshId`/`meshPath` may be null during early render so the hook can
 * mount before the mesh is known.
 */
export function useMeshHealth(
  meshId: number | null,
  meshPath: string | null,
): { health: MeshHealth | null; refresh: () => void } {
  const [health, setHealth] = useState<MeshHealth | null>(null);

  const fetch = useCallback(async (id: number) => {
    try {
      setHealth(await getMeshHealth(id));
    } catch {
      setHealth(null);
    }
  }, []);

  const refresh = useCallback(() => {
    if (meshId != null) fetch(meshId);
  }, [meshId, fetch]);

  // Fetch on mount / meshId change
  useEffect(() => {
    if (meshId == null) {
      setHealth(null);
      return;
    }
    fetch(meshId);
  }, [meshId, fetch]);

  // Refetch on GIT_CHANGED for this mesh path (file-watcher driven).
  // The mesh health depends on the root's HEAD ref and the worktree set;
  // any git-changed event for the root path or any of the worktree paths
  // could invalidate the snapshot.
  useEffect(() => {
    if (meshId == null || meshPath == null) return;
    const unlisten = listen<{ path: string; internal_path?: string }>(
      GIT_CHANGED,
      (event) => {
        const changed = event.payload.path === meshPath;
        const internalMatches = event.payload.internal_path === meshPath;
        if (changed || internalMatches) refresh();
      },
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [meshId, meshPath, refresh]);

  // Refetch when the window regains focus (the user may have run `git
  // checkout` in a terminal while away).
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

  return { health, refresh };
}
