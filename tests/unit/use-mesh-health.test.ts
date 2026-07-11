import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { useMeshHealth } from '../../src/hooks/useMeshHealth';

// Force the helper to behave as on Windows so the slash-normalization
// and case-insensitive branches run in `pathMatchesGitEvent`, regardless
// of the host running the suite.
vi.mock('../../src/lib/platform', () => ({
  isMac: false,
  isWindows: true,
}));

// Capture the window-focus callback so we can fire a focus event on demand.
// `useMeshHealth` opts into `refetchOnFocus: true` via the underlying
// `usePathInvalidatedQuery`, so we need to assert that wiring.
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

// A minimal MeshHealth payload — only the fields the hook actually returns
// are required. Issue #404: this type is generated from Rust via ts-rs;
// tests should mock the full wire shape even if the hook doesn't read
// every field.
const HEALTH_A = {
  base_ref: 'origin/main',
  local_base_branch: 'main',
  current_branch: 'main',
  current_short_sha: 'abc1234',
  is_detached: false,
  is_dirty: false,
  unpushed_ahead: 0,
  has_upstream: true,
  is_drifted: false,
  base_branch_holder: null,
};

const HEALTH_DRIFTED = {
  ...HEALTH_A,
  is_drifted: true,
  current_branch: 'feature/x',
  unpushed_ahead: 3,
};

const MESH_PATH = 'X:\\repo';
// Worktree subdir emitted by the file_watcher for a branched worktree
// node — `pathMatchesGitEvent` recognises this as a match for `MESH_PATH`
// via the worktree-subdir rule (issue #304).
const WORKTREE_PATH = 'X:\\repo\\.claude\\worktrees\\gentle-fox';

function callsTo(cmd: string): number {
  return mockInvoke.mock.calls.filter((c) => c[0] === cmd).length;
}

beforeEach(() => {
  focusCb = null;
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(HEALTH_A);
});

// Restore the per-test `Date.now` spies (the freshness-window jumps) so a
// jump in one test can't leak into a test that relies on real time.
afterEach(() => {
  vi.restoreAllMocks();
});


describe('useMeshHealth — dual-key shape integration', () => {
  // Pins the dual-key + bus wiring: the hook must call `get_mesh_health`
  // with the meshId, mount-fetch on render, and serve the value back.
  // This is the SEVERE finding from the code review (issue #347): no
  // hook-level test exercised the dual-key shape through the React glue,
  // so the `isDualKey` discriminator was untested.
  it('fetches MeshHealth on mount, keyed by meshId', async () => {
    const { result } = renderHook(() => useMeshHealth(7, MESH_PATH));
    await waitFor(() => expect(result.current.health).toEqual(HEALTH_A));
    expect(mockInvoke).toHaveBeenCalledWith('get_mesh_health', { meshId: 7 });
  });

  // The dual-key shape: subscription is on a different `path` than the
  // `key` (meshId). A GIT_CHANGED for that path must invalidate and
  // refetch — not fire the same IPC twice (refresh dedupe).
  it('refetches (not serves stale) when GIT_CHANGED fires for the watched path', async () => {
    const { result } = renderHook(() => useMeshHealth(7, MESH_PATH));
    await waitFor(() => expect(result.current.health).toEqual(HEALTH_A));
    expect(callsTo('get_mesh_health')).toBe(1);

    // The Mesh drifted (HEAD moved to feature/x, 3 commits unpushed).
    // Jump Date.now() past the client's freshness window (see
    // minRefetchIntervalMs) so this test keeps asserting the
    // immediate-refetch path — inside the window, bus events are absorbed
    // into a trailing refetch (same pattern as use-open-pr.test.ts).
    vi.spyOn(Date, 'now').mockReturnValue(Date.now() + 10_000);
    mockInvoke.mockResolvedValue(HEALTH_DRIFTED);
    await act(async () => {
      await emit('git-changed', { path: MESH_PATH });
    });

    await waitFor(() => expect(result.current.health).toEqual(HEALTH_DRIFTED));
    expect(callsTo('get_mesh_health')).toBe(2);
  });

  // Regression for issue #304: the file_watcher emits the worktree
  // subdir path (not the mesh root) when a branched worktree node edits
  // a file. A strict `===` against `meshPath` would never match this.
  // `pathMatchesGitEvent` recognises `<meshPath>/.claude/worktrees/*`
  // as a match, so the dual-key client's `subscribeByPath(meshId, meshPath)`
  // subscription must still fire on the worktree subdir.
  it('refetches when GIT_CHANGED fires for a worktree subdir of the mesh', async () => {
    const { result } = renderHook(() => useMeshHealth(7, MESH_PATH));
    await waitFor(() => expect(result.current.health).toEqual(HEALTH_A));
    expect(callsTo('get_mesh_health')).toBe(1);

    // Jump Date.now() past the client's freshness window (see
    // minRefetchIntervalMs) so this test keeps asserting the
    // immediate-refetch path — inside the window, bus events are absorbed
    // into a trailing refetch (same pattern as use-open-pr.test.ts).
    vi.spyOn(Date, 'now').mockReturnValue(Date.now() + 10_000);
    mockInvoke.mockResolvedValue(HEALTH_DRIFTED);
    await act(async () => {
      await emit('git-changed', {
        path: WORKTREE_PATH,
        internal_path: WORKTREE_PATH,
      });
    });

    await waitFor(() => expect(callsTo('get_mesh_health')).toBe(2));
    expect(result.current.health).toEqual(HEALTH_DRIFTED);
  });

  // Pins the WSL UNC form: the backend sometimes emits the WSL guest
  // path (UNC `\\wsl$\...` form) as the event's `path` with the resolved
  // POSIX path as `internal_path`. `pathMatchesGitEvent` must recognise
  // the UNC form as a match for the subscribed mesh path.
  it('refetches when GIT_CHANGED fires with a WSL UNC path that normalises to the watched path', async () => {
    const { result } = renderHook(() => useMeshHealth(7, MESH_PATH));
    await waitFor(() => expect(result.current.health).toEqual(HEALTH_A));

    // Jump Date.now() past the client's freshness window (see
    // minRefetchIntervalMs) so this test keeps asserting the
    // immediate-refetch path — inside the window, bus events are absorbed
    // into a trailing refetch (same pattern as use-open-pr.test.ts).
    vi.spyOn(Date, 'now').mockReturnValue(Date.now() + 10_000);
    mockInvoke.mockResolvedValue(HEALTH_DRIFTED);
    await act(async () => {
      await emit('git-changed', {
        path: '\\\\wsl$\\Ubuntu\\home\\user\\repo\\.claude\\worktrees\\gentle-fox',
        internal_path: WORKTREE_PATH,
      });
    });

    await waitFor(() => expect(callsTo('get_mesh_health')).toBe(2));
  });

  // The dual-key hook + `refetchOnFocus: true` combination: when the
  // window regains focus, the hook must refetch. This is the only test
  // that exercises focus-refetch through a dual-key client — the
  // existing `use-path-invalidated-query-focus.test.ts` only covers
  // path-keyed + focus.
  it('refetches when the window regains focus (refetchOnFocus: true)', async () => {
    const { result } = renderHook(() => useMeshHealth(7, MESH_PATH));
    await waitFor(() => expect(result.current.health).toEqual(HEALTH_A));
    expect(callsTo('get_mesh_health')).toBe(1);

    // A focus listener was registered via `getCurrentWindow().onFocusChanged`.
    expect(focusCb).not.toBeNull();

    mockInvoke.mockResolvedValue(HEALTH_DRIFTED);
    await act(async () => {
      focusCb?.({ payload: true });
    });
    await waitFor(() => expect(callsTo('get_mesh_health')).toBe(2));
    expect(result.current.health).toEqual(HEALTH_DRIFTED);

    // Losing focus does NOT refetch.
    await act(async () => {
      focusCb?.({ payload: false });
    });
    await new Promise((r) => setTimeout(r, 0));
    expect(callsTo('get_mesh_health')).toBe(2);
  });

  // A GIT_CHANGED for a DIFFERENT path must NOT refetch this hook's
  // key. The dual-key bus dispatch is scoped via `clientId`; a foreign
  // event for an unrelated path skips this client entirely.
  it('does NOT refetch when GIT_CHANGED fires for an unrelated path', async () => {
    const { result } = renderHook(() => useMeshHealth(7, MESH_PATH));
    await waitFor(() => expect(result.current.health).toEqual(HEALTH_A));
    expect(callsTo('get_mesh_health')).toBe(1);

    await act(async () => {
      await emit('git-changed', { path: 'X:\\other-repo' });
    });
    await new Promise((r) => setTimeout(r, 0));
    expect(callsTo('get_mesh_health')).toBe(1);
    expect(result.current.health).toEqual(HEALTH_A);
  });

  // Pins the dual-key + null path short-circuit: a caller can pass
  // `null` as the path to disable the GIT_CHANGED subscription while
  // still serving the meshId. The hook must NOT crash and must still
  // serve the cached value on remount.
  it('passing null as path disables the GIT_CHANGED subscription but still serves the cache', async () => {
    const { result, rerender } = renderHook(
      ({ id, p }: { id: number | null; p: string | null }) => useMeshHealth(id, p),
      { initialProps: { id: 7 as number | null, p: MESH_PATH } },
    );
    await waitFor(() => expect(result.current.health).toEqual(HEALTH_A));

    // Re-mount with path: null — the hook should not crash and should
    // not subscribe. The cache for meshId=7 is still populated, so
    // a focused read returns the cached value.
    rerender({ id: 7, p: null });
    await new Promise((r) => setTimeout(r, 0));

    // A GIT_CHANGED for the mesh path must NOT trigger a refetch
    // (the path subscription is disabled).
    await act(async () => {
      await emit('git-changed', { path: MESH_PATH });
    });
    await new Promise((r) => setTimeout(r, 0));
    expect(callsTo('get_mesh_health')).toBe(1);
  });

  // Sanity check that the `refresh()` handle is exposed (matches the
  // other 5 hooks' public shape).
  it('exposes a refresh() handle that re-invokes the IPC', async () => {
    const { result } = renderHook(() => useMeshHealth(7, MESH_PATH));
    await waitFor(() => expect(result.current.health).toEqual(HEALTH_A));
    expect(callsTo('get_mesh_health')).toBe(1);

    mockInvoke.mockResolvedValue(HEALTH_DRIFTED);
    await act(async () => {
      result.current.refresh();
    });
    await waitFor(() => expect(callsTo('get_mesh_health')).toBe(2));
    expect(result.current.health).toEqual(HEALTH_DRIFTED);
  });
});
