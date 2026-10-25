import { describe, it, expect, beforeEach, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { useGitPruneInfo } from '../../src/hooks/useGitPruneInfo';

// Force the helper to behave as on Windows so the path normalisation in
// `pathMatchesGitEvent` runs the same way regardless of the host running the
// suite (same reason as `use-mesh-health.test.ts`).
vi.mock('../../src/lib/platform', () => ({
  isMac: false,
  isWindows: true,
}));

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

const MESH_PATH = 'X:\\repo';

// The prune payload mirrors the generated `GitRepoPruneInfo` wire shape.
// `merge_scan_truncated` is the flag the backend raises when its
// wall-clock budget stopped the squash-merge scan (issue #2580).
const BRANCH = {
  name: 'main',
  is_head: true,
  is_merged_into_main: true,
  is_orphan: false,
  is_active: false,
  checked_out_in_worktree: null,
  has_uncommitted: false,
  last_commit_date: null,
  ahead: 0,
  behind: 0,
};

const PRUNE_OK = [
  {
    path: MESH_PATH,
    local_branches: [BRANCH],
    worktrees: [],
    remote_tracking_branches: [],
    merge_scan_truncated: false,
  },
];

const PRUNE_TRUNCATED = [
  {
    ...PRUNE_OK[0],
    local_branches: [{ ...BRANCH, name: 'feature/x', is_head: false }],
    merge_scan_truncated: true,
  },
];

function callsTo(cmd: string): number {
  return mockInvoke.mock.calls.filter((c) => c[0] === cmd).length;
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(PRUNE_OK);
});

describe('useGitPruneInfo — the Repository destination read', () => {
  // The read goes through the shared git cache, so it is keyed by mesh id
  // and the GIT_CHANGED subscription is scoped to the mesh root (issue
  // #2580).
  it('reads the prune info on mount, keyed by meshId', async () => {
    const { result } = renderHook(() => useGitPruneInfo(101, MESH_PATH));

    await waitFor(() => expect(result.current.repos).toEqual(PRUNE_OK));
    expect(mockInvoke).toHaveBeenCalledWith('get_git_prune_info', { meshId: 101 });
  });

  // The regression the whole change exists for: the destination used to
  // re-run the whole branch/worktree walk on a return visit, which on a repo
  // with many branches over a long history kept it spinning for tens of
  // minutes. A second visit must be served from the cache.
  it('serves a repeat visit from the cache instead of re-reading', async () => {
    const first = renderHook(() => useGitPruneInfo(102, MESH_PATH));
    await waitFor(() => expect(first.result.current.repos).toEqual(PRUNE_OK));
    expect(callsTo('get_git_prune_info')).toBe(1);

    first.unmount();

    const second = renderHook(() => useGitPruneInfo(102, MESH_PATH));
    await waitFor(() => expect(second.result.current.repos).toEqual(PRUNE_OK));

    expect(callsTo('get_git_prune_info')).toBe(1);
  });

  // A truncated merge scan must reach the UI. The branches the backend did
  // not classify carry an unknown merge status, and the destination has to
  // be able to say so — a silently missing "merged" badge reads as "not
  // merged", which is what decides what the user deletes.
  it('surfaces a truncated merge scan', async () => {
    mockInvoke.mockResolvedValue(PRUNE_TRUNCATED);

    const { result } = renderHook(() => useGitPruneInfo(103, MESH_PATH));

    await waitFor(() => expect(result.current.mergeScanTruncated).toBe(true));
  });

  // A failed read must surface as an error, not as an empty repository list.
  // The cache keeps the last successful value, so a transient failure after
  // a good read shows both: the old rows and the error.
  it('surfaces a failed read as an error rather than an empty list', async () => {
    mockInvoke.mockRejectedValue(new Error('git exploded'));

    const { result } = renderHook(() => useGitPruneInfo(104, MESH_PATH));

    await waitFor(() => expect(result.current.error).toBeInstanceOf(Error));
    expect(result.current.repos).toEqual([]);
  });

  // The imperative refresh must re-read, bypassing the cache — the
  // destination calls it after a delete or a restore.
  it('exposes a refresh() handle that re-invokes the read', async () => {
    const { result } = renderHook(() => useGitPruneInfo(105, MESH_PATH));
    await waitFor(() => expect(result.current.repos).toEqual(PRUNE_OK));
    expect(callsTo('get_git_prune_info')).toBe(1);

    mockInvoke.mockResolvedValue(PRUNE_TRUNCATED);
    await act(async () => {
      result.current.refresh();
    });

    await waitFor(() => expect(callsTo('get_git_prune_info')).toBe(2));
    await waitFor(() => expect(result.current.mergeScanTruncated).toBe(true));
  });
});
