import { describe, it, expect, beforeEach, vi } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import type { useOpenPr as UseOpenPr } from '../../src/hooks/useOpenPr';

// Force the helper to behave as on Windows so the slash-normalization
// and case-insensitive branches run, regardless of the host running the
// suite.
vi.mock('../../src/lib/platform', () => ({
  isMac: false,
  isWindows: true,
}));

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

const PR_A = { number: 11, url: 'https://github.com/o/r/pull/11', title: 'first', draft: false };
const PR_B = { number: 12, url: 'https://github.com/o/r/pull/12', title: 'second', draft: true };

const GIT_PATH = 'X:\\repo\\.claude\\worktrees\\node';

// The hook keeps a module-level cache and a once-only GIT_CHANGED listener.
// Re-import a fresh module per test so cache state can't leak between tests
// (the global setup clears the mock event listeners each test, which would
// otherwise strand the already-installed listener).
let useOpenPr: typeof UseOpenPr;

beforeEach(async () => {
  vi.resetModules();
  ({ useOpenPr } = await import('../../src/hooks/useOpenPr'));
  mockInvoke.mockReset();
});

describe('useOpenPr', () => {
  it('fetches the open PR for a node on mount', async () => {
    mockInvoke.mockResolvedValue(PR_A);

    const { result } = renderHook(() => useOpenPr(7, GIT_PATH));

    await waitFor(() => expect(result.current.pr).toEqual(PR_A));
    expect(mockInvoke).toHaveBeenCalledWith('get_open_pr_for_node', { nodeId: 7 });
  });

  it('refetches (not serves the stale cache) when GIT_CHANGED fires for the node path', async () => {
    mockInvoke.mockResolvedValue(PR_A);
    const { result } = renderHook(() => useOpenPr(7, GIT_PATH));
    await waitFor(() => expect(result.current.pr).toEqual(PR_A));

    // The PR changed on GitHub (e.g. opened/retitled); the next fetch returns B.
    mockInvoke.mockResolvedValue(PR_B);
    // Jump past the client's minRefetchIntervalMs freshness window (60s) —
    // inside it, bus events intentionally serve the cache (rate-limit guard).
    const nowSpy = vi.spyOn(Date, 'now').mockReturnValue(Date.now() + 61_000);
    await act(async () => {
      await emit('git-changed', { path: GIT_PATH });
    });

    await waitFor(() => expect(result.current.pr).toEqual(PR_B));
    nowSpy.mockRestore();
  });

  it('serves the cache (no refetch) for GIT_CHANGED inside the 60s freshness window', async () => {
    mockInvoke.mockResolvedValue(PR_A);
    const { result } = renderHook(() => useOpenPr(7, GIT_PATH));
    await waitFor(() => expect(result.current.pr).toEqual(PR_A));

    // Every agent file-write fires GIT_CHANGED; within the window the hook
    // must NOT burn a GitHub API call per event.
    const callsBefore = mockInvoke.mock.calls.length;
    mockInvoke.mockResolvedValue(PR_B);
    await act(async () => {
      await emit('git-changed', { path: GIT_PATH });
    });

    expect(mockInvoke.mock.calls.length).toBe(callsBefore);
    expect(result.current.pr).toEqual(PR_A);
  });

  it('serves the cache (no second IPC call) when a second subscriber mounts', async () => {
    mockInvoke.mockResolvedValue(PR_A);
    const first = renderHook(() => useOpenPr(7, GIT_PATH));
    await waitFor(() => expect(first.result.current.pr).toEqual(PR_A));

    const callsBefore = mockInvoke.mock.calls.length;
    const second = renderHook(() => useOpenPr(7, GIT_PATH));
    await waitFor(() => expect(second.result.current.pr).toEqual(PR_A));
    expect(mockInvoke.mock.calls.length).toBe(callsBefore);
  });

  // Regression for issue #304: the helper-based listener iterates all
  // subscribers and asks `pathMatchesGitEvent` for each, so a future
  // GIT_CHANGED event shape (e.g. worktree-subdir events the file_watcher
  // doesn't emit today) won't silently strand the cache invalidation.
  // The hook-level contract — that a refetch happens for a matching event
  // — is already covered by the second test above; the slash/case
  // normalization is pinned by `tests/unit/paths.test.ts`. Keeping this
  // test as a smoke check for the listener's wiring.
  it('refetches when GIT_CHANGED fires for an event whose internal_path matches the worktree path', async () => {
    mockInvoke.mockResolvedValue(PR_A);
    const { result } = renderHook(() => useOpenPr(7, GIT_PATH));
    await waitFor(() => expect(result.current.pr).toEqual(PR_A));

    mockInvoke.mockResolvedValue(PR_B);
    // Past the freshness window so the bus event reaches the subscriber.
    const nowSpy = vi.spyOn(Date, 'now').mockReturnValue(Date.now() + 61_000);
    await act(async () => {
      await emit('git-changed', {
        // UNC form the WSL host_path produces — should be normalized away
        // by pathMatchesGitEvent.
        path: '\\\\wsl$\\Ubuntu\\home\\user\\repo\\.claude\\worktrees\\gentle-fox',
        internal_path: GIT_PATH,
      });
    });

    await waitFor(() => expect(result.current.pr).toEqual(PR_B));
    nowSpy.mockRestore();
  });
});
