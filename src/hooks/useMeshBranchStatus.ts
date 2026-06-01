import { useEffect, useState, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { getGitBranchStatus, type GitBranchStatus } from '../lib/tauri';
import { GIT_CHANGED } from '../lib/events';

/**
 * Tracks how far a mesh's branch is ahead/behind its upstream.
 *
 * Refetches on mount, on window focus (the user may have pulled in a terminal
 * while away), and on GIT_CHANGED events for this path (file-watcher driven).
 * Returns null branch status for non-git paths or before the first fetch.
 */
export function useMeshBranchStatus(meshPath: string | null): {
  branchStatus: GitBranchStatus | null;
  refresh: () => void;
} {
  const [branchStatus, setBranchStatus] = useState<GitBranchStatus | null>(null);

  const fetch = useCallback(async (path: string) => {
    try {
      setBranchStatus(await getGitBranchStatus(path));
    } catch {
      setBranchStatus(null);
    }
  }, []);

  const refresh = useCallback(() => {
    if (meshPath) fetch(meshPath);
  }, [meshPath, fetch]);

  // Fetch on mount / path change
  useEffect(() => {
    if (!meshPath) {
      setBranchStatus(null);
      return;
    }
    fetch(meshPath);
  }, [meshPath, fetch]);

  // Refetch when the file watcher reports a change for this path
  useEffect(() => {
    if (!meshPath) return;
    const unlisten = listen<{ path: string; internal_path?: string }>(
      GIT_CHANGED,
      (event) => {
        if (event.payload.path === meshPath || event.payload.internal_path === meshPath) {
          refresh();
        }
      },
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [meshPath, refresh]);

  // Refetch when the window regains focus
  useEffect(() => {
    if (!meshPath) return;
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
  }, [meshPath, refresh]);

  return { branchStatus, refresh };
}
