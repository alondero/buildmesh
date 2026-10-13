import { beforeEach, expect, it, vi } from 'vitest';
import { addToast, useToastStore } from '../../src/stores/toastStore';
import { needsAttention } from '../../src/lib/notifications';
import { refreshWorktreeNotifications, watchWorktreeNotifications } from '../../src/lib/worktreeNotifications';
import { useBlockedCleanupStore } from '../../src/stores/blockedCleanupStore';
import * as api from '../../src/lib/tauri';
import type { PendingWorktreeRemoval } from '../../src/types/generated/PendingWorktreeRemoval';

vi.mock('../../src/lib/tauri', () => ({
  listPendingWorktreeRemovals: vi.fn(), retryWorktreeCleanup: vi.fn(), dismissWorktreeCleanup: vi.fn(),
  diagnoseWorktreeCleanupBlockers: vi.fn(), releaseWorktreeCleanupBlocker: vi.fn(),
}));
const entry: PendingWorktreeRemoval = {
  worktree_path: '/repo/blocked', node_name: 'blocked', attempt_count: 2, last_attempt_at: 1700000000000,
  last_operation: 'rename-worktree-to-staging', last_error: 'Folder in use', retry_not_before: 1700000005000,
  notified_error: 'Folder in use', notified_at: 1700000000000,
};
const action = { kind: 'worktree-cleanup' as const, path: entry.worktree_path, nodeName: entry.node_name, detail: entry.last_error! };
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(r => { resolve = r; });
  return { promise, resolve };
}
beforeEach(() => {
  vi.clearAllMocks();
  useToastStore.setState({ toasts: [], notifications: [], worktreeRevision: 0, cleanupError: null, cleanupLoading: false });
  useBlockedCleanupStore.setState({ entries: [entry], isOpen: false, loading: false, blockers: {}, actionStatus: {} });
});

it('restores failed queue entries without replaying toasts or showing a dialog, then follows automatic recovery', async () => {
  vi.mocked(api.listPendingWorktreeRemovals).mockResolvedValue([entry, { ...entry, worktree_path: '/new', attempt_count: 0, last_error: null }]);
  await refreshWorktreeNotifications();
  expect(useToastStore.getState().notifications).toHaveLength(1);
  expect(needsAttention(useToastStore.getState().notifications[0])).toBe(true);
  expect(useToastStore.getState().toasts).toEqual([]);
  expect(useBlockedCleanupStore.getState().isOpen).toBe(false);
  vi.mocked(api.listPendingWorktreeRemovals).mockResolvedValue([]);
  await refreshWorktreeNotifications();
  expect(useToastStore.getState().notifications[0].action?.resolution).toBe('No longer awaiting cleanup.');
});

it.each([true, false])('only commits the newest queue read (old read finishes first: %s)', async oldFirst => {
  const old = deferred<PendingWorktreeRemoval[]>();
  const latest = deferred<PendingWorktreeRemoval[]>();
  vi.mocked(api.listPendingWorktreeRemovals).mockReturnValueOnce(old.promise).mockReturnValueOnce(latest.promise);
  const p1 = refreshWorktreeNotifications();
  const p2 = refreshWorktreeNotifications();
  if (oldFirst) { old.resolve([]); await p1; latest.resolve([entry]); await p2; }
  else { latest.resolve([entry]); await p2; old.resolve([]); await p1; }
  expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  expect(useToastStore.getState().cleanupLoading).toBe(false);
});

it('does not let an older empty queue hide a newer failure event', async () => {
  const queue = deferred<PendingWorktreeRemoval[]>();
  vi.mocked(api.listPendingWorktreeRemovals).mockReturnValueOnce(queue.promise);
  const pending = refreshWorktreeNotifications();
  addToast('Worktree', 'Failed', 'warning', action);
  queue.resolve([]); await pending;
  expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
});

it.each([true, false])('shares ownership across an older poll and newer Resolve read (old first: %s)', async oldFirst => {
  addToast('Worktree', 'Failed', 'warning', action);
  const old = deferred<PendingWorktreeRemoval[]>();
  const latest = deferred<PendingWorktreeRemoval[]>();
  vi.mocked(api.listPendingWorktreeRemovals).mockReturnValueOnce(old.promise).mockReturnValueOnce(latest.promise);
  const p1 = refreshWorktreeNotifications();
  const p2 = useBlockedCleanupStore.getState().open(entry.worktree_path);
  if (oldFirst) { old.resolve([entry]); await p1; latest.resolve([]); await p2; }
  else { latest.resolve([]); await p2; old.resolve([entry]); await p1; }
  expect(useToastStore.getState().notifications.some(needsAttention)).toBe(false);
  expect(useBlockedCleanupStore.getState().isOpen).toBe(false);
});

it.each([true, false])('shares ownership across an older Resolve and newer poll (old first: %s)', async oldFirst => {
  const old = deferred<PendingWorktreeRemoval[]>();
  const latest = deferred<PendingWorktreeRemoval[]>();
  vi.mocked(api.listPendingWorktreeRemovals).mockReturnValueOnce(old.promise).mockReturnValueOnce(latest.promise);
  const p1 = useBlockedCleanupStore.getState().open();
  const p2 = refreshWorktreeNotifications();
  if (oldFirst) { old.resolve([]); await p1; latest.resolve([entry]); await p2; }
  else { latest.resolve([entry]); await p2; old.resolve([]); await p1; }
  expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
});

it('keeps a newer dialog read loading when an older open finishes', async () => {
  const old = deferred<PendingWorktreeRemoval[]>();
  const latest = deferred<PendingWorktreeRemoval[]>();
  vi.mocked(api.listPendingWorktreeRemovals).mockReturnValueOnce(old.promise).mockReturnValueOnce(latest.promise);
  const p1 = useBlockedCleanupStore.getState().open();
  const p2 = useBlockedCleanupStore.getState().open(entry.worktree_path);
  old.resolve([]); await p1;
  expect(useBlockedCleanupStore.getState()).toMatchObject({ loading: true, isOpen: true });
  latest.resolve([entry]); await p2;
  expect(useBlockedCleanupStore.getState()).toMatchObject({ loading: false, isOpen: true, entries: [entry] });
});

it('does not resurrect a manually resolved problem from a stale queue read', async () => {
  addToast('Worktree', 'Failed', 'warning', action);
  const queue = deferred<PendingWorktreeRemoval[]>();
  vi.mocked(api.listPendingWorktreeRemovals).mockReturnValueOnce(queue.promise);
  const pending = refreshWorktreeNotifications();
  useToastStore.getState().resolveWorktreeNotification(entry.worktree_path, 'Worktree kept.');
  queue.resolve([entry]); await pending;
  expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(0);
});

it('keeps last known problems on a failed read and clears the error after recovery', async () => {
  addToast('Worktree', 'Failed', 'warning', action);
  vi.mocked(api.listPendingWorktreeRemovals).mockRejectedValueOnce(new Error('queue locked')).mockResolvedValueOnce([entry]);
  await refreshWorktreeNotifications();
  expect(useToastStore.getState().cleanupError).toContain('queue locked');
  expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  await refreshWorktreeNotifications();
  expect(useToastStore.getState().cleanupError).toBeNull();
});

it('cancels an in-flight watcher read on unmount', async () => {
  const queue = deferred<PendingWorktreeRemoval[]>();
  vi.mocked(api.listPendingWorktreeRemovals).mockReturnValueOnce(queue.promise);
  const stop = watchWorktreeNotifications();
  stop(); queue.resolve([entry]); await queue.promise; await Promise.resolve();
  expect(useToastStore.getState().notifications).toEqual([]);
  expect(useToastStore.getState().cleanupLoading).toBe(false);
});

it.each(['removed', 'gone', 'claimed', 'still-blocked', 'queue-read-failed'] as const)('records the actual recovery outcome for %s', async status => {
  addToast('Worktree', 'Failed', 'warning', action);
  vi.mocked(api.retryWorktreeCleanup).mockResolvedValue({ status, record: status === 'still-blocked' ? entry : null });
  await useBlockedCleanupStore.getState().retry(entry.worktree_path);
  const notification = useToastStore.getState().notifications.find(n => n.action?.path === entry.worktree_path)!;
  expect(needsAttention(notification)).toBe(status === 'still-blocked' || status === 'queue-read-failed');
  if (status === 'claimed') expect(notification.action?.resolution).toContain('live agent');
  if (status === 'gone') expect(notification.action?.resolution).toBe('No longer awaiting cleanup.');
  if (status === 'removed') expect(notification.action?.resolution).toContain('Removed');
});

it.each([true, false])('resolves Keep worktree only after the intent was cancelled (%s)', async cancelled => {
  addToast('Worktree', 'Failed', 'warning', action);
  vi.mocked(api.dismissWorktreeCleanup).mockResolvedValue({ cancelled, message: cancelled ? 'Kept' : 'Restore failed' });
  await useBlockedCleanupStore.getState().dismiss(entry.worktree_path);
  expect(useToastStore.getState().notifications.some(needsAttention)).toBe(!cancelled);
});
