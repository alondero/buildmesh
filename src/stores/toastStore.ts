import { create } from 'zustand';
import { createJSONStorage, persist } from 'zustand/middleware';
import {
  applyToastCap, createToastKey, dedupToasts, TOAST_DEDUP_TTL_MS, TOAST_MAX, TOAST_TTL_MS,
  type Toast, type ToastSeverity,
} from '../lib/toastUtils';
import {
  limitNotifications, needsAttention, restoreNotifications,
  type Notification, type WorktreeNotificationAction, type WorktreeNotice,
} from '../lib/notifications';

interface ToastState {
  toasts: Toast[];
  notifications: Notification[];
  cleanupLoading: boolean;
  cleanupError: string | null;
  worktreeRevision: number;
  addToast: (provider: string, message: string, severity?: ToastSeverity, action?: WorktreeNotificationAction) => void;
  dismissToast: (id: number) => void;
  dismissExpired: (now: number) => void;
  markNotificationRead: (id: number) => void;
  markAllNotificationsRead: () => void;
  clearReadNotifications: () => void;
  resolveWorktreeNotification: (path: string, resolution: string) => void;
  reconcileWorktrees: (entries: WorktreeNotice[], expectedRevision: number) => void;
}

let nextToastId = 0;

// Storage failure must not prevent the toast or recovery state transition.
let savedHistory: string | null = null;
const storage = createJSONStorage(() => ({
  getItem: (name: string) => { try { savedHistory = localStorage.getItem(name); return savedHistory; } catch { return null; } },
  setItem: (name: string, value: string) => {
    if (value === savedHistory) return;
    try { localStorage.setItem(name, value); savedHistory = value; } catch { /* Keep in-memory history. */ }
  },
  removeItem: (name: string) => { try { localStorage.removeItem(name); savedHistory = null; } catch { /* Storage may be unavailable. */ } },
}));

export const useToastStore = create<ToastState>()(persist((set) => ({
  toasts: [], notifications: [], cleanupLoading: false, cleanupError: null, worktreeRevision: 0,
  addToast: (provider, message, severity = 'error', action) => {
    const now = Date.now();
    set(state => {
      const incoming = { id: ++nextToastId, provider, message, createdAt: now, severity };
      const merged = dedupToasts(state.toasts, incoming, now, TOAST_DEDUP_TTL_MS);
      const toast = merged.find(t => t.createdAt === now && createToastKey(t) === createToastKey(incoming))!;
      const previous = action
        ? state.notifications.find(n => needsAttention(n) && n.action?.path === action.path)
        : state.notifications.find(n => !n.action && n.id === toast.id);
      const id = previous?.id ?? (state.notifications.some(n => n.id === toast.id) ? ++nextToastId : toast.id);
      const notice: Notification = { ...toast, id, message, read: false, action };
      return {
        toasts: applyToastCap(merged, TOAST_MAX),
        notifications: limitNotifications([notice, ...state.notifications.filter(n => n.id !== id)]),
        worktreeRevision: state.worktreeRevision + (action ? 1 : 0),
      };
    });
  },
  dismissToast: id => set(state => ({ toasts: state.toasts.filter(t => t.id !== id) })),
  dismissExpired: now => set(state => ({ toasts: state.toasts.filter(t => now - t.createdAt < TOAST_TTL_MS) })),
  markNotificationRead: id => set(state => ({ notifications: state.notifications.map(n => n.id === id ? { ...n, read: true } : n) })),
  markAllNotificationsRead: () => set(state => ({ notifications: state.notifications.map(n => ({ ...n, read: true })) })),
  clearReadNotifications: () => set(state => ({ notifications: state.notifications.filter(n => !n.read || needsAttention(n)) })),
  resolveWorktreeNotification: (path, resolution) => set(state => ({
    worktreeRevision: state.worktreeRevision + 1,
    notifications: limitNotifications(state.notifications.map(n => needsAttention(n) && n.action?.path === path
      ? { ...n, action: { ...n.action, resolution } } : n)),
  })),
  reconcileWorktrees: (entries, expectedRevision) => set(state => {
    // A failure event or manual recovery after this read began owns the newer state.
    if (state.worktreeRevision !== expectedRevision) return state;
    let notifications = state.notifications.map(n => needsAttention(n) && !entries.some(e => e.path === n.action?.path)
      ? { ...n, action: { ...n.action!, resolution: 'No longer awaiting cleanup.' } } : n);
    for (const entry of entries) {
      const previous = notifications.find(n => needsAttention(n) && n.action?.path === entry.path);
      const action: WorktreeNotificationAction = { kind: 'worktree-cleanup', path: entry.path, nodeName: entry.nodeName, detail: entry.detail };
      const notice: Notification = {
        id: previous?.id ?? ++nextToastId, provider: 'Worktree', severity: 'warning', message: entry.message,
        createdAt: entry.createdAt, action,
        read: previous?.message === entry.message ? previous.read : false,
      };
      notifications = [notice, ...notifications.filter(n => n.id !== notice.id)];
    }
    return { notifications: limitNotifications(notifications), worktreeRevision: state.worktreeRevision + 1 };
  }),
}), {
  name: 'buildmesh-notifications', version: 1, storage,
  partialize: state => ({ notifications: state.notifications }),
  merge: (persisted, current) => {
    const notifications = restoreNotifications((persisted as { notifications?: unknown } | undefined)?.notifications);
    nextToastId = Math.max(nextToastId, ...notifications.map(n => n.id));
    return { ...current, notifications };
  },
}));

export function addToast(provider: string, message: string, severity?: ToastSeverity, action?: WorktreeNotificationAction): void {
  useToastStore.getState().addToast(provider, message, severity, action);
}

export function dismissToast(id: number): void {
  useToastStore.getState().dismissToast(id);
}
