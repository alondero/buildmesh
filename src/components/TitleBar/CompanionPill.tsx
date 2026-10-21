import { useEffect } from 'react';
import { companionRunning, initializeCompanion, useCompanionStore } from '../../stores/companionStore';
import { useUIStore } from '../../stores/uiStore';
import { formatError } from '../../lib/errorUtils';

export function CompanionPill() {
  const task = useCompanionStore(s => s.task);
  useEffect(() => { void initializeCompanion().catch(error => useCompanionStore.setState({ error: formatError(error) })); }, []);
  if (!task) return null;
  return <div className="flex items-center gap-2 text-xs text-accent-violet">
    <button type="button" onClick={() => {
      useCompanionStore.setState({ visible: true });
      useUIStore.getState().openOmnibar('files');
    }}>✦ Companion · {companionRunning(task) ? `${task.steps.length} steps` : task.status}</button>
    {companionRunning(task) && <button type="button" aria-label="Cancel Companion task" onClick={() => void useCompanionStore.getState().cancel()}>×</button>}
  </div>;
}
