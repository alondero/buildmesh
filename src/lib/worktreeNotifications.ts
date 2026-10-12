import { listPendingWorktreeRemovals } from './tauri';
import { operationLabel } from './worktreeCleanupDiagnostics';
import { blockedCleanupToastMessage } from './worktreeCleanupAlerts';
import { useToastStore } from '../stores/toastStore';
import type { PendingWorktreeRemoval } from '../types/generated/PendingWorktreeRemoval';
import type { WorktreeNotice } from './notifications';

export function worktreeNotices(entries: PendingWorktreeRemoval[]): WorktreeNotice[] {
  return entries.filter(e => e.attempt_count > 0 && e.last_error !== null).map(e => ({
    path: e.worktree_path,
    nodeName: e.node_name,
    message: blockedCleanupToastMessage({ node_name: e.node_name, worktree_path: e.worktree_path, operation: e.last_operation, error: e.last_error! }),
    detail: `${operationLabel(e.last_operation)} failed: ${e.last_error}`,
    createdAt: e.last_attempt_at,
  }));
}

let request = 0;

// Resolve and background refresh share one owner for notification reconciliation.
export async function readWorktreeNotifications(): Promise<PendingWorktreeRemoval[]> {
  const owner = ++request;
  const revision = useToastStore.getState().worktreeRevision;
  useToastStore.setState({ cleanupLoading: true });
  try {
    const entries = await listPendingWorktreeRemovals();
    if (!Array.isArray(entries)) throw new Error('Invalid cleanup queue response');
    if (owner === request) {
      useToastStore.getState().reconcileWorktrees(worktreeNotices(entries), revision);
      useToastStore.setState({ cleanupLoading: false, cleanupError: null });
    }
    return entries;
  } catch (error) {
    if (owner === request) {
      useToastStore.setState({
        cleanupLoading: false,
        cleanupError: `Couldn't refresh worktree problems: ${String(error)}`,
      });
    }
    throw error;
  }
}

export async function refreshWorktreeNotifications(): Promise<void> {
  try { await readWorktreeNotifications(); } catch { /* The reader retains state and exposes the retryable error. */ }
}

export function watchWorktreeNotifications(): () => void {
  void refreshWorktreeNotifications();
  const timer = window.setInterval(() => void refreshWorktreeNotifications(), 30_000);
  return () => {
    window.clearInterval(timer);
    ++request;
    useToastStore.setState({ cleanupLoading: false });
  };
}
