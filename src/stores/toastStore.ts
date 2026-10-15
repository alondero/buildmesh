import { create } from 'zustand';
import { createJSONStorage, persist } from 'zustand/middleware';
import {
  applyToastCap, createToastKey, dedupToasts, TOAST_DEDUP_TTL_MS, TOAST_MAX, TOAST_TTL_MS,
  type Toast, type ToastSeverity,
} from '../lib/toastUtils';
import {
  isMeshSyncFailure, isWorktreeCleanup, limitNotifications, needsAttention, restoreNotifications,
  type Notification, type NotificationAction, type WorktreeNotice,
} from '../lib/notifications';

export interface AddToastOptions {
  /** Record the notification without showing a toast. See `addToast`. */
  silent?: boolean;
}

interface ToastState {
  toasts: Toast[];
  notifications: Notification[];
  cleanupLoading: boolean;
  cleanupError: string | null;
  worktreeRevision: number;
  addToast: (provider: string, message: string, severity?: ToastSeverity, action?: NotificationAction, options?: AddToastOptions) => void;
  dismissToast: (id: number) => void;
  dismissExpired: (now: number) => void;
  markNotificationRead: (id: number) => void;
  markAllNotificationsRead: () => void;
  clearReadNotifications: () => void;
  resolveWorktreeNotification: (path: string, resolution: string) => void;
  resolveMeshSyncNotification: (path: string, resolution: string) => void;
  reconcileWorktrees: (entries: WorktreeNotice[], expectedRevision: number) => void;
}

let nextToastId = 0;

/** The unresolved problem, if any, that a repeat of this action should merge
 *  into rather than stack beside. Keyed by *kind + path*, not path alone: two
 *  kinds can legitimately name the same directory (a mesh whose root checkout
 *  is itself the blocked worktree), and collapsing them would drop one
 *  problem's record into another's. */
function previousFor(notices: Notification[], action: NotificationAction): Notification | undefined {
  return notices.find(n => needsAttention(n) && n.action?.kind === action.kind && n.action.path === action.path);
}

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
  addToast: (provider, message, severity = 'error', action, options) => {
    const now = Date.now();
    set(state => {
      // `silent` records the problem in history and in Needs attention without
      // putting anything on the three-deep toast stack. It is how a command
      // that syncs N meshes reports N problems without N toasts: the caller
      // shows one summary instead. Only meaningful with an action — a silent
      // record with no action would be history nobody ever sees, so the flag is
      // ignored there and the toast is shown as usual.
      //
      // The recording step below is shared by both cases; only the toast stack
      // differs. Recording is the part that must not drift: a silent record and
      // a visible one have to deduplicate against the same unresolved problem
      // and be retained by the same rules, or the same failure would file two
      // differently-behaving records depending on which entry point reported it.
      const silent = options?.silent === true && action !== undefined;
      const incoming = { id: ++nextToastId, provider, message, createdAt: now, severity };
      const merged = silent ? state.toasts : dedupToasts(state.toasts, incoming, now, TOAST_DEDUP_TTL_MS);
      // The record the history entry is built from: the toast as it ended up on
      // the stack after dedup, or the incoming record when there is no stack.
      const toast = silent
        ? incoming
        : merged.find(t => t.createdAt === now && createToastKey(t) === createToastKey(incoming))!;
      const previous = action
        ? previousFor(state.notifications, action)
        : state.notifications.find(n => !n.action && n.id === toast.id);
      const id = previous?.id ?? (state.notifications.some(n => n.id === toast.id) ? ++nextToastId : toast.id);
      const notice: Notification = { ...toast, id, message, read: false, action };
      return {
        toasts: silent ? state.toasts : applyToastCap(merged, TOAST_MAX),
        notifications: limitNotifications([notice, ...state.notifications.filter(n => n.id !== id)]),
        // Only a worktree-cleanup action changes what the reconciliation can
        // legitimately decide. Bumping on a sync failure would instead fence
        // out an in-flight queue read for no reason.
        worktreeRevision: state.worktreeRevision + (isWorktreeCleanup(action) ? 1 : 0),
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
    // `needsAttention` matters: a path can be blocked again after an earlier
    // attempt was resolved. Without it this setter would also rewrite that
    // older record's resolution text, overwriting what actually happened then.
    notifications: limitNotifications(state.notifications.map(n => isWorktreeCleanup(n.action) && needsAttention(n)
      && n.action.path === path
      ? { ...n, action: { ...n.action, resolution } } : n)),
  })),
  /** Close a mesh-sync failure. Deliberately does NOT bump
   *  `worktreeRevision`: that counter fences an in-flight cleanup-queue read,
   *  and a sync saying nothing about worktrees is not newer evidence about
   *  them — bumping here would make a concurrent poll discard a perfectly
   *  good reconciliation. */
  resolveMeshSyncNotification: (path, resolution) => set(state => ({
    notifications: limitNotifications(state.notifications.map(n => isMeshSyncFailure(n.action) && needsAttention(n)
      && n.action.path === path
      ? { ...n, action: { ...n.action, resolution } } : n)),
  })),
  reconcileWorktrees: (entries, expectedRevision) => set(state => {
    // A failure event or manual recovery after this read began owns the newer state.
    if (state.worktreeRevision !== expectedRevision) return state;
    // Only cleanup notices may be closed by queue absence. A mesh sync failure
    // is not in this queue and is not ended by a row leaving it, so a poll
    // must never invent a resolution for one.
    let notifications = state.notifications.map(n => {
      // Bound to a local so the narrowing survives into the `some` closure —
      // `n.action` is re-read there, and TS does not carry a property
      // narrowing across a function boundary.
      const existing = n.action;
      return isWorktreeCleanup(existing) && needsAttention(n)
        && !entries.some(e => e.path === existing.path)
        ? { ...n, action: { ...existing, resolution: 'No longer awaiting cleanup.' } } : n;
    });
    for (const entry of entries) {
      const previous = notifications.find(n => isWorktreeCleanup(n.action) && needsAttention(n) && n.action.path === entry.path);
      const action: NotificationAction = { kind: 'worktree-cleanup', path: entry.path, nodeName: entry.nodeName, detail: entry.detail };
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

export function addToast(provider: string, message: string, severity?: ToastSeverity, action?: NotificationAction, options?: AddToastOptions): void {
  useToastStore.getState().addToast(provider, message, severity, action, options);
}

export function dismissToast(id: number): void {
  useToastStore.getState().dismissToast(id);
}
