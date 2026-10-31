import { PROBE_TAB_DEFINITIONS } from '../../lib/probeContext';
import { useProbeResource } from '../../lib/probeResource';
import { lazy, Suspense, useEffect, useState, type ReactNode } from 'react';
import { useToastStore } from '../../stores/toastStore';
import { openBlockedCleanups, useBlockedCleanupStore } from '../../stores/blockedCleanupStore';
import { isAgentInput, isMeshSyncFailure, isWorktreeCleanup, needsAttention,
  type AgentInputNotificationAction, type MeshSyncNotificationAction, type Notification, type SyncEvidence, type WorktreeNotificationAction } from '../../lib/notifications';
import { runMeshSync } from '../../lib/meshSyncFeedback';
import { refreshWorktreeNotifications } from '../../lib/worktreeNotifications';
import { ProbeTabBody } from './ProbeTabBody';
import { BellIcon } from './probeIcons';
import { AgentInputLink } from '../Notifications/AgentInputLink';
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

/** The layout every unresolved-problem card shares: a heading with its time, the
 *  thing that needs attention, the reason, its path, and a status line beside the
 *  one action that can move it. The two kinds differ only in what those slots
 *  say and in which action they offer, so the structure lives here once — a card
 *  whose reason does not wrap and a path that does not fit is the same bug in
 *  both kinds. */
function AttentionCard({ title, notice, subject, detail, path, status, action, testId }: {
  title: string;
  notice: Notification;
  subject: string;
  detail: string;
  path: string;
  status: string;
  action: ReactNode;
  testId?: string;
}) {
  return <article className="notification-attention-card" data-testid={testId}>
    <div className="notification-heading"><h4>{title}</h4><NoticeTime notice={notice} /></div>
    <div className="notification-node">{subject}</div>
    <p>{detail}</p>
    <code>{path}</code>
    <div className="notification-card-actions"><span>{status}</span>{action}</div>
  </article>;
}

/** What the card may assert, per what the record actually establishes. The
 *  cases are genuinely different claims, not shades of one: a fetched run that
 *  did not fast-forward knows the mesh is behind; `not-fetched` means the
 *  record came from a mesh with no remote, where nothing was ever fetched (only
 *  records written by earlier builds carry it, and it raises no card today);
 *  and everything else establishes nothing either way — a fetch that did not
 *  complete, a comparison that failed, an IPC that never returned — so the card
 *  says so instead of picking the more alarming reading. */
function syncStatusLine(evidence: SyncEvidence): string {
  switch (evidence) {
    case 'behind':
      return 'Behind upstream — the fetched commits were not applied';
    case 'not-fetched':
      return 'Upstream not checked — the fetch did not run';
    case 'unknown':
    default:
      // The `default` arm is load-bearing, not a formality: `evidence` is
      // read out of persisted JSON, so a record written by an older build can
      // arrive here with the field missing and TypeScript cannot see it. It
      // falls into the same reading as `unknown` — which is the point.
      return 'Upstream state unknown — this sync could not be verified';
  }
}

/** A mesh whose manual sync did not finish. Retrying from here is the point of
 *  putting it in this section: the sync runs through the same `runMeshSync` the
 *  sidebar uses, and a success resolves this very card. The local `pending`
 *  state disables the button for the duration of one IPC round trip. */
function SyncAttentionCard({ notice }: { notice: Notification & { action: MeshSyncNotificationAction } }) {
  const [pending, setPending] = useState(false);
  const action = notice.action;
  return <AttentionCard
    title="Sync from upstream incomplete"
    notice={notice}
    subject={action.meshName}
    detail={action.detail}
    path={action.path}
    status={pending ? 'Retrying…' : syncStatusLine(action.evidence)}
    testId={`sync-attention-${action.path}`}
    action={<button type="button" className="notification-resolve" disabled={pending}
      aria-label={`Retry sync for ${action.meshName}`}
      onClick={async () => {
        setPending(true);
        await runMeshSync({ name: action.meshName, path: action.path });
        setPending(false);
      }}>Retry sync <span aria-hidden="true">→</span></button>} />;
}

function WorktreeAttentionCard({ notice, onResolve }: { notice: Notification & { action: WorktreeNotificationAction }; onResolve: (n: Notification) => void }) {
  const action = notice.action;
  return <AttentionCard
    title="Worktree cleanup blocked"
    notice={notice}
    subject={action.nodeName}
    detail={action.detail}
    path={action.path}
    status="Queued for automatic retry"
    action={<button type="button" className="notification-resolve" aria-label={`Resolve worktree ${action.nodeName}`}
      onClick={() => onResolve(notice)}>Resolve <span aria-hidden="true">→</span></button>} />;
}

export async function loadNotifications() {
  await refreshWorktreeNotifications();
  return null;
}

export function NotificationsTab() {
  useProbeResource(PROBE_TAB_DEFINITIONS.notifications.load, { meshId: null }, 'host');
  const notices = useToastStore(s => s.notifications);
  const loading = useToastStore(s => s.cleanupLoading);
  const error = useToastStore(s => s.cleanupError);
  const [recoveryMounted, setRecoveryMounted] = useState(false);
  const attention = notices.filter(needsAttention);
  const activity = notices.filter(n => !needsAttention(n));
  const unread = notices.filter(n => !n.read).length;
  const clearable = activity.some(n => n.read);
  // Each recovery card receives only its own action fields.
  const cleanupAttention = attention.filter(
    (n): n is Notification & { action: WorktreeNotificationAction } => isWorktreeCleanup(n.action));
  const syncAttention = attention.filter(
    (n): n is Notification & { action: MeshSyncNotificationAction } => isMeshSyncFailure(n.action));
  const inputAttention = attention.filter(
    (n): n is Notification & { action: AgentInputNotificationAction } => isAgentInput(n.action));

  useEffect(() => {
    return () => useBlockedCleanupStore.getState().close();
  }, []);

  function resolve(notice: Notification) {
    if (!isWorktreeCleanup(notice.action)) return;
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
        <p>App toasts will appear here. Input requests and other problems will have an action.</p>
        {loading && <p role="status">Checking worktree problems…</p>}
      </div> : <>
        <section aria-label="Needs attention" className="notification-attention">
          <div className="notification-section-heading"><h3>Needs attention</h3><span>{attention.length}</span></div>
          {attention.length === 0 ? <p className="notification-caught-up">
            {error ? 'Worktree status is unavailable.' : loading ? 'Checking worktree problems…' : 'You’re all caught up.'}
          </p> : <>
            {inputAttention.map(notice => <article key={notice.id} className="notification-attention-card">
              <div className="notification-heading"><h4>Needs input</h4><NoticeTime notice={notice} /></div>
              <AgentInputLink action={notice.action} className="notification-input-link">
                <span className="block notification-node">{notice.action.agentName}</span>
                <span className="block mt-1 text-text-secondary">{notice.message}</span>
                <span className="block mt-2 text-accent-cyan">Open agent →</span>
              </AgentInputLink>
            </article>)}
            {syncAttention.map(notice => <SyncAttentionCard key={notice.id} notice={notice} />)}
            {cleanupAttention.map(notice => <WorktreeAttentionCard key={notice.id} notice={notice} onResolve={resolve} />)}
          </>}
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
                {isAgentInput(notice.action) && <AgentInputLink action={notice.action} className="notification-link" />}
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
