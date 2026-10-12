// Failure events record the problem; recovery opens only after explicit Resolve.

import type { WorktreeCleanupFailedPayload } from '../types/generated/WorktreeCleanupFailedPayload';
import { operationLabel } from './worktreeCleanupDiagnostics';
import { addToast } from '../stores/toastStore';

/** The toast text for one blocked cleanup. The single source for the wording,
 *  so the test pins what production actually shows rather than a parallel
 *  helper nothing calls (issue #2139 review round 1). */
export function blockedCleanupToastMessage(
  payload: Pick<WorktreeCleanupFailedPayload, 'node_name' | 'worktree_path' | 'error'> & { operation: string | null },
): string {
  return (
    `Couldn't remove the worktree for ${payload.node_name} (${payload.worktree_path}) — ` +
    `${operationLabel(payload.operation)} failed: ${payload.error}`
  );
}

export function reportBlockedWorktreeCleanup(
  payload: WorktreeCleanupFailedPayload,
): void {
  addToast('Worktree', blockedCleanupToastMessage(payload), 'warning', {
    kind: 'worktree-cleanup', path: payload.worktree_path, nodeName: payload.node_name,
    detail: `${operationLabel(payload.operation)} failed: ${payload.error}`,
  });
}
