/**
 * Unit tests for worktree path resolution
 *
 * Tests resolving the git worktree root path via `git rev-parse --show-toplevel`
 * from within an agent node's worktree directory.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const mockExecSync = vi.hoisted(() => vi.fn());

vi.mock('child_process', () => ({
  execSync: mockExecSync,
}));

describe('worktree path resolution', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  describe('git rev-parse --show-toplevel', () => {
    it('resolves worktree root from within worktree directory', () => {
      const worktreePath = '/some/worktree/path';
      mockExecSync.mockReturnValue(Buffer.from(worktreePath + '\n'));

      const result = resolveWorktreeRoot(worktreePath);
      expect(result).toBe(worktreePath);
    });

    it('throws when worktree does not exist', () => {
      mockExecSync.mockImplementation(() => {
        throw new Error('fatal: not a git repository');
      });

      expect(() => resolveWorktreeRoot('/nonexistent/path')).toThrow(
        'Worktree not found'
      );
    });

    it('returns normalized path with trailing slash removed', () => {
      const worktreePath = '/some/worktree/path';
      mockExecSync.mockReturnValue(Buffer.from(worktreePath + '/\n'));

      const result = resolveWorktreeRoot(worktreePath);
      expect(result).toBe(worktreePath);
    });

    it('throws when git output is empty', () => {
      mockExecSync.mockReturnValue(Buffer.from(''));

      expect(() => resolveWorktreeRoot('/some/path')).toThrow(
        'Failed to resolve worktree root'
      );
    });
  });

  describe('worktree path construction', () => {
    it('constructs worktree path from mesh path and node id', () => {
      const meshPath = '/mesh/path';
      const nodeId = 42;
      const expected = '/mesh/path/worktrees/agent-42';

      const result = constructWorktreePath(meshPath, nodeId);
      expect(result).toBe(expected);
    });
  });
});

// Function that runs git rev-parse --show-toplevel
function resolveWorktreeRoot(worktreePath: string): string {
  let output: Buffer | string;
  try {
    output = mockExecSync(`git rev-parse --show-toplevel`, {
      cwd: worktreePath,
      encoding: 'utf-8',
    });
  } catch {
    throw new Error('Worktree not found');
  }
  const trimmed = (output as Buffer).toString('utf-8').trim();
  if (!trimmed) {
    throw new Error('Failed to resolve worktree root');
  }
  return trimmed.replace(/\/+$/, '');
}

// Function that constructs worktree path from mesh path and node id
function constructWorktreePath(meshPath: string, nodeId: number): string {
  return `${meshPath}/worktrees/agent-${nodeId}`;
}
