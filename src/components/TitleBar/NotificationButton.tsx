import { useUIStore } from '../../stores/uiStore';
import { useToastStore } from '../../stores/toastStore';
import { needsAttention } from '../../lib/notifications';
import { BellIcon } from '../Probe/probeIcons';

export function NotificationButton() {
  const open = useUIStore(s => s.probeOpen && s.probeTab === 'notifications');
  const unread = useToastStore(s => s.notifications.filter(n => !n.read).length);
  const attention = useToastStore(s => s.notifications.filter(needsAttention).length);
  const count = attention || unread;
  return <button type="button" aria-label="Open Notifications" aria-expanded={open}
    aria-description={`${unread} unread; ${attention} need attention`}
    aria-controls={open ? 'probe-tab-panel' : undefined}
    title={`Notifications · ${attention} need attention · ${unread} unread`}
    className={`relative flex h-9 w-8 shrink-0 items-center justify-center rounded-md transition-colors focus-visible:outline-2 focus-visible:outline-accent-cyan ${open ? 'bg-bg-card text-accent-cyan' : 'text-text-secondary hover:bg-bg-card hover:text-text-primary'}`}
    onClick={() => {
      const ui = useUIStore.getState();
      if (ui.probeOpen && ui.probeTab === 'notifications') ui.toggleProbe();
      else ui.openProbeTab('notifications');
    }}>
    <BellIcon className="h-4 w-4 shrink-0" />
    {count > 0 && <span aria-hidden="true" className={`absolute -top-0.5 -right-0.5 min-w-3.5 rounded-sm px-0.5 text-center text-2xs font-semibold leading-3.5 text-text-primary ${attention ? 'bg-status-warning-bg border border-accent-amber' : 'bg-bg-selection border border-accent-cyan'}`}>
      {count > 99 ? '99+' : count}
    </span>}
    <span className="sr-only">{unread} unread; {attention} need attention</span>
  </button>;
}
