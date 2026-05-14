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

  const fetch = useCallback(async (path: string) => {
    setLoading(true);
    try {
      const [repoOk, ghOk, branchResult] = await Promise.all([
        checkIsGitRepo(path),
        checkGhAuth(),
        getDefaultBranch(path),
      ]);

      if (!repoOk) {
        setIsGitRepo(false);
        setLoading(false);
        return;
      }

      setIsGitRepo(true);
      setIsAuthenticated(ghOk);
      setDefaultBranch(branchResult);

      // Fetch git status and files
      const statusFiles = await getGitStatus(path);
      setFiles(statusFiles);
    } catch {
      setIsGitRepo(false);
    } finally {
      setLoading(false);
    }
  }, []);

  const refresh = useCallback(() => {
    if (!meshPath) return;
    fetch(meshPath);
  }, [meshPath, fetch]);

  // Fetch on mount
  useEffect(() => {
    if (!meshPath) return;
    fetch(meshPath);
  }, [meshPath, fetch]);

  // Listen for git change events on this mesh path
  useEffect(() => {
    if (!meshPath) return;

    const unlisten = listen<{ path: string; internal_path?: string }>(
      GIT_CHANGED,
      (event) => {
        const matchPath = event.payload.path === meshPath;
        const matchInternal = event.payload.internal_path === meshPath;
        if (matchPath || matchInternal) {
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