import { useEffect, useState, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import {
  checkIsGitRepo,
  getGitStatus,
  getDefaultBranch,
  checkGhAuth,
  type GitStatus,
} from '../lib/tauri';
import { GIT_CHANGED } from '../lib/events';
import { pathMatchesGitEvent } from '../lib/paths';

export interface MeshGitStatus {
  files: GitStatus[];
  isGitRepo: boolean;
  isAuthenticated: boolean;
  defaultBranch: string;
  loading: boolean;
  refresh: () => void;
}

export function useMeshGitStatus(meshPath: string | null): MeshGitStatus | null {
  const [isGitRepo, setIsGitRepo] = useState<boolean | null>(null);
  const [files, setFiles] = useState<GitStatus[]>([]);
  const [isAuthenticated, setIsAuthenticated] = useState<boolean>(false);
  const [defaultBranch, setDefaultBranch] = useState<string>('main');
  const [loading, setLoading] = useState(false);

  // The changed-file list is the only part that varies as the agent edits
  // files; refetch just that on GIT_CHANGED / manual refresh.
  const fetchFiles = useCallback(async (path: string) => {
    setLoading(true);
    try {
      setFiles(await getGitStatus(path));
    } catch {
      setIsGitRepo(false);
    } finally {
      setLoading(false);
    }
  }, []);

  // Repo-ness, GitHub auth, and the default branch are stable per panel
  // session — fetch once per path. Re-running checkGhAuth (a GitHub network
  // round-trip) on every file save was pure waste.
  const fetchStatic = useCallback(async (path: string) => {
    setLoading(true);
    try {
      const [repoOk, ghOk, branchResult] = await Promise.all([
        checkIsGitRepo(path),
        checkGhAuth(),
        getDefaultBranch(path),
      ]);

      if (!repoOk) {
        setIsGitRepo(false);
        return;
      }

      setIsGitRepo(true);
      setIsAuthenticated(ghOk);
      setDefaultBranch(branchResult);

      await fetchFiles(path);
    } catch {
      setIsGitRepo(false);
    } finally {
      setLoading(false);
    }
  }, [fetchFiles]);

  const refresh = useCallback(() => {
    if (!meshPath) return;
    fetchFiles(meshPath);
  }, [meshPath, fetchFiles]);

  // Fetch on mount
  useEffect(() => {
    if (!meshPath) return;
    fetchStatic(meshPath);
  }, [meshPath, fetchStatic]);

  // Listen for git change events on this mesh path
  useEffect(() => {
    if (!meshPath) return;

    const unlisten = listen<{ path: string; internal_path?: string }>(
      GIT_CHANGED,
      (event) => {
        // Issue #304: the watcher emits the worktree subdir path, not the
        // mesh root, so a strict `===` against `meshPath` never matches.
        // The helper also normalizes back/forward-slash and case
        // differences for the WSL UNC path case.
        if (pathMatchesGitEvent(event.payload, meshPath)) {
          refresh();
        }
      }
    );

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [meshPath, refresh]);

  if (isGitRepo === null && loading) return null;
  if (isGitRepo === false) return null;

  return { files, isGitRepo: isGitRepo ?? false, isAuthenticated, defaultBranch, loading, refresh };
}