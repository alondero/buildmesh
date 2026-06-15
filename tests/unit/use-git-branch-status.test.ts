import { describe, it, expect, beforeEach, vi } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import type { useGitBranchStatus as UseGitBranchStatus } from '../../src/hooks/useGitBranchStatus';

// Force the helper to behave as on Windows so the slash-normalization and
// case-insensitive branches run in `pathMatchesGitEvent`, regardless of the
// host running the suite.
vi.mock('../../src/lib/platform', () => ({
  isMac: false,
  isWindows: true,
}));

// Capture the window-focus callback so we can fire a focus event on demand
// (the shared setup mock resolves to a no-op and never invokes the callback).
// The hook opts into `refetchOnFocus: true`, so we need to assert that wiring.
let focusCb: ((e: { payload: boolean }) => void) | null = null;
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({
    onFocusChanged: (cb: (e: { payload: boolean }) => void) => {
      focusCb = cb;
      return Promise.resolve(() => {
        focusCb = null;
      });
    },
  }),
}));

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

const STATUS_A = { name: 'main', ahead: 0, behind: 2, short_sha: 'abc1234' };
const STATUS_B = { name: 'feature/x', ahead: 1, behind: 0, short_sha: 'def5678' };

const MESH_PATH = 'X:\\repo';
// Worktree subdir that the file_watcher emits when a branched worktree node
// edits a file. Mirrors what the backend produces for a worktree named
// `gentle-fox` on the mesh above. Issue #304 — `pathMatchesGitEvent` must
// recognize this as a match for `MESH_PATH` (via the worktree-subdir rule).
const WORKTREE_PATH = 'X:\\repo\\.claude\\worktrees\\gentle-fox';

function callsTo(cmd: string): number {
  return mockInvoke.mock.calls.filter(c => c[0] === cmd).length;
}

// The hook keeps a module-level cache + a once-only GIT_CHANGED listener.
// Re-import a fresh module per test so cache state can't leak between tests
// (the global setup clears the mock event listeners each test, which would
// otherwise strand the already-installed listener — issue #354).
let useGitBranchStatus: typeof UseGitBranchStatus;

beforeEach(async () => {
  vi.resetModules();
  focusCb = null;
  ({ useGitBranchStatus } = await import('../../src/hooks/useGitBranchStatus'));
  mockInvoke.mockReset();
});

describe('useGitBranchStatus', () => {
  it('fetches the branch status on mount', async () => {
    mockInvoke.mockResolvedValue(STATUS_A);

    const { result } = renderHook(() => useGitBranchStatus(MESH_PATH));

    await waitFor(() => expect(result.current.branchStatus).toEqual(STATUS_A));
    expect(mockInvoke).toHaveBeenCalledWith('get_git_branch_status', { path: MESH_PATH });
  });

  it('refetches (not serves the stale cache) when GIT_CHANGED fires for the mesh path', async () => {
    mockInvoke.mockResolvedValue(STATUS_A);
    const { result } = renderHook(() => useGitBranchStatus(MESH_PATH));
    await waitFor(() => expect(result.current.branchStatus).toEqual(STATUS_A));

    // The user ran `git fetch` in an external terminal — the next fetch
    // returns the up-to-date behind count.
    mockInvoke.mockResolvedValue(STATUS_B);
    await act(async () => {
      await emit('git-changed', { path: MESH_PATH });
    });

    await waitFor(() => expect(result.current.branchStatus).toEqual(STATUS_B));
  });

  it('serves the cache (no second IPC call) when a second subscriber mounts for the same path', async () => {
    // The MeshItem sidebar list and any future second view of the same mesh
    // share one cache + one subscription through the primitive's client.
    mockInvoke.mockResolvedValue(STATUS_A);
    const first = renderHook(() => useGitBranchStatus(MESH_PATH));
    await waitFor(() => expect(first.result.current.branchStatus).toEqual(STATUS_A));

    const callsBefore = mockInvoke.mock.calls.length;
    const second = renderHook(() => useGitBranchStatus(MESH_PATH));
    await waitFor(() => expect(second.result.current.branchStatus).toEqual(STATUS_A));
    expect(mockInvoke.mock.calls.length).toBe(callsBefore);
  });

  // Regression for issue #304: the file_watcher emits the worktree subdir
  // (not the mesh root) when a branched worktree node edits a file. A
  // strict `===` against `meshPath` would never match this. The shared
  // `pathMatchesGitEvent` helper recognizes `<meshPath>/.claude/worktrees/*`
  // as a match.
  it('refetches when GIT_CHANGED fires for a worktree subdir of the mesh', async () => {
    mockInvoke.mockResolvedValue(STATUS_A);
    const { result } = renderHook(() => useGitBranchStatus(MESH_PATH));
    await waitFor(() => expect(result.current.branchStatus).toEqual(STATUS_A));

    mockInvoke.mockResolvedValue(STATUS_B);
    await act(async () => {
      await emit('git-changed', {
        path: WORKTREE_PATH,
        internal_path: WORKTREE_PATH,
      });
    });

    await waitFor(() => expect(result.current.branchStatus).toEqual(STATUS_B));
  });

  // Regression for issue #304: the WSL `host_path` produces a `\\wsl$\...`
  // UNC form that the worktree subdir lives under after the env::to_host_path
  // conversion. The shared `pathMatchesGitEvent` helper normalizes backslashes
  // and recognizes the worktree subdir as a match.
  it('refetches when GIT_CHANGED fires for a WSL UNC form of a worktree subdir', async () => {
    mockInvoke.mockResolvedValue(STATUS_A);
    const { result } = renderHook(() => useGitBranchStatus(MESH_PATH));
    await waitFor(() => expect(result.current.branchStatus).toEqual(STATUS_A));

    mockInvoke.mockResolvedValue(STATUS_B);
    await act(async () => {
      await emit('git-changed', {
        // UNC form the WSL host_path produces.
        path: '\\\\wsl$\\Ubuntu\\home\\user\\repo\\.claude\\worktrees\\gentle-fox',
        internal_path: WORKTREE_PATH,
      });
    });

    await waitFor(() => expect(result.current.branchStatus).toEqual(STATUS_B));
  });

  it('does NOT refetch when GIT_CHANGED fires for an unrelated path', async () => {
    mockInvoke.mockResolvedValue(STATUS_A);
    const { result } = renderHook(() => useGitBranchStatus(MESH_PATH));
    await waitFor(() => expect(result.current.branchStatus).toEqual(STATUS_A));
    const callsBefore = mockInvoke.mock.calls.length;

    await act(async () => {
      await emit('git-changed', { path: 'X:\\some-other-repo' });
    });

    // Drain the bus dispatch (mock listen + notify) before counting calls.
    await new Promise(r => setTimeout(r, 0));
    expect(mockInvoke.mock.calls.length).toBe(callsBefore);
    expect(result.current.branchStatus).toEqual(STATUS_A);
  });

  // `MeshItem.handleSync` calls `refreshBranchStatus()` after a successful
  // `gitSync` so the behind count re-computes against the newly-pulled
  // HEAD. The hook must expose a `refresh` handle that re-fetches.
  it('exposes refresh() which re-fetches the current path', async () => {
    mockInvoke.mockResolvedValue(STATUS_A);
    const { result } = renderHook(() => useGitBranchStatus(MESH_PATH));
    await waitFor(() => expect(result.current.branchStatus).toEqual(STATUS_A));
    const callsBefore = callsTo('get_git_branch_status');

    mockInvoke.mockResolvedValue(STATUS_B);
    await act(async () => {
      result.current.refresh();
    });

    await waitFor(() => expect(result.current.branchStatus).toEqual(STATUS_B));
    expect(callsTo('get_git_branch_status')).toBe(callsBefore + 1);
  });

  it('refetches when the window regains focus (refetchOnFocus opt-in)', async () => {
    let calls = 0;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_git_branch_status') {
        calls += 1;
        return Promise.resolve({ ...STATUS_A, behind: calls - 1 });
      }
      return Promise.resolve(undefined);
    });

    const { result } = renderHook(() => useGitBranchStatus(MESH_PATH));
    await waitFor(() => expect(result.current.branchStatus?.behind).toBe(0));
    expect(focusCb).not.toBeNull();

    // The user may have run `git fetch` in an external terminal while
    // the app was unfocused — refetch on focus gain.
    await act(async () => {
      focusCb?.({ payload: true });
    });
    await waitFor(() => expect(result.current.branchStatus?.behind).toBe(1));

    // Losing focus does NOT refetch.
    await act(async () => {
      focusCb?.({ payload: false });
    });
    await new Promise(r => setTimeout(r, 0));
    expect(calls).toBe(2);
  });

  it('returns null branchStatus and skips fetching when path is null', async () => {
    mockInvoke.mockResolvedValue(STATUS_A);

    const { result } = renderHook(() => useGitBranchStatus(null));

    // Let the async effect settle — no fetch should have fired, no
    // listener should have been installed.
    await new Promise(r => setTimeout(r, 0));
    expect(result.current.branchStatus).toBeNull();
    expect(callsTo('get_git_branch_status')).toBe(0);
    expect(focusCb).toBeNull();
  });

  it('switches the cached value (no second fetch for the same key) when path changes', async () => {
    // Two meshes back-to-back — the second mounts with a different path,
    // so the primitive's cache has a separate entry. The first mount
    // already cached MESH_PATH, so re-rendering with it serves from cache
    // (no new IPC). The OTHER_PATH mount triggers a fresh fetch.
    const OTHER_PATH = 'X:\\other-repo';
    const OTHER_STATUS = { name: 'develop', ahead: 0, behind: 0, short_sha: 'fff9999' };
    mockInvoke.mockImplementation((cmd: string, args?: { path: string }) => {
      if (cmd !== 'get_git_branch_status') return Promise.resolve(undefined);
      return Promise.resolve(args?.path === OTHER_PATH ? OTHER_STATUS : STATUS_A);
    });

    const { result, rerender } = renderHook(
      ({ path }: { path: string | null }) => useGitBranchStatus(path),
      { initialProps: { path: MESH_PATH } },
    );
    await waitFor(() => expect(result.current.branchStatus).toEqual(STATUS_A));
    const callsAfterFirst = callsTo('get_git_branch_status');

    // Re-render with the same path → cache hit, no fetch, value preserved.
    rerender({ path: MESH_PATH });
    await new Promise(r => setTimeout(r, 0));
    expect(result.current.branchStatus).toEqual(STATUS_A);
    expect(callsTo('get_git_branch_status')).toBe(callsAfterFirst);

    // Re-render with a different path → fresh fetch, new value.
    rerender({ path: OTHER_PATH });
    await waitFor(() => expect(result.current.branchStatus).toEqual(OTHER_STATUS));
    expect(callsTo('get_git_branch_status')).toBe(callsAfterFirst + 1);
  });
});
