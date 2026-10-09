/**
 * Blocked worktree cleanup alerts (issue #2139).
 *
 * The old `worktree-cleanup-failed` listener rendered one sentence with the
 * node name in it and nothing else. These tests pin that the alert names the
 * node, the full path, the failed step and the OS error, and that the
 * actionable dialog is actually opened.
 */

import { describe, it, expect, beforeEach, vi } from 'vitest';
import {
  blockedCleanupToastMessage,
  reportBlockedWorktreeCleanup,
} from '../../src/lib/worktreeCleanupAlerts';
import { useToastStore } from '../../src/stores/toastStore';
import type { WorktreeCleanupFailedPayload } from '../../src/types/generated/WorktreeCleanupFailedPayload';

vi.mock('../../src/stores/blockedCleanupStore', () => ({
  openBlockedCleanups: vi.fn(),
}));

const PAYLOAD: WorktreeCleanupFailedPayload = {
  node_name: 'agent-node',
  worktree_path: 'C:/repo/.claude/worktrees/agent-node',
  error: 'The process cannot access the file because it is being used by another process. (os error 32)',
  operation: 'rename-worktree-to-staging',
  attempt_count: 2,
  last_attempt_at: 1_700_000_000_000,
  retry_not_before: 1_700_000_300_000,
};

describe('blocked worktree cleanup alerts (#2139)', () => {
  beforeEach(() => {
    useToastStore.setState({ toasts: [] });
    vi.clearAllMocks();
  });

  it('names the node, the folder, the failed step and the OS error', () => {
    const message = blockedCleanupToastMessage(PAYLOAD);

    expect(message).toContain('agent-node');
    expect(message).toContain('C:/repo/.claude/worktrees/agent-node');
    // The step is the human-readable label, not the raw id the backend sends.
    expect(message).toContain('Moving the folder aside to remove it failed');
    expect(message).toContain('os error 32');
  });

  it('warns once and opens the blocked-cleanup dialog', async () => {
    await reportBlockedWorktreeCleanup(PAYLOAD);

    const toasts = useToastStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0].provider).toBe('Worktree');
    expect(toasts[0].severity).toBe('warning');
    expect(toasts[0].message).toContain('C:/repo/.claude/worktrees/agent-node');

    const { openBlockedCleanups } = await import('../../src/stores/blockedCleanupStore');
    expect(openBlockedCleanups).toHaveBeenCalledTimes(1);
  });
});
