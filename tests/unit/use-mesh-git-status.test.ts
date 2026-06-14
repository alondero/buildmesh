import { describe, it, expect, beforeEach, vi } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { useMeshGitStatus } from '../../src/hooks/useMeshGitStatus';
import { resetPathInvalidatedCacheForTests } from '../../src/lib/pathInvalidatedCache';

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

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockImplementation((cmd: string) => {
    switch (cmd) {
      case 'check_is_git_repo': return Promise.resolve(true);
      case 'check_gh_auth': return Promise.resolve(true);
      case 'get_default_branch': return Promise.resolve('main');
      case 'get_git_status': return Promise.resolve([]);
      default: return Promise.resolve(undefined);
    }
  });
  // The primitive installs ONE global `GIT_CHANGED` listener for the whole
  // process. The global vitest setup's `beforeEach` clears its mock listener
  // set between tests, so without resetting the primitive's `listenerInstalled`
  // flag the next test's `subscribe` calls would short-circuit and the
  // listener would never be re-added to the mock set. This is the test
  // counterpart of the "Test isolation pattern" note in the memory file.
  resetPathInvalidatedCacheForTests();
});

describe('useMeshGitStatus', () => {
  it('fetches repo state, auth, branch, and files on mount', async () => {
    const { result } = renderHook(() => useMeshGitStatus(MESH_PATH));

    await waitFor(() => expect(result.current?.isGitRepo).toBe(true));
    expect(result.current?.isAuthenticated).toBe(true);
    expect(result.current?.defaultBranch).toBe('main');
    expect(callsTo('get_git_status')).toBe(1);
  });

  it('refetches only the file list on GIT_CHANGED — not gh auth or default branch', async () => {
    const { result } = renderHook(() => useMeshGitStatus(MESH_PATH));
    await waitFor(() => expect(result.current?.isGitRepo).toBe(true));
    await waitFor(() => expect(callsTo('get_git_status')).toBe(1));

    await act(async () => {
      await emit('git-changed', { path: MESH_PATH });
    });

    await waitFor(() => expect(callsTo('get_git_status')).toBe(2));
    // checkGhAuth is a GitHub network round-trip; it must not re-run per file change.
    expect(callsTo('check_gh_auth')).toBe(1);
    expect(callsTo('get_default_branch')).toBe(1);
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
    // Start with the default mock: repo=true, auth=true, branch='main'.
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

    // Now wire the THREE static-fetch commands to three independent
    // deferred promises so we can control each separately and observe
    // the in-between state. Use a `Map` so we can resolve them by command.
    const deferreds = new Map<string, { resolve: (v: unknown) => void }>();
    function deferFor(cmd: string): Promise<unknown> {
      return new Promise<unknown>((resolve) => {
        deferreds.set(cmd, { resolve });
      });
    }
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'check_is_git_repo'
          || cmd === 'check_gh_auth'
          || cmd === 'get_default_branch') return deferFor(cmd);
      // get_git_status: shouldn't be called during this test (static
      // effect only calls `refresh()` after repo check resolves), but
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
    // + the re-render they trigger) without awaiting any of the deferred
    // promises. `act` flushes microtasks once; the deferreds are still
    // pending so the async IIFE inside the effect can't have completed.
    await act(async () => {
      await Promise.resolve();
    });

    expect(result.current).toBeNull();

    // Let the new fetch resolve with DIFFERENT values from PATH_A and
    // verify the new values land (and the panel re-appears).
    deferreds.get('check_is_git_repo')!.resolve(true);
    deferreds.get('check_gh_auth')!.resolve(false);
    deferreds.get('get_default_branch')!.resolve('develop');
    await waitFor(() => expect(result.current?.isAuthenticated).toBe(false));
    expect(result.current?.defaultBranch).toBe('develop');
    expect(result.current?.isGitRepo).toBe(true);
  });
});
