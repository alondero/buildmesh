import { lazy, Suspense, useEffect, useState } from 'react';
import { useToastStore } from '../../stores/toastStore';
import { openBlockedCleanups, useBlockedCleanupStore } from '../../stores/blockedCleanupStore';
import { needsAttention, type Notification } from '../../lib/notifications';
import { refreshWorktreeNotifications } from '../../lib/worktreeNotifications';
import { ProbeTabBody } from './ProbeTabBody';
import { BellIcon } from './probeIcons';
import './NotificationsTab.css';

const BlockedCleanupDialog = lazy(() => import('../BlockedCleanupDialog/BlockedCleanupDialog')
  .then(m => ({ default: m.BlockedCleanupDialog })));

function NoticeTime({ notice }: { notice: Notification }) {
  const date = new Date(notice.createdAt);
  return <time className="notification-time" dateTime={date.toISOString()} title={date.toLocaleString()}>
    {date.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })}
    {' · '}{date.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })}
  </time>;
}

export function NotificationsTab() {
  const notices = useToastStore(s => s.notifications);
  const loading = useToastStore(s => s.cleanupLoading);
  const error = useToastStore(s => s.cleanupError);
  const [recoveryMounted, setRecoveryMounted] = useState(false);
  const attention = notices.filter(needsAttention);
  const activity = notices.filter(n => !needsAttention(n));
  const unread = notices.filter(n => !n.read).length;
  const clearable = activity.some(n => n.read);

  useEffect(() => {
    void refreshWorktreeNotifications();
    return () => useBlockedCleanupStore.getState().close();
  }, []);

  function resolve(notice: Notification) {
    if (!notice.action) return;
    useToastStore.getState().markNotificationRead(notice.id);
    setRecoveryMounted(true);
    openBlockedCleanups(notice.action.path);
  }

  return <div className="notifications-tab flex-1 min-h-0 min-w-0 flex flex-col" data-testid="notifications-tab">
    <div className="notification-toolbar">
      <span>{unread} unread <span aria-hidden="true">·</span> {attention.length} need attention</span>
      <button type="button" className="notification-link" disabled={!unread}
        onClick={() => useToastStore.getState().markAllNotificationsRead()}>Mark all read</button>
    </div>
    {error && <div className="notification-error" role="alert">
      <p>{error}. Showing the last known notifications.</p>
      <button type="button" className="notification-link" disabled={loading}
        onClick={() => void refreshWorktreeNotifications()}>Try again</button>
    </div>}
    <ProbeTabBody>
      {notices.length === 0 ? <div className="notification-empty">
        <BellIcon /><h3>No notifications yet</h3>
        <p>App toasts will appear here. Worktree problems will have a recovery action.</p>
        {loading && <p role="status">Checking worktree problems…</p>}
      </div> : <>
        <section aria-label="Needs attention" className="notification-attention">
          <div className="notification-section-heading"><h3>Needs attention</h3><span>{attention.length}</span></div>
          {attention.length === 0 ? <p className="notification-caught-up">
            {error ? 'Worktree status is unavailable.' : loading ? 'Checking worktree problems…' : 'You’re all caught up.'}
          </p> : attention.map(notice => <article className="notification-attention-card" key={notice.id}>
            <div className="notification-heading"><h4>Worktree cleanup blocked</h4><NoticeTime notice={notice} /></div>
            <div className="notification-node">{notice.action!.nodeName}</div>
            <p>{notice.action!.detail}</p>
            <code>{notice.action!.path}</code>
            <div className="notification-card-actions"><span>Queued for automatic retry</span>
              <button type="button" className="notification-resolve" aria-label={`Resolve worktree ${notice.action!.nodeName}`}
                onClick={() => resolve(notice)}>Resolve <span aria-hidden="true">→</span></button>
            </div>
          </article>)}
        </section>
        <section aria-label="Recent activity" className="notification-activity">
          <div className="notification-section-heading"><h3>Recent activity</h3><span>{activity.length}</span></div>
          {activity.length === 0 ? <p className="notification-caught-up">Your other app notifications will appear here.</p>
            : activity.map(notice => <article key={notice.id} className={`notification-row ${!notice.read ? 'notification-unread' : ''}`}>
              <span aria-label={notice.severity ?? 'error'} className={`notification-status notification-${notice.severity ?? 'error'}`}>
                {notice.action?.resolution || notice.severity === 'success' ? '✓' : notice.severity === 'info' ? 'i' : '!'}
              </span>
              <div className="notification-row-body">
                <div className="notification-heading"><h4>{notice.provider}</h4><NoticeTime notice={notice} /></div>
                <p>{notice.message}</p>
                {notice.action?.resolution && <p className="notification-resolution">{notice.action.resolution}</p>}
                {!notice.read && <button type="button" className="notification-link" aria-label={`Mark ${notice.provider} notification as read`}
                  onClick={() => useToastStore.getState().markNotificationRead(notice.id)}>Mark read</button>}
              </div>
            </article>)}
        </section>
      </>}
    </ProbeTabBody>
    <div className="notification-footer">
      <button type="button" className="notification-link" disabled={loading}
        onClick={() => void refreshWorktreeNotifications()}>{loading ? 'Refreshing…' : 'Refresh'}</button>
      <button type="button" className="notification-link" disabled={!clearable}
        onClick={() => useToastStore.getState().clearReadNotifications()}>Clear read</button>
    </div>
    {recoveryMounted && <Suspense fallback={null}><BlockedCleanupDialog /></Suspense>}
  </div>;
}
