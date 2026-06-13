import { describe, it, expect, vi } from 'vitest';

// Force `isWindows = true` so the case-insensitive branches and the
// backslash/forward-slash normalization branches all run, regardless of
// the host running the test suite. None of the test data below depends on
// case-sensitivity preservation (the would-be "case-sensitive" assertion
// would require us to mock the *opposite* — easier to test it
// deterministically by pinning the platform here).
vi.mock('../../src/lib/platform', () => ({
  isMac: false,
  isWindows: true,
}));

import { getNodeGitPath, pathMatchesGitEvent } from '../../src/lib/paths';

describe('getNodeGitPath', () => {
  it('returns worktree path when worktree_name is set', () => {
    const node = { path: '/Users/adam/myproject', worktree_name: 'gentle-fox' };
    expect(getNodeGitPath(node)).toBe('/Users/adam/myproject/.claude/worktrees/gentle-fox');
  });

  it('returns node.path when worktree_name is undefined', () => {
    const node = { path: '/Users/adam/myproject' };
    expect(getNodeGitPath(node)).toBe('/Users/adam/myproject');
  });

  it('returns node.path when worktree_name is empty string', () => {
    const node = { path: '/Users/adam/myproject', worktree_name: '' };
    expect(getNodeGitPath(node)).toBe('/Users/adam/myproject');
  });
});

// Regression coverage for issue #304: every GIT_CHANGED consumer used to do
// a strict `===` comparison between the event's path/internal_path and the
// path it cared about, so worktree edits (whose event path is the worktree
// subdir) and WSL UNC paths (whose event path uses backslashes) never
// matched. These tests pin the helper that all six consumer sites now share.
describe('pathMatchesGitEvent', () => {
  it('matches when the event path equals the watched path exactly', () => {
    const event = { path: '/home/user/repo' };
    expect(pathMatchesGitEvent(event, '/home/user/repo')).toBe(true);
  });

  it('matches when internal_path equals the watched path (Linux-side worktree case)', () => {
    // Backend file_watcher always emits both path (host) and internal_path
    // (the Linux/POSIX form that mirrors getNodeGitPath).
    const event = {
      path: 'X:\\repo\\.claude\\worktrees\\gentle-fox',
      internal_path: 'X:\\repo/.claude/worktrees/gentle-fox',
    };
    expect(pathMatchesGitEvent(event, 'X:\\repo/.claude/worktrees/gentle-fox')).toBe(true);
  });

  it('matches the mesh root when the event is for one of its worktrees', () => {
    // Consumer pattern for useMeshHealth / useMeshGitStatus: subscribed with
    // mesh.path, but the file_watcher emits the worktree subdir. The helper
    // must treat the mesh root as "this path + any .claude/worktrees/* under
    // it".
    const event = {
      path: '/home/user/repo/.claude/worktrees/gentle-fox',
      internal_path: '/home/user/repo/.claude/worktrees/gentle-fox',
    };
    expect(pathMatchesGitEvent(event, '/home/user/repo')).toBe(true);
  });

  it('matches Windows backslash paths against forward-slash watched paths', () => {
    // file_watcher emits `path = host_path` (backslashes on Windows for WSL,
    // forwards for native); the frontend gitPath uses forward slashes via
    // getNodeGitPath.
    const event = {
      path: '\\\\wsl$\\Ubuntu\\home\\user\\repo\\.claude\\worktrees\\gentle-fox',
      internal_path: '/home/user/repo/.claude/worktrees/gentle-fox',
    };
    expect(pathMatchesGitEvent(event, '/home/user/repo/.claude/worktrees/gentle-fox')).toBe(true);
  });

  it('is case-insensitive on Windows', () => {
    // Windows filesystems are case-insensitive; the helper follows the same
    // rule. macOS/Linux stay case-sensitive (the OS file lookup is the source
    // of truth there), so we don't lowercase unconditionally.
    const event = { path: 'c:\\Repo\\.claude\\worktrees\\gentle-fox' };
    expect(pathMatchesGitEvent(event, 'C:\\Repo\\.claude\\worktrees\\gentle-fox')).toBe(true);
  });

  it('tolerates a trailing separator on the watched path', () => {
    const event = { path: '/home/user/repo' };
    expect(pathMatchesGitEvent(event, '/home/user/repo/')).toBe(true);
  });

  it('does NOT match an unrelated worktree under a different mesh', () => {
    // The prefix match is scoped to `<watched>/.claude/worktrees/` — so a
    // worktree path that *is* a worktree itself does not accidentally match
    // events from a sibling worktree at a different mesh.
    const event = { path: '/other/repo/.claude/worktrees/y' };
    expect(pathMatchesGitEvent(event, '/home/user/repo/.claude/worktrees/x')).toBe(false);
  });

  it('returns false for null / undefined watched path', () => {
    const event = { path: '/home/user/repo' };
    expect(pathMatchesGitEvent(event, null)).toBe(false);
    expect(pathMatchesGitEvent(event, undefined)).toBe(false);
    expect(pathMatchesGitEvent(event, '')).toBe(false);
  });

  it('falls back to event.path when internal_path is absent', () => {
    const event = { path: '/home/user/repo' };
    expect(pathMatchesGitEvent(event, '/home/user/repo')).toBe(true);
  });

  it('does not match when neither event field relates to the watched path', () => {
    const event = {
      path: '/other/repo/.claude/worktrees/y',
      internal_path: '/other/repo/.claude/worktrees/y',
    };
    expect(pathMatchesGitEvent(event, '/home/user/repo')).toBe(false);
  });
});
