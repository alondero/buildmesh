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

  it('keeps the dialog open and reports the failure when the queue cannot be read', async () => {
    // `list_pending_worktree_removals` rejects when the queue read fails, so a
    // stuck worktree cannot be hidden behind an empty list (issue #2139 review
    // round 2). The command must reject for this test to be able to fail.
    mocked.listPendingWorktreeRemovals.mockRejectedValue(new Error('db locked'));
    useBlockedCleanupStore.setState({ isOpen: true, entries: [] });

    await useBlockedCleanupStore.getState().open();

    const state = useBlockedCleanupStore.getState();
    expect(state.loading).toBe(false);
    expect(state.isOpen, 'the only recovery surface must not close on a read failure').toBe(true);
    const toasts = useToastStore.getState().toasts;
    expect(toasts.some((t) => t.provider === 'Worktree' && t.message.includes('db locked'))).toBe(true);
  });

  it('replaces the entry with the backend row when a retry is still blocked', async () => {
    const updated = { ...BLOCKED, attempt_count: 3 };
    mocked.retryWorktreeCleanup.mockResolvedValue({ status: 'still-blocked', record: updated });
    useBlockedCleanupStore.setState({ entries: [BLOCKED], isOpen: true });

    await useBlockedCleanupStore.getState().retry(BLOCKED.worktree_path);

    const state = useBlockedCleanupStore.getState();
    expect(state.entries).toEqual([updated]);
    expect(state.isOpen, 'the dialog stays open while something is still blocked').toBe(true);
    expect(state.actionStatus[BLOCKED.worktree_path]).toMatch(/still blocked/i);
  });

  it('drops the entry when the backend reports the worktree is removed', async () => {
    mocked.retryWorktreeCleanup.mockResolvedValue({ status: 'removed', record: null });
    useBlockedCleanupStore.setState({ entries: [BLOCKED], isOpen: true });

    await useBlockedCleanupStore.getState().retry(BLOCKED.worktree_path);

    const state = useBlockedCleanupStore.getState();
    expect(state.entries).toEqual([]);
    expect(state.isOpen, 'nothing left to act on').toBe(false);
    expect(
      useToastStore.getState().toasts.some((toast) => toast.message.includes('Worktree removed')),
    ).toBe(true);
  });

  it('drops the entry without claiming a removal when the backend says it is already gone', async () => {
    mocked.retryWorktreeCleanup.mockResolvedValue({ status: 'gone', record: null });
    useBlockedCleanupStore.setState({ entries: [BLOCKED], isOpen: true });

    await useBlockedCleanupStore.getState().retry(BLOCKED.worktree_path);

    const state = useBlockedCleanupStore.getState();
    expect(state.entries).toEqual([]);
    expect(state.isOpen).toBe(false);
    const toasts = useToastStore.getState().toasts;
    expect(toasts.some((toast) => /already cleaned/i.test(toast.message))).toBe(true);
    expect(toasts.some((toast) => /Worktree removed/.test(toast.message))).toBe(
      false,
      'a gone entry is not a removal',
    );
  });

  it('drops the entry and says a live agent owns it when the claim guard fires', async () => {
    mocked.retryWorktreeCleanup.mockResolvedValue({ status: 'claimed', record: null });
    useBlockedCleanupStore.setState({ entries: [BLOCKED], isOpen: true });

    await useBlockedCleanupStore.getState().retry(BLOCKED.worktree_path);

    const state = useBlockedCleanupStore.getState();
    expect(state.entries).toEqual([]);
    expect(
      useToastStore.getState().toasts.some((toast) => /live agent/i.test(toast.message)),
    ).toBe(true);
  });

  it('keeps the entry when the queue could not be read, and says nothing was retried', async () => {
    mocked.retryWorktreeCleanup.mockResolvedValue({ status: 'queue-read-failed', record: null });
    useBlockedCleanupStore.setState({ entries: [BLOCKED], isOpen: true });

    await useBlockedCleanupStore.getState().retry(BLOCKED.worktree_path);

    const state = useBlockedCleanupStore.getState();
    expect(state.entries).toEqual([BLOCKED], 'the row is untouched, so it stays visible');
    expect(state.isOpen).toBe(true);
    expect(
      useToastStore.getState().toasts.some((toast) => /nothing was retried/i.test(toast.message)),
    ).toBe(true);
  });

  it('keeps the worktree in place on "keep worktree" and repeats the backend message', async () => {
    mocked.dismissWorktreeCleanup.mockResolvedValue({
      message: `Worktree kept in place: ${BLOCKED.worktree_path}`,
      cancelled: true,
    });
    useBlockedCleanupStore.setState({ entries: [BLOCKED], isOpen: true });

    await useBlockedCleanupStore.getState().dismiss(BLOCKED.worktree_path);

    expect(mocked.dismissWorktreeCleanup).toHaveBeenCalledWith(BLOCKED.worktree_path);
    const state = useBlockedCleanupStore.getState();
    expect(state.entries).toEqual([]);
    expect(state.isOpen).toBe(false);
    // The message is the backend's, because only it knows what happened to the
    // disk (a removal may already have moved the folder aside).
    expect(
      useToastStore.getState().toasts.some((toast) => toast.message === `Worktree kept in place: ${BLOCKED.worktree_path}`),
    ).toBe(true);
  });

  it('keeps the entry when the restore failed, so the drain keeps trying', async () => {
    // The staged copy could not be moved back: the row stays queued, so the
    // entry must stay in the dialog (issue #2139 review round 2).
    mocked.dismissWorktreeCleanup.mockResolvedValue({
      message: `Couldn't move the worktree back to ${BLOCKED.worktree_path} — something still holds`,
      cancelled: false,
    });
    useBlockedCleanupStore.setState({ entries: [BLOCKED], isOpen: true });

    await useBlockedCleanupStore.getState().dismiss(BLOCKED.worktree_path);

    const state = useBlockedCleanupStore.getState();
    expect(state.entries).toEqual([BLOCKED], 'the cleanup intent survived, so the entry stays');
    expect(state.isOpen).toBe(true);
    expect(
      useToastStore.getState().toasts.some((toast) => /Couldn't move the worktree back/.test(toast.message)),
    ).toBe(true);
  });

  it('records the diagnosis per path and clears it before a retry', async () => {
    mocked.diagnoseWorktreeCleanupBlockers.mockResolvedValue([HOLDER]);
    mocked.retryWorktreeCleanup.mockResolvedValue({ status: 'still-blocked', record: BLOCKED });
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
    mocked.retryWorktreeCleanup.mockResolvedValue({ status: 'removed', record: null });
    useBlockedCleanupStore.setState({
      entries: [BLOCKED],
      isOpen: true,
      blockers: { [BLOCKED.worktree_path]: [HOLDER] },
    });
    useBlockedCleanupStore.getState().requestRelease(BLOCKED.worktree_path, HOLDER);

    await useBlockedCleanupStore.getState().confirmRelease();

    expect(mocked.releaseWorktreeCleanupBlocker).toHaveBeenCalledWith(
      BLOCKED.worktree_path,
      5212,
    );
    const state = useBlockedCleanupStore.getState();
    expect(state.pendingKill).toBeNull();
    expect(state.blockers[BLOCKED.worktree_path]).toBeUndefined();
    expect(state.entries).toEqual([]);
    expect(state.isOpen).toBe(false);
  });

  it('does nothing when a release is confirmed with nothing pending', async () => {
    useBlockedCleanupStore.getState().cancelRelease();
    await useBlockedCleanupStore.getState().confirmRelease();
    expect(mocked.releaseWorktreeCleanupBlocker).not.toHaveBeenCalled();
  });

  it('surfaces a refused release without dropping the blocked entry', async () => {
    // The backend refuses a pid that is no longer holding the worktree, which
    // is what an id-reuse accident looks like (issue #2139 review round 1).
    mocked.releaseWorktreeCleanupBlocker.mockRejectedValue(
      new Error('process 5212 is no longer holding'),
    );
    useBlockedCleanupStore.setState({
      entries: [BLOCKED],
      isOpen: true,
      blockers: { [BLOCKED.worktree_path]: [HOLDER] },
    });
    useBlockedCleanupStore.getState().requestRelease(BLOCKED.worktree_path, HOLDER);

    await useBlockedCleanupStore.getState().confirmRelease();

    const state = useBlockedCleanupStore.getState();
    expect(state.entries).toEqual([BLOCKED], 'the blocked cleanup stays until it is resolved');
    expect(state.actionStatus[BLOCKED.worktree_path]).toMatch(/no longer holding/);
    expect(mocked.retryWorktreeCleanup).not.toHaveBeenCalled();
  });

  it('openBlockedCleanups reaches the store from a non-React caller', async () => {
    mocked.listPendingWorktreeRemovals.mockResolvedValue([BLOCKED]);
    openBlockedCleanups();
    await vi.waitFor(() => expect(useBlockedCleanupStore.getState().isOpen).toBe(true));
  });
});
