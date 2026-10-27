import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { useMeshHealth } from '../../src/hooks/useMeshHealth';
import { resetPathInvalidatedCacheForTests } from '../../src/lib/pathInvalidatedCache';
import { MESH_FOLDER_MISSING, isMeshFolderMissing } from '../../src/lib/meshFolder';

// Force the helper to behave as on Windows so the slash-normalization
// branches in `pathMatchesGitEvent` run regardless of the host running the
// suite.
vi.mock('../../src/lib/platform', () => ({
  isMac: false,
  isWindows: true,
}));

// Capture the window-focus callback so we can fire a focus event on demand.
// `useMeshHealth` opts into `refetchOnFocus: true`.
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

const MESH_PATH = 'X:\\repo';
// The worktree subdir the file_watcher emits for a branched worktree node.
// This is the event that used to keep a mesh with a deleted folder asking:
// the container survives, so its siblings keep firing.
const WORKTREE_PATH = 'X:\\repo\\.claude\\worktrees\\gentle-fox';

const HEALTH = {
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
  pool_fill_failure: null,
};

function callsTo(cmd: string): number {
  return mockInvoke.mock.calls.filter((c) => c[0] === cmd).length;
}

beforeEach(() => {
  focusCb = null;
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(HEALTH);
  // The health client is module-level and shared, so the stop recorded by one
  // test would otherwise leak into the next.
  resetPathInvalidatedCacheForTests();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('the mesh-folder-missing marker', () => {
  // The marker is an IPC contract with `src-tauri/src/services/mesh_folder.rs`.
  // Both halves are pinned so a rename cannot silently break the other side.
  it('matches the Rust-side constant', () => {
    expect(MESH_FOLDER_MISSING).toBe('MESH_FOLDER_MISSING:');
  });

  it('recognises the marker on an Error and on a raw IPC string', () => {
    expect(isMeshFolderMissing(new Error(`${MESH_FOLDER_MISSING} no working repository`))).toBe(true);
    expect(isMeshFolderMissing(`${MESH_FOLDER_MISSING} no working repository`)).toBe(true);
  });

  it('leaves other failures alone — they might fix themselves', () => {
    expect(isMeshFolderMissing(new Error('database is locked'))).toBe(false);
    expect(isMeshFolderMissing('could not find repository')).toBe(false);
    expect(isMeshFolderMissing(null)).toBe(false);
    expect(isMeshFolderMissing(undefined)).toBe(false);
  });
});

describe('a mesh whose folder no longer exists', () => {
  // Issue #2602: the frontend used to keep asking — a `git-changed` event for
  // a sibling worktree, a window focus, a remount — and each ask cost a
  // failing IPC round-trip and a pair of log lines. About 140 failures per
  // mesh in five hours.
  it('produces one failure per refresh instead of a continuous stream', async () => {
    mockInvoke.mockRejectedValue(`${MESH_FOLDER_MISSING} no working repository`);

    const { result } = renderHook(() => useMeshHealth(7, MESH_PATH));
    await waitFor(() => expect(callsTo('get_mesh_health')).toBe(1));

    // A sibling worktree changes: the container survived, so the watcher
    // keeps firing. Each of these used to be a fresh failing call.
    for (let i = 0; i < 3; i++) {
      await act(async () => {
        await emit('git-changed', { path: WORKTREE_PATH, internal_path: WORKTREE_PATH });
      });
    }
    await new Promise((r) => setTimeout(r, 0));

    // The window regains focus — another refetch the user did not ask for.
    await act(async () => {
      focusCb?.({ payload: true });
    });
    await new Promise((r) => setTimeout(r, 0));

    expect(callsTo('get_mesh_health')).toBe(
      1,
      'a mesh whose folder is gone must not be refetched on the app’s own initiative',
    );
  });

  it('still refetches when the user asks for it', async () => {
    mockInvoke.mockRejectedValue(`${MESH_FOLDER_MISSING} no working repository`);

    const { result } = renderHook(() => useMeshHealth(7, MESH_PATH));
    await waitFor(() => expect(callsTo('get_mesh_health')).toBe(1));

    await act(async () => {
      result.current.refresh();
    });
    await waitFor(() => expect(callsTo('get_mesh_health')).toBe(2));

    // And the second failure stops the stream again.
    await act(async () => {
      await emit('git-changed', { path: WORKTREE_PATH, internal_path: WORKTREE_PATH });
    });
    await new Promise((r) => setTimeout(r, 0));
    expect(callsTo('get_mesh_health')).toBe(2);
  });

  it('keeps refetching a mesh whose folder is there but whose read failed', async () => {
    // A failure that might fix itself must keep the surface live — the stop
    // is for folders that are gone, not for transient errors.
    mockInvoke.mockRejectedValue('database is locked');

    renderHook(() => useMeshHealth(7, MESH_PATH));
    await waitFor(() => expect(callsTo('get_mesh_health')).toBe(1));

    await act(async () => {
      await emit('git-changed', { path: WORKTREE_PATH, internal_path: WORKTREE_PATH });
    });
    await waitFor(() => expect(callsTo('get_mesh_health')).toBe(2));
  });

  it('resumes on its own once the folder is back', async () => {
    mockInvoke.mockRejectedValueOnce(`${MESH_FOLDER_MISSING} no working repository`);
    mockInvoke.mockResolvedValue(HEALTH);

    const { result } = renderHook(() => useMeshHealth(7, MESH_PATH));
    await waitFor(() => expect(callsTo('get_mesh_health')).toBe(1));

    // The user refreshes; the folder is back, so the read succeeds.
    await act(async () => {
      result.current.refresh();
    });
    await waitFor(() => expect(result.current.health).toEqual(HEALTH));

    // The stop is gone: the mesh is live again.
    await act(async () => {
      await emit('git-changed', { path: WORKTREE_PATH, internal_path: WORKTREE_PATH });
    });
    await waitFor(() => expect(callsTo('get_mesh_health')).toBe(3));
  });
});
