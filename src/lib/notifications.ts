import type { Toast } from '../lib/toastUtils';

export interface WorktreeNotificationAction {
  kind: 'worktree-cleanup';
  path: string;
  nodeName: string;
  detail: string;
  resolution?: string;
}

export interface Notification extends Toast {
  read: boolean;
  action?: WorktreeNotificationAction;
}

export interface WorktreeNotice {
  path: string;
  nodeName: string;
  message: string;
  detail: string;
  createdAt: number;
}

export const NOTIFICATION_HISTORY_LIMIT = 100;
export const needsAttention = (notice: Notification): boolean =>
  notice.action?.kind === 'worktree-cleanup' && !notice.action.resolution;

export function limitNotifications(notices: Notification[]): Notification[] {
  let activity = 0;
  return [...notices].sort((a, b) => b.createdAt - a.createdAt)
    .filter(n => needsAttention(n) || ++activity <= NOTIFICATION_HISTORY_LIMIT);
}

export function restoreNotifications(value: unknown): Notification[] {
  if (!Array.isArray(value)) return [];
  return limitNotifications(value.filter((n): n is Notification => {
    if (!n || typeof n !== 'object' || !Number.isSafeInteger(n.id)
      || typeof n.provider !== 'string' || typeof n.message !== 'string'
      || typeof n.read !== 'boolean' || !Number.isFinite(n.createdAt) || Math.abs(n.createdAt) > 8.64e15
      || !['error', 'warning', 'success', 'info', undefined].includes(n.severity)) return false;
    const a = n.action;
    return a === undefined || (a !== null && typeof a === 'object' && a.kind === 'worktree-cleanup' && typeof a.path === 'string'
      && typeof a.nodeName === 'string' && typeof a.detail === 'string'
      && (a.resolution === undefined || typeof a.resolution === 'string'));
  }));
}
