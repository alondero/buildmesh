// Pure formatters for the blocked worktree-cleanup surface (issue #2139).
//
// The dialog needs two shapes of the same evidence: a machine-copyable block
// ("Copy diagnostics" — what the user pastes into a bug report or a chat with
// support) and human-readable fragments (the operation label, when the last
// attempt ran, how long the backoff still has). Both are derived from the
// persisted queue row, so a test can pin them from a record alone — no Tauri
// backend, no clock.

import type { PendingWorktreeRemoval } from '../types/generated/PendingWorktreeRemoval';
import type { BlockingProcess } from '../types/generated/BlockingProcess';

/** Human-readable labels for the `git::worktree` operation vocabulary.
 *  The backend persists a stable identifier so a UI change can't make old
 *  rows unreadable; this map is the only place that vocabulary is translated.
 *  An unknown operation still renders (it shows the raw id) — a new step on
 *  the Rust side must not blank the dialog. */
const OPERATION_LABELS: Record<string, string> = {
  'open-worktree-repository': 'Opening the worktree as a repository',
  'open-worktree-admin-entry': "Reading the worktree's git entry",
  'rename-worktree-to-staging': 'Moving the folder aside to remove it',
  'delete-staged-worktree': 'Deleting the moved-aside folder',
  'prune-worktree-admin-entry': 'Cleaning up the git worktree entry',
};

const NEVER_ATTEMPTED = 0;

export function operationLabel(operation: string | null): string {
  if (!operation) return 'No removal attempt has run yet';
  return OPERATION_LABELS[operation] ?? operation;
}

/** When the last attempt ran, as a relative age. Returns a neutral string for
 *  a row that has never been attempted (timestamp 0). */
export function lastAttemptLabel(record: PendingWorktreeRemoval, nowMs: number): string {
  if (record.last_attempt_at === NEVER_ATTEMPTED) return 'never attempted';
  return `${new Date(record.last_attempt_at).toISOString()} (${ageLabel(record.last_attempt_at, nowMs)})`;
}

/** How long the automatic drain will wait before it may try again. */
export function nextRetryLabel(record: PendingWorktreeRemoval, nowMs: number): string {
  if (record.last_attempt_at === NEVER_ATTEMPTED || record.retry_not_before <= 0) {
    return 'on the next launch or close';
  }
  if (record.retry_not_before <= nowMs) return 'due now';
  const seconds = Math.ceil((record.retry_not_before - nowMs) / 1000);
  return `backing off for another ${formatSeconds(seconds)}`;
}

/** The full diagnostics block for "Copy diagnostics". Every line names
 *  something the user (or a maintainer) needs: which node, which folder, which
 *  step, what the OS said, how often it has been tried, and what is pinning it. */
export function formatCleanupDiagnostics(
  record: PendingWorktreeRemoval,
  blockers: BlockingProcess[] = [],
): string {
  const lines = [
    `Worktree cleanup blocked`,
    `Node: ${record.node_name}`,
    `Worktree path: ${record.worktree_path}`,
    `Failed operation: ${record.last_operation ?? 'none'}`,
    `Last error: ${record.last_error ?? 'none'}`,
    `Attempts: ${record.attempt_count}`,
    `Last attempt: ${record.last_attempt_at === NEVER_ATTEMPTED ? 'never' : new Date(record.last_attempt_at).toISOString()}`,
    `Automatic retry: ${record.retry_not_before <= 0 ? 'not scheduled' : new Date(record.retry_not_before).toISOString()}`,
  ];
  if (blockers.length === 0) {
    lines.push('Processes holding the folder: not diagnosed yet (use "What is holding it?")');
  } else {
    lines.push(`Processes holding the folder (${blockers.length}):`);
    for (const blocker of blockers) {
      const who = blocker.name ?? 'unknown';
      lines.push(`  - ${who} (pid ${blocker.pid}) — ${blocker.reason}: ${blocker.detail}`);
    }
  }
  return lines.join('\n');
}

/// Relative-age label without depending on the Date object shape the shared
/// `formatRelativeAge` helper takes — this one works on epoch milliseconds
/// because that is what the persisted rows carry.
function ageLabel(thenMs: number, nowMs: number): string {
  const seconds = Math.max(0, Math.floor((nowMs - thenMs) / 1000));
  if (seconds < 60) return 'just now';
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

function formatSeconds(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  return `${Math.floor(minutes / 60)}h`;
}
