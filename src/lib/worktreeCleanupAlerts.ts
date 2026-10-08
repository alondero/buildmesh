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
// The store is imported dynamically. This module is reached from the
// `worktree-cleanup-failed` listener installed at boot, so a static import would
// put the whole blocked-cleanup state (and its API helpers) into the initial
// bundle every boot pays for — and the #1568 bundle budget fails on far less.
// The store is only loaded once a cleanup actually blocks.

import type { WorktreeCleanupFailedPayload } from '../types/generated/WorktreeCleanupFailedPayload';
import { addToast } from '../stores/toastStore';

export async function reportBlockedWorktreeCleanup(
  payload: WorktreeCleanupFailedPayload,
): Promise<void> {
  addToast(
    'Worktree',
    `Couldn't remove the worktree for ${payload.node_name} (${payload.worktree_path}) — ` +
      `${payload.operation.replace(/-/g, ' ')} failed: ${payload.error}`,
    'warning',
  );
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

/** The toast text for one blocked cleanup, split out so the wording is pinned
 *  by a test rather than living inside an event handler. */
export function blockedCleanupToastMessage(payload: WorktreeCleanupFailedPayload): string {
  return (
    `Couldn't remove the worktree for ${payload.node_name} (${payload.worktree_path}) — ` +
    `${payload.operation.replace(/-/g, ' ')} failed: ${payload.error}`
  );
}
