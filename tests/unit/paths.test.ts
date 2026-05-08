import { describe, it, expect } from 'vitest';
import { getNodeGitPath } from '../../src/lib/paths';

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
