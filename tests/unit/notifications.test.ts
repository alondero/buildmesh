import { beforeEach, describe, expect, it, vi } from 'vitest';
import { addToast, dismissToast, useToastStore } from '../../src/stores/toastStore';
import { needsAttention, NOTIFICATION_HISTORY_LIMIT, restoreNotifications } from '../../src/lib/notifications';
import { TOAST_TTL_MS } from '../../src/lib/toastUtils';

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
