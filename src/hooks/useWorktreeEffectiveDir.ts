import { useEffect, useState } from 'react';
import { getWorktreeDirectoryConfig } from '../lib/tauri';
import {
  concernsMesh,
  subscribeWorktreeDirectoryChanges,
} from '../lib/streams/worktreeDirectoryStream';
import { useAsyncEffect } from './useAsyncEffect';

/**
 * Effective worktree container dir for a Mesh (issue #1519).
 *
 * Mesh-root `GIT_CHANGED` subscribers (e.g. `useMeshHealth`) match events
 * under `.worktrees/` and legacy `.claude/worktrees/` by default; a mesh with a
 * custom directory (relative or absolute-outside-the-root) needs its
 * effective container as an extra match dir, or edits in its worktrees
 * never invalidate the mesh-level caches. This hook resolves it once per
 * `meshId` via the backend-authoritative `get_worktree_directory_config`
 * (Mesh override → app default → `.worktrees`) so callers never
 * re-spell the precedence rule.
 *
 * Reactive: re-resolves on the backend's `worktree-directory-changed`
 * event when the payload is `null` (app default moved — every inheriting
 * mesh may have moved) or matches this `meshId`. Without this, a Settings
 * change elsewhere would leave stale extras until restart, and file
 * changes under the new directory would silently skip invalidation.
 *
 * Returns `null` while loading, when `meshId` is null, or when the fetch
 * fails (callers treat null as "no extras" — built-in layouts still match).
 */
export function useWorktreeEffectiveDir(meshId: number | null): string | null {
  const [effective, setEffective] = useState<string | null>(null);

  useAsyncEffect(
    (signal) => {
      if (meshId === null) {
        setEffective(null);
        return;
      }
      getWorktreeDirectoryConfig(meshId)
        .then((cfg) => {
          if (!signal.aborted) setEffective(cfg.effective_directory);
        })
        .catch(() => {
          if (!signal.aborted) setEffective(null);
        });
    },
    [meshId],
  );

  useEffect(() => {
    if (meshId === null) return;
    let cancelled = false;
    const release = subscribeWorktreeDirectoryChanges((change) => {
      if (!concernsMesh(change, meshId)) return;
      getWorktreeDirectoryConfig(meshId)
        .then((cfg) => {
          if (!cancelled) setEffective(cfg.effective_directory);
        })
        .catch(() => {
          if (!cancelled) setEffective(null);
        });
    });
    return () => {
      cancelled = true;
      release();
    };
  }, [meshId]);

  return effective;
}
