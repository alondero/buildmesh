import { beforeEach, describe, expect, it, vi } from 'vitest';
import { addToast, dismissToast, useToastStore } from '../../src/stores/toastStore';
import { needsAttention, NOTIFICATION_HISTORY_LIMIT, restoreNotifications } from '../../src/lib/notifications';
import { TOAST_TTL_MS } from '../../src/lib/toastUtils';

// Spelled out rather than imported from `meshSyncFeedback`: these tests are
// about the store's handling of an action kind, so they should not drag the
// IPC layer in behind it. The label itself is pinned by mesh-sync-feedback.
const SYNC = 'Git sync';

const action = { kind: 'worktree-cleanup' as const, path: '/repo/worktrees/blocked', nodeName: 'blocked', detail: 'Folder in use' };

beforeEach(() => {
  useToastStore.setState({ toasts: [], notifications: [], worktreeRevision: 0, cleanupError: null, cleanupLoading: false });
});

describe('notification history', () => {
  it('captures toasts before the visible cap and retains history after dismissal and expiry', () => {
    for (let i = 0; i < 5; i++) addToast('App', `Message ${i}`, 'info');
    expect(useToastStore.getState().toasts).toHaveLength(3);
    dismissToast(useToastStore.getState().toasts[0].id);
    useToastStore.getState().dismissExpired(Date.now() + TOAST_TTL_MS);
    expect(useToastStore.getState().toasts).toHaveLength(0);
    expect(useToastStore.getState().notifications).toHaveLength(5);
    expect(useToastStore.getState().notifications.map(n => n.message)).toEqual(expect.arrayContaining(['Message 0', 'Message 4']));
  });

  it('refreshes a deduplicated notification and makes the new occurrence unread', () => {
    addToast('App', 'Repeated');
    const id = useToastStore.getState().notifications[0].id;
    useToastStore.getState().markAllNotificationsRead();
    addToast('App', 'Repeated', 'warning');
    expect(useToastStore.getState().notifications).toHaveLength(1);
    expect(useToastStore.getState().notifications[0]).toMatchObject({ id, read: false, severity: 'warning' });
  });

  it('deduplicates worktree reminders by unresolved path even after toast expiry', () => {
    addToast('Worktree', 'First failure', 'warning', action);
    const id = useToastStore.getState().notifications[0].id;
    useToastStore.getState().dismissExpired(Date.now() + TOAST_TTL_MS);
    addToast('Worktree', 'Changed failure', 'warning', { ...action, detail: 'Access denied' });
    expect(useToastStore.getState().notifications).toHaveLength(1);
    expect(useToastStore.getState().notifications[0]).toMatchObject({ id, action: { detail: 'Access denied' } });
  });

  it('protects read unresolved problems from clearing and the activity cap', () => {
    addToast('Worktree', 'Failed', 'warning', action);
    for (let i = 0; i <= NOTIFICATION_HISTORY_LIMIT; i++) addToast('App', `Message ${i}`, 'info');
    expect(useToastStore.getState().notifications).toHaveLength(NOTIFICATION_HISTORY_LIMIT + 1);
    useToastStore.getState().markAllNotificationsRead();
    useToastStore.getState().clearReadNotifications();
    expect(useToastStore.getState().notifications).toHaveLength(1);
    expect(needsAttention(useToastStore.getState().notifications[0])).toBe(true);
    useToastStore.getState().resolveWorktreeNotification(action.path, 'Worktree kept.');
    useToastStore.getState().clearReadNotifications();
    expect(useToastStore.getState().notifications).toHaveLength(0);
  });

  it('rehydrates read history and unresolved actions without replaying toasts', async () => {
    addToast('Worktree', 'Failed', 'warning', action);
    addToast('App', 'Complete', 'success');
    useToastStore.getState().markAllNotificationsRead();
    const saved = localStorage.getItem('buildmesh-notifications')!;
    useToastStore.setState({ toasts: [], notifications: [] });
    localStorage.setItem('buildmesh-notifications', saved);
    await useToastStore.persist.rehydrate();
    expect(useToastStore.getState().toasts).toEqual([]);
    expect(useToastStore.getState().notifications).toHaveLength(2);
    expect(useToastStore.getState().notifications.every(n => n.read)).toBe(true);
    addToast('App', 'Next');
    const ids = useToastStore.getState().notifications.map(n => n.id);
    expect(new Set(ids).size).toBe(3);
  });

  it('keeps notification delivery working when local storage cannot be written', () => {
    const storage = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('quota'); });
    try {
      expect(() => addToast('App', 'Delivery')).not.toThrow();
      expect(useToastStore.getState().notifications[0].message).toBe('Delivery');
    } finally { storage.mockRestore(); }
  });

  it('drops malformed persisted entries', () => {
    expect(restoreNotifications([null, { id: 'bad' }, { id: 1, read: true }])).toEqual([]);
  });
});

// The worktree queue can close a cleanup notice on its own, and only a cleanup
// notice. A mesh sync failure is resolved by a later successful sync, so if the
// 30-second reconciliation were allowed to touch it the user's problem would
// silently self-heal seconds after being reported.
describe('mesh sync failures alongside worktree problems', () => {
  const syncAction = { kind: 'mesh-sync-failure' as const, path: '/repos/mesh-a', meshName: 'mesh-a', detail: 'Fetch failed: network down' };

  beforeEach(() => {
    useToastStore.setState({ toasts: [], notifications: [], worktreeRevision: 0, cleanupError: null, cleanupLoading: false });
  });

  it('counts an unresolved sync as needing attention and a resolved one as history', () => {
    expect(needsAttention({ id: 1, provider: SYNC, message: 'x', createdAt: 1, read: false, action: syncAction })).toBe(true);
    expect(needsAttention({ id: 1, provider: SYNC, message: 'x', createdAt: 1, read: false, action: { ...syncAction, resolution: 'Synced.' } })).toBe(false);
  });

  it('keeps a sync failure out of the worktree reconciliation', () => {
    addToast(SYNC, 'mesh-a: Fetch failed', 'error', syncAction);
    useToastStore.getState().reconcileWorktrees([], 0);
    const [notice] = useToastStore.getState().notifications;
    expect(needsAttention(notice)).toBe(true);
    expect(notice.action).not.toHaveProperty('resolution');
  });

  it('keeps resolving a worktree problem from resolving a sync failure on the same path', () => {
    // A mesh's root checkout can itself be a blocked worktree path, so the two
    // kinds must never be conflated by path alone.
    addToast(SYNC, 'mesh-a: Fetch failed', 'error', syncAction);
    addToast('Worktree', 'Cannot remove', 'warning', { ...action, path: syncAction.path });
    useToastStore.getState().resolveWorktreeNotification(syncAction.path, 'Removed.');
    const sync = useToastStore.getState().notifications.filter(n => n.action?.kind === 'mesh-sync-failure');
    expect(sync).toHaveLength(1);
    expect(needsAttention(sync[0])).toBe(true);
  });

  it('does not merge a sync failure into a worktree problem that shares its path', () => {
    addToast('Worktree', 'Cannot remove', 'warning', { ...action, path: syncAction.path });
    addToast(SYNC, 'mesh-a: Fetch failed', 'error', syncAction);
    expect(useToastStore.getState().notifications).toHaveLength(2);
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(2);
  });

  it('resolves only the sync failure a later successful sync names', () => {
    addToast(SYNC, 'mesh-a: Fetch failed', 'error', syncAction);
    addToast(SYNC, 'mesh-b: Fetch failed', 'error', { ...syncAction, path: '/repos/mesh-b', meshName: 'mesh-b' });
    useToastStore.getState().resolveMeshSyncNotification('/repos/mesh-a', 'Synced successfully.');
    const attention = useToastStore.getState().notifications.filter(needsAttention);
    expect(attention).toHaveLength(1);
    expect(attention[0].action).toMatchObject({ path: '/repos/mesh-b' });
  });

  it('rehydrates a persisted sync failure as needing attention', () => {
    const restored = restoreNotifications([{ id: 1, provider: SYNC, message: 'mesh-a: Fetch failed', createdAt: 1, read: false, action: syncAction }]);
    expect(restored).toHaveLength(1);
    expect(needsAttention(restored[0])).toBe(true);
    expect(restored[0].action).toEqual(syncAction);
  });

  it('drops a persisted sync failure whose fields are incomplete', () => {
    // Rehydrating a malformed action would render a Needs-attention card with
    // no mesh name and no retry target.
    expect(restoreNotifications([
      { id: 1, provider: SYNC, message: 'x', createdAt: 1, read: false, action: { kind: 'mesh-sync-failure', path: '/repos/mesh-a' } },
      { id: 2, provider: SYNC, message: 'x', createdAt: 2, read: false, action: { kind: 'something-else', path: '/repos/mesh-a' } },
    ])).toEqual([]);
  });

  it('does not rewrite an older resolved cleanup for the same path', () => {
    // A path can be blocked again after an earlier attempt was resolved, so
    // two records can share it. Resolving the new one must not overwrite what
    // actually happened on the earlier one.
    addToast('Worktree', 'First block', 'warning', action);
    useToastStore.getState().resolveWorktreeNotification(action.path, 'Removed.');
    addToast('Worktree', 'Blocked again', 'warning', action);
    useToastStore.getState().resolveWorktreeNotification(action.path, 'Removed. (again)');

    const cleanup = useToastStore.getState().notifications.filter(n => n.action?.kind === 'worktree-cleanup');
    expect(cleanup).toHaveLength(2);
    // Order-independent: the point is that both texts survive, not which one
    // sorts first.
    expect(cleanup.map(n => n.action!.resolution)).toEqual(
      expect.arrayContaining(['Removed.', 'Removed. (again)']));
  });

  it('protects an unresolved sync failure from the activity cap and Clear read', () => {
    addToast(SYNC, 'mesh-a: Fetch failed', 'error', syncAction);
    for (let i = 0; i <= NOTIFICATION_HISTORY_LIMIT; i++) addToast('App', `Message ${i}`, 'info');
    useToastStore.getState().markAllNotificationsRead();
    useToastStore.getState().clearReadNotifications();
    const left = useToastStore.getState().notifications;
    expect(left).toHaveLength(1);
    expect(left[0].action?.kind).toBe('mesh-sync-failure');
  });
});
