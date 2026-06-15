import { describe, it, expect, beforeEach, vi } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { useMeshGitStatus } from '../../src/hooks/useMeshGitStatus';

// Force the helper to behave as on Windows so the slash-normalization and
// case-insensitive branches run, regardless of the host running the suite.
vi.mock('../../src/lib/platform', () => ({
  isMac: false,
  isWindows: true,
}));

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

const MESH_PATH = 'X:\\repo';
// Worktree subdir that the file_watcher emits when a branched worktree
// node edits a file. Mirrors what the backend produces for a worktree
// named `gentle-fox` on the mesh above.
const WORKTREE_PATH = 'X:\\repo\\.claude\\worktrees\\gentle-fox';

function callsTo(cmd: string): number {
  return mockInvoke.mock.calls.filter(c => c[0] === cmd).length;
}

// Default snapshot returned by `get_mesh_git_static` (issue #348):
// repo=true, auth=true, default_branch='main'. Mirrors the previous
// trio's defaults so existing tests' "happy path" assertions still pass
// without re-wiring every case.
const DEFAULT_SNAPSHOT = {
  is_git_repo: true,
  is_gh_authenticated: true,
  default_branch: 'main',
};

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockImplementation((cmd: string) => {
    switch (cmd) {
      case 'get_mesh_git_static': return Promise.resolve(DEFAULT_SNAPSHOT);
      case 'get_git_status': return Promise.resolve([]);
      default: return Promise.resolve(undefined);
    }
  });
  // The primitive is reset between tests by the global vitest setup
  // (tests/setup/vitest.setup.ts) — see the rationale comment there for
  // why "the primitive is process-wide" + "setup clears mockListeners"
  // requires a per-test reset.
});

describe('useMeshGitStatus', () => {
  it('fetches repo state, auth, branch, and files on mount', async () => {
    const { result } = renderHook(() => useMeshGitStatus(MESH_PATH));

    await waitFor(() => expect(result.current?.isGitRepo).toBe(true));
    expect(result.current?.isAuthenticated).toBe(true);
    expect(result.current?.defaultBranch).toBe('main');
    expect(callsTo('get_git_status')).toBe(1);
    // Issue #348: the static trio is one IPC, not three.
    expect(callsTo('get_mesh_git_static')).toBe(1);
  });

  it('refetches only the file list on GIT_CHANGED — not the static snapshot', async () => {
    const { result } = renderHook(() => useMeshGitStatus(MESH_PATH));
    await waitFor(() => expect(result.current?.isGitRepo).toBe(true));
    await waitFor(() => expect(callsTo('get_git_status')).toBe(1));

    await act(async () => {
      await emit('git-changed', { path: MESH_PATH });
    });

    await waitFor(() => expect(callsTo('get_git_status')).toBe(2));
    // The snapshot bundles the gh-auth network round-trip; it must not
    // re-run per file change.
    expect(callsTo('get_mesh_git_static')).toBe(1);
  });

  // Regression for issue #304: the file_watcher emits the worktree subdir
  // path (not the mesh root) when a branched worktree node edits a file.
  // A strict `===` against `meshPath` would never match this. The shared
  // `pathMatchesGitEvent` helper recognizes `<meshPath>/.claude/worktrees/*`
  // as a match.
  it('refetches when GIT_CHANGED fires for a worktree subdir of the mesh', async () => {
    const { result } = renderHook(() => useMeshGitStatus(MESH_PATH));
    await waitFor(() => expect(result.current?.isGitRepo).toBe(true));
    await waitFor(() => expect(callsTo('get_git_status')).toBe(1));

    await act(async () => {
      await emit('git-changed', {
        path: WORKTREE_PATH,
        internal_path: WORKTREE_PATH,
      });
    });

    await waitFor(() => expect(callsTo('get_git_status')).toBe(2));
  });

  // Regression for issue #343: when `meshPath` changes, the static-fetch
  // effect re-runs — but the prior `isGitRepo` / `isAuthenticated` /
  // `defaultBranch` values linger in `useState` until the new fetch
  // resolves. The panel briefly shows stale values from the previous mesh.
  // Fix: the effect resets all three at the top so the panel goes back to
  // its "loading" shape for a beat (mirroring a fresh mount), and any
  // caller that re-reads during the transition sees the defaults rather
  // than the previous mesh's snapshot.
  it('resets static state (isGitRepo/isAuthenticated/defaultBranch) when meshPath changes', async () => {
    // Start with the default mock: snapshot → repo=true, auth=true, branch='main'.
    // Let the first render settle so the hook is "primed" with PATH_A's
    // values.
    const OTHER_MESH = 'X:\\other-repo';
    const { result, rerender } = renderHook(
      ({ path }: { path: string }) => useMeshGitStatus(path),
      { initialProps: { path: MESH_PATH } },
    );
    await waitFor(() => expect(result.current?.isGitRepo).toBe(true));
    expect(result.current?.isAuthenticated).toBe(true);
    expect(result.current?.defaultBranch).toBe('main');

    // Now wire the single snapshot IPC to a deferred promise so we can
    // observe the in-between state. With the trio there were three
    // independent deferreds; with the snapshot there's one.
    let resolveSnapshot: (v: unknown) => void = () => {};
    const snapshotPromise = new Promise<unknown>((resolve) => {
      resolveSnapshot = resolve;
    });
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_mesh_git_static') return snapshotPromise;
      // get_git_status: shouldn't be called during this test (static
      // effect only calls `refresh()` after the snapshot resolves), but
      // return [] defensively in case a stale call slips through.
      if (cmd === 'get_git_status') return Promise.resolve([]);
      return Promise.resolve(undefined);
    });

    // Switch paths. With the fix, the effect should immediately reset
    // isGitRepo → null, isAuthenticated → false, defaultBranch → 'main',
    // making the hook return `null` (via the `isGitRepo === null` early
    // return) until the new fetch resolves. Without the fix, `result.current`
    // would still hold PATH_A's snapshot throughout the transition.
    rerender({ path: OTHER_MESH });

    // Flush the synchronous portion of the new effect (the setState calls
    // + the re-render they trigger) without awaiting the deferred
    // promise. `act` flushes microtasks once; the deferred is still
    // pending so the async IIFE inside the effect can't have completed.
    await act(async () => {
      await Promise.resolve();
    });

    expect(result.current).toBeNull();

    // Let the new snapshot resolve with DIFFERENT values from PATH_A and
    // verify the new values land (and the panel re-appears).
    resolveSnapshot({
      is_git_repo: true,
      is_gh_authenticated: false,
      default_branch: 'develop',
    });
    await waitFor(() => expect(result.current?.isAuthenticated).toBe(false));
    expect(result.current?.defaultBranch).toBe('develop');
    expect(result.current?.isGitRepo).toBe(true);
  });

  // New behaviour guard for issue #348: a non-repo path must take the
  // same "panel returns null" short-circuit the trio used to. The
  // snapshot returns `is_git_repo: false` for a non-repo path (other
  // fields are still populated, but the hook ignores them).
  it('returns null and skips the file-list fetch when the snapshot says non-repo', async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_mesh_git_static') {
        return Promise.resolve({
          is_git_repo: false,
          is_gh_authenticated: false,
          default_branch: 'main',
        });
      }
      if (cmd === 'get_git_status') return Promise.resolve([]);
      return Promise.resolve(undefined);
    });

    const { result } = renderHook(() => useMeshGitStatus(MESH_PATH));
    await waitFor(() => expect(callsTo('get_mesh_git_static')).toBe(1));
    // The static check resolved to non-repo: the hook must NOT call
    // `refresh()` and `get_git_status` should never fire.
    expect(callsTo('get_git_status')).toBe(0);
    expect(result.current).toBeNull();
  });
});
