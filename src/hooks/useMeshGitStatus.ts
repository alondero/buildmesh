import { useState } from 'react';
import {
  getMeshGitStatic,
  type GitStatus,
} from '../lib/tauri';
import { usePathInvalidatedQuery } from './usePathInvalidatedQuery';
import { useAsyncEffect } from './useAsyncEffect';
import { gitStatusClient } from './useChangedFiles';

export interface MeshGitStatus {
  files: GitStatus[];
  isGitRepo: boolean;
  isAuthenticated: boolean;
  defaultBranch: string;
  loading: boolean;
  refresh: () => void;
}

// The variable part of the snapshot (the file list) is the only thing that
// changes as the agent edits files. The shared `gitStatusClient` handles dedupe
// + cache + GIT_CHANGED invalidation for it (and shares one fetch with
// ChangedFilesSection / FileTree); the static part (repo-ness, auth, default
// branch) is fetched once per path via a separate effect below.

export function useMeshGitStatus(meshPath: string | null): MeshGitStatus | null {
  const [isGitRepo, setIsGitRepo] = useState<boolean | null>(null);
  const [isAuthenticated, setIsAuthenticated] = useState<boolean>(false);
  const [defaultBranch, setDefaultBranch] = useState<string>('main');

  // The primitive still subscribes for GIT_CHANGED invalidation, but the
  // initial fetch is gated by the static check below — the file list is
  // only useful once we know the path is a git repo. The `enabled: false`
  // option skips the mount-refetch so the static effect is the single
  // caller of the initial `refresh()`.
  const { data: files, loading, refresh } = usePathInvalidatedQuery(
    gitStatusClient,
    meshPath,
    meshPath,
    { enabled: false },
  );

  // Static snapshot (issue #348): one IPC replaces the three-way
  // `Promise.all` of `check_is_git_repo` + `check_gh_auth` +
  // `get_default_branch`. The backend's `get_mesh_git_static` composes
  // the trio atomically — partial state (e.g. `is_gh_authenticated`
  // updated but the path is not a repo) is no longer observable in the
  // UI. The GitHub network round-trip still only fires once per mesh
  // switch: the effect is keyed on `meshPath`, not on file-change events.
  useAsyncEffect((signal) => {
    if (!meshPath) return;
    // Issue #343: when `meshPath` changes within the same mount (e.g. user
    // switches meshes in the sidebar), the effect re-runs but the local
    // `useState` values from the previous mesh linger until the new fetch
    // resolves. Reset all three to their initial values up-front so the
    // panel returns to its "loading" shape for a beat (mirroring a fresh
    // mount) and downstream consumers never see stale auth/branch info.
    // `isGitRepo → null` also re-engages the `if (isGitRepo === null)
    // return null` early-return at the bottom of the hook, so the panel
    // disappears until the new static check lands.
    setIsGitRepo(null);
    setIsAuthenticated(false);
    setDefaultBranch('main');
    (async () => {
      try {
        const snap = await getMeshGitStatic(meshPath);
        if (signal.aborted) return;
        if (!snap.is_git_repo) {
          setIsGitRepo(false);
          return;
        }
        setIsGitRepo(true);
        setIsAuthenticated(snap.is_gh_authenticated);
        setDefaultBranch(snap.default_branch);
        // Static check passed — kick off the file-list fetch. The
        // primitive's subscribe effect is already wired, so any
        // GIT_CHANGED after this point will refetch via the bus.
        refresh();
      } catch {
        if (!signal.aborted) setIsGitRepo(false);
      }
    })();
  }, [meshPath, refresh]);

  if (isGitRepo === null) return null;
  if (isGitRepo === false) return null;

  return {
    files: files ?? [],
    isGitRepo,
    isAuthenticated,
    defaultBranch,
    loading,
    refresh,
  };
}
