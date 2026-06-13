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
});
