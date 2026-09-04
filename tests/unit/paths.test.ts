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

import { getEffectiveWorktreeDir, getNodeGitPath, pathMatchesGitEvent } from '../../src/lib/paths';

describe('getNodeGitPath', () => {
  it('returns worktree path when worktree_name is set', () => {
    const node = { path: '/Users/adam/myproject', worktree_name: 'gentle-fox' };
    expect(getNodeGitPath(node)).toBe('/Users/adam/myproject/.claude/worktrees/gentle-fox');
  });

  it('prefers persisted worktree_path for configured directories (issue #1519)', () => {
    const node = {
      path: '/repo/mesh',
      worktree_name: 'my-node',
      use_worktree: true,
      worktree_path: '/repo/mesh/custom/my-node',
    };
    expect(getNodeGitPath(node)).toBe('/repo/mesh/custom/my-node');
  });

  it('falls back to legacy layout when worktree_path is blank', () => {
    const node = {
      path: '/repo/mesh',
      worktree_name: 'my-node',
      use_worktree: true,
      worktree_path: '   ',
    };
    expect(getNodeGitPath(node)).toBe('/repo/mesh/.claude/worktrees/my-node');
  });

  it('ignores stored worktree_path for Root Nodes', () => {
    const node = {
      path: '/repo/mesh',
      worktree_name: 'my-node',
      use_worktree: false,
      worktree_path: '/repo/mesh/custom/my-node',
    };
    expect(getNodeGitPath(node)).toBe('/repo/mesh');
  });

  it('returns node.path when worktree_name is undefined', () => {
    const node = { path: '/Users/adam/myproject' };
    expect(getNodeGitPath(node)).toBe('/Users/adam/myproject');
  });

  it('returns node.path when worktree_name is empty string', () => {
    const node = { path: '/Users/adam/myproject', worktree_name: '' };
    expect(getNodeGitPath(node)).toBe('/Users/adam/myproject');
  });

  it('returns node.path when worktree_name is whitespace-only', () => {
    // Paired with env::worktree_segment in src-tauri/src/env/mod.rs — a
    // whitespace-only name trims to empty, which collapses to the no-worktree
    // branch (Node Working Directory = Mesh root). See issue #387.
    const node = { path: '/Users/adam/myproject', worktree_name: '   ' };
    expect(getNodeGitPath(node)).toBe('/Users/adam/myproject');
  });

  it('trims a padded worktree_name to match the canonical env::worktree_segment rule', () => {
    // The GIT_CHANGED `internal_path` Rust emits (file_watcher::node_internal_path)
    // and the path this helper returns must be byte-identical — a divergent
    // `internal_path` never matches the frontend subscription, so the node's
    // changed-files goes stale. The canonical rule (env::worktree_segment) trims;
    // this helper must agree. See issue #387.
    const node = { path: '/Users/adam/myproject', worktree_name: '  gentle-fox  ' };
    expect(getNodeGitPath(node)).toBe('/Users/adam/myproject/.claude/worktrees/gentle-fox');
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

describe('getEffectiveWorktreeDir (issue #1519)', () => {
  it('defaults to .claude/worktrees when unconfigured', () => {
    expect(getEffectiveWorktreeDir('/repo/mesh', null, null)).toBe('/repo/mesh/.claude/worktrees');
    expect(getEffectiveWorktreeDir('/repo/mesh', '  ', '')).toBe('/repo/mesh/.claude/worktrees');
  });

  it('applies a relative app setting to inheriting meshes', () => {
    expect(getEffectiveWorktreeDir('/repo/mesh', null, 'custom-wt')).toBe('/repo/mesh/custom-wt');
  });

  it('prefers the Mesh override and restores inheritance when cleared', () => {
    expect(getEffectiveWorktreeDir('/repo/mesh', 'mesh-wt', 'app-wt')).toBe('/repo/mesh/mesh-wt');
    expect(getEffectiveWorktreeDir('/repo/mesh', '  ', 'app-wt')).toBe('/repo/mesh/app-wt');
  });

  it('uses absolute values verbatim without shell expansion', () => {
    expect(getEffectiveWorktreeDir('/repo/mesh', '/tmp/wt', null)).toBe('/tmp/wt');
    expect(getEffectiveWorktreeDir('/repo/mesh', '~/wt', null)).toBe('/repo/mesh/~/wt');
  });

  it('never doubles separators when the mesh root has a trailing slash', () => {
    expect(getEffectiveWorktreeDir('/repo/mesh/', null, null)).toBe('/repo/mesh/.claude/worktrees');
    expect(getEffectiveWorktreeDir('/repo/mesh/', null, 'custom-wt')).toBe('/repo/mesh/custom-wt');
  });
});

describe('pathMatchesGitEvent with custom dirs (issue #1519)', () => {
  it('matches a mesh root when the event is under the effective dir', () => {
    const event = { path: '/repo/mesh/custom/my-node', internal_path: '/repo/mesh/custom/my-node' };
    expect(pathMatchesGitEvent(event, '/repo/mesh', ['/repo/mesh/custom'])).toBe(true);
  });

  it('matches an absolute effective dir outside the mesh root', () => {
    const event = { path: '/tmp/wt/my-node', internal_path: '/tmp/wt/my-node' };
    expect(pathMatchesGitEvent(event, '/repo/mesh', ['/tmp/wt'])).toBe(true);
  });

  it('still rejects unrelated paths even with effective dirs', () => {
    const event = { path: '/other/wt/my-node' };
    expect(pathMatchesGitEvent(event, '/repo/mesh', ['/repo/mesh/custom'])).toBe(false);
  });
});
