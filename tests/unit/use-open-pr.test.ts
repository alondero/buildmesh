import { describe, it, expect, beforeEach, vi } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import type { useOpenPr as UseOpenPr } from '../../src/hooks/useOpenPr';

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
    await act(async () => {
      await emit('git-changed', { path: GIT_PATH });
    });

    await waitFor(() => expect(result.current.pr).toEqual(PR_B));
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
});
