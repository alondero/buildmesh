/**
 * Blocked worktree cleanup store (issue #2139).
 *
 * The store is the frontend half of the durable cleanup queue, so the behaviour
 * worth pinning is: the backend's answer wins over any local copy, a dismissed
 * or removed entry leaves the list, the dialog closes when nothing is left, and
 * a diagnosis is never allowed to go stale.
 */

import { describe, it, expect, beforeEach, vi } from 'vitest';
import * as api from '../../src/lib/tauri';
import { useBlockedCleanupStore, openBlockedCleanups } from '../../src/stores/blockedCleanupStore';
import { useToastStore } from '../../src/stores/toastStore';
import type { PendingWorktreeRemoval } from '../../src/types/generated/PendingWorktreeRemoval';
import type { BlockingProcess } from '../../src/types/generated/BlockingProcess';

vi.mock('../../src/lib/tauri', () => ({
  listPendingWorktreeRemovals: vi.fn(),
  retryWorktreeCleanup: vi.fn(),
  dismissWorktreeCleanup: vi.fn(),
  diagnoseWorktreeCleanupBlockers: vi.fn(),
  releaseWorktreeCleanupBlocker: vi.fn(),
}));

const BLOCKED: PendingWorktreeRemoval = {
  worktree_path: 'C:/repo/.claude/worktrees/agent-node',
  node_name: 'agent-node',
  attempt_count: 2,
  last_attempt_at: 1_700_000_000_000,
  last_operation: 'rename-worktree-to-staging',
  last_error: 'being used by another process. (os error 32)',
  retry_not_before: 1_700_000_300_000,
  notified_error: 'rename-worktree-to-staging: being used by another process. (os error 32)',
};

const HOLDER: BlockingProcess = {
  pid: 5212,
  name: 'powershell.exe',
  executable_path: 'C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe',
  reason: 'working-directory',
  detail: 'C:/repo/.claude/worktrees/agent-node/child',
};

const mocked = api as unknown as {
  listPendingWorktreeRemovals: ReturnType<typeof vi.fn>;
  retryWorktreeCleanup: ReturnType<typeof vi.fn>;
  dismissWorktreeCleanup: ReturnType<typeof vi.fn>;
  diagnoseWorktreeCleanupBlockers: ReturnType<typeof vi.fn>;
  releaseWorktreeCleanupBlocker: ReturnType<typeof vi.fn>;
};

describe('blocked cleanup store (#2139)', () => {
  beforeEach(() => {
    useBlockedCleanupStore.setState({ entries: [], isOpen: false, loading: false, blockers: {}, actionStatus: {} });
    useToastStore.setState({ toasts: [] });
    mocked.listPendingWorktreeRemovals.mockReset();
    mocked.retryWorktreeCleanup.mockReset();
    mocked.dismissWorktreeCleanup.mockReset();
    mocked.diagnoseWorktreeCleanupBlockers.mockReset();
    mocked.releaseWorktreeCleanupBlocker.mockReset();
  });

  it('opens by fetching the live queue instead of trusting the event payload', async () => {
    mocked.listPendingWorktreeRemovals.mockResolvedValue([BLOCKED]);

    await useBlockedCleanupStore.getState().open();

    expect(mocked.listPendingWorktreeRemovals).toHaveBeenCalledTimes(1);
    const state = useBlockedCleanupStore.getState();
    expect(state.isOpen).toBe(true);
    expect(state.entries).toEqual([BLOCKED]);
  });

  it('stays closed when the backend reports nothing blocked', async () => {
    mocked.listPendingWorktreeRemovals.mockResolvedValue([]);

    await useBlockedCleanupStore.getState().open();

    expect(useBlockedCleanupStore.getState().isOpen).toBe(false);
  });

  it('reports a failed read instead of silently showing an empty list', async () => {
    mocked.listPendingWorktreeRemovals.mockRejectedValue(new Error('db locked'));

    await useBlockedCleanupStore.getState().open();

    expect(useBlockedCleanupStore.getState().loading).toBe(false);
    const toasts = useToastStore.getState().toasts;
    expect(toasts.some((t) => t.provider === 'Worktree' && t.message.includes('db locked'))).toBe(true);
  });

  it('replaces the entry with the backend row when a retry is still blocked', async () => {
    const updated = { ...BLOCKED, attempt_count: 3 };
    mocked.retryWorktreeCleanup.mockResolvedValue(updated);
    useBlockedCleanupStore.setState({ entries: [BLOCKED], isOpen: true });

    await useBlockedCleanupStore.getState().retry(BLOCKED.worktree_path);

    const state = useBlockedCleanupStore.getState();
    expect(state.entries).toEqual([updated]);
    expect(state.isOpen).toBe(true, 'the dialog stays open while something is still blocked');
    expect(state.actionStatus[BLOCKED.worktree_path]).toMatch(/still blocked/i);
  });

  it('drops the entry when the backend reports the worktree is gone (null)', async () => {
    mocked.retryWorktreeCleanup.mockResolvedValue(null);
    useBlockedCleanupStore.setState({ entries: [BLOCKED], isOpen: true });

    await useBlockedCleanupStore.getState().retry(BLOCKED.worktree_path);

    const state = useBlockedCleanupStore.getState();
    expect(state.entries).toEqual([]);
    expect(state.isOpen).toBe(false, 'nothing left to act on');
  });

  it('keeps the worktree in place on "keep worktree" and closes when empty', async () => {
    mocked.dismissWorktreeCleanup.mockResolvedValue(undefined);
    useBlockedCleanupStore.setState({ entries: [BLOCKED], isOpen: true });

    await useBlockedCleanupStore.getState().dismiss(BLOCKED.worktree_path);

    expect(mocked.dismissWorktreeCleanup).toHaveBeenCalledWith(BLOCKED.worktree_path);
    const state = useBlockedCleanupStore.getState();
    expect(state.entries).toEqual([]);
    expect(state.isOpen).toBe(false);
    // Nothing on disk was asked to change — the toast says what happened.
    expect(useToastStore.getState().toasts.some((t) => t.message.includes('kept in place'))).toBe(true);
  });

  it('records the diagnosis per path and clears it before a retry', async () => {
    mocked.diagnoseWorktreeCleanupBlockers.mockResolvedValue([HOLDER]);
    mocked.retryWorktreeCleanup.mockResolvedValue(BLOCKED);
    useBlockedCleanupStore.setState({ entries: [BLOCKED], isOpen: true });

    await useBlockedCleanupStore.getState().diagnose(BLOCKED.worktree_path);
    expect(useBlockedCleanupStore.getState().blockers[BLOCKED.worktree_path]).toEqual([HOLDER]);
    expect(useBlockedCleanupStore.getState().actionStatus[BLOCKED.worktree_path]).toMatch(/may be holding/i);

    await useBlockedCleanupStore.getState().retry(BLOCKED.worktree_path);
    expect(useBlockedCleanupStore.getState().blockers[BLOCKED.worktree_path]).toBeUndefined();
  });

  it('releasing a blocker ends that one process and then retries the cleanup', async () => {
    mocked.releaseWorktreeCleanupBlocker.mockResolvedValue(undefined);
    mocked.diagnoseWorktreeCleanupBlockers.mockResolvedValue([HOLDER]);
    mocked.retryWorktreeCleanup.mockResolvedValue(null);
    useBlockedCleanupStore.setState({ entries: [BLOCKED], isOpen: true, blockers: { [BLOCKED.worktree_path]: [HOLDER] } });

    await useBlockedCleanupStore.getState().release(BLOCKED.worktree_path, 5212);

    expect(mocked.releaseWorktreeCleanupBlocker).toHaveBeenCalledWith(5212);
    const state = useBlockedCleanupStore.getState();
    expect(state.blockers[BLOCKED.worktree_path]).toBeUndefined();
    expect(state.entries).toEqual([]);
    expect(state.isOpen).toBe(false);
  });

  it('surfaces a failed release without dropping the blocked entry', async () => {
    mocked.releaseWorktreeCleanupBlocker.mockRejectedValue(new Error('access denied'));
    useBlockedCleanupStore.setState({ entries: [BLOCKED], isOpen: true, blockers: { [BLOCKED.worktree_path]: [HOLDER] } });

    await useBlockedCleanupStore.getState().release(BLOCKED.worktree_path, 5212);

    const state = useBlockedCleanupStore.getState();
    expect(state.entries).toEqual([BLOCKED], 'the blocked cleanup stays until it is resolved');
    expect(state.actionStatus[BLOCKED.worktree_path]).toMatch(/access denied/);
    expect(mocked.retryWorktreeCleanup).not.toHaveBeenCalled();
  });

  it('openBlockedCleanups reaches the store from a non-React caller', async () => {
    mocked.listPendingWorktreeRemovals.mockResolvedValue([BLOCKED]);
    openBlockedCleanups();
    await vi.waitFor(() => expect(useBlockedCleanupStore.getState().isOpen).toBe(true));
  });
});
