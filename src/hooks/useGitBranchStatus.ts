import { useEffect, useState, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { getGitBranchStatus, type GitBranchStatus } from '../lib/tauri';
import { GIT_CHANGED } from '../lib/events';

/**
 * Tracks the current Git branch (name, ahead/behind, short SHA) of an arbitrary
 * repository path — mesh root, agent worktree, etc.
 *
 * Refetches on mount, on window focus (the user may have pulled in a terminal
 * while away), and on GIT_CHANGED events for this path (file-watcher driven).
 * Returns null branch status for non-git paths or before the first fetch.
 */
export function useGitBranchStatus(gitPath: string | null): {
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
    if (gitPath) fetch(gitPath);
  }, [gitPath, fetch]);

  // Fetch on mount / path change
  useEffect(() => {
    if (!gitPath) {
      setBranchStatus(null);
      return;
    }
    fetch(gitPath);
  }, [gitPath, fetch]);

  // Refetch when the file watcher reports a change for this path
  useEffect(() => {
    if (!gitPath) return;
    const unlisten = listen<{ path: string; internal_path?: string }>(
      GIT_CHANGED,
      (event) => {
        if (event.payload.path === gitPath || event.payload.internal_path === gitPath) {
          refresh();
        }
      },
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [gitPath, refresh]);

  // Refetch when the window regains focus
  useEffect(() => {
    if (!gitPath) return;
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
  }, [gitPath, refresh]);

  return { branchStatus, refresh };
}
