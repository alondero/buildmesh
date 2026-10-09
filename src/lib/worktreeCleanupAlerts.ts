// What to do when a background worktree removal cannot complete (issue #2139).
//
// Two surfaces, in order:
//
//   1. an immediate toast that names the node, the folder and the reason — the
//      old toast omitted the path and the error, which is what left the user
//      with nothing to act on;
//   2. the blocked-cleanup dialog, which is the actionable surface (copy path,
//      copy diagnostics, diagnose, retry, keep worktree).
//
// This whole module is imported dynamically by the `worktree-cleanup-failed`
// listener, so a boot with nothing blocked loads neither the diagnostics
// formatters it uses for the toast wording nor the store it opens the dialog
// through — the #1568 bundle budget fails on far less than that (issue #2139
// review round 1).

import type { WorktreeCleanupFailedPayload } from '../types/generated/WorktreeCleanupFailedPayload';
import { operationLabel } from './worktreeCleanupDiagnostics';
import { addToast } from '../stores/toastStore';

/** The toast text for one blocked cleanup. The single source for the wording,
 *  so the test pins what production actually shows rather than a parallel
 *  helper nothing calls (issue #2139 review round 1). */
export function blockedCleanupToastMessage(payload: WorktreeCleanupFailedPayload): string {
  return (
    `Couldn't remove the worktree for ${payload.node_name} (${payload.worktree_path}) — ` +
    `${operationLabel(payload.operation)} failed: ${payload.error}`
  );
}

export async function reportBlockedWorktreeCleanup(
  payload: WorktreeCleanupFailedPayload,
): Promise<void> {
  addToast('Worktree', blockedCleanupToastMessage(payload), 'warning');
  try {
    const { openBlockedCleanups } = await import('../stores/blockedCleanupStore');
    openBlockedCleanups();
  } catch (error) {
    // The toast already told the user what failed and that it stays queued; a
    // failure to load the dialog must not turn into a second, duplicate error.
    addToast(
      'Worktree',
      `Couldn't open the blocked worktree cleanups: ${String(error)}`,
      'error',
    );
  }
}
