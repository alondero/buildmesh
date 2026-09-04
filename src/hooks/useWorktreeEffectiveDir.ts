import { useState } from 'react';
import { getWorktreeDirectoryConfig } from '../lib/tauri';
import { useAsyncEffect } from './useAsyncEffect';

/**
 * Effective worktree container dir for a Mesh (issue #1519).
 *
 * Mesh-root `GIT_CHANGED` subscribers (e.g. `useMeshHealth`) match events
 * under the legacy `.claude/worktrees/` prefix by default; a mesh with a
 * custom directory (relative or absolute-outside-the-root) needs its
 * effective container as an extra match dir, or edits in its worktrees
 * never invalidate the mesh-level caches. This hook resolves it once per
 * `meshId` via the backend-authoritative `get_worktree_directory_config`
 * (Mesh override → app default → `.claude/worktrees`) so callers never
 * re-spell the precedence rule.
 *
 * Returns `null` while loading, when `meshId` is null, or when the fetch
 * fails (callers treat null as "no extras" — legacy matching only).
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

  return effective;
}
