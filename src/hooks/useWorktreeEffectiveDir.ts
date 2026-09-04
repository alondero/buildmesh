import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getWorktreeDirectoryConfig } from '../lib/tauri';
import { WORKTREE_DIR_CHANGED_EVENT } from '../lib/events';
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
 * Reactive: re-resolves on the backend's `worktree-directory-changed`
 * event when the payload is `null` (app default moved — every inheriting
 * mesh may have moved) or matches this `meshId`. Without this, a Settings
 * change elsewhere would leave stale extras until restart, and file
 * changes under the new directory would silently skip invalidation.
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

  useEffect(() => {
    if (meshId === null) return;
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    listen<number | null>(WORKTREE_DIR_CHANGED_EVENT, (event) => {
      const affected = event.payload;
      if (affected !== null && affected !== undefined && affected !== meshId) return;
      getWorktreeDirectoryConfig(meshId)
        .then((cfg) => {
          if (!cancelled) setEffective(cfg.effective_directory);
        })
        .catch(() => {
          if (!cancelled) setEffective(null);
        });
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [meshId]);

  return effective;
}
