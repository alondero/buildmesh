// Feedback for a user-triggered mesh sync (sidebar "Force sync from upstream"
// and the omnibar's Git sync command).
//
// The `git_sync` command has always returned a result describing what happened;
// nothing read it. `MeshItem` awaited the promise and dropped the value, so a
// fetch that failed and a pull that fast-forwarded 40 commits were the same
// experience: a spinner that stopped, and a behind-count badge that either
// stayed or silently moved. This module is the one place that turns a
// `GitSyncStatus` into the two things the user can actually see: a toast, and
// (when the mesh was left behind) a "Needs attention" notification.

import { addToast, useToastStore } from '../stores/toastStore';
import { gitSync } from './tauri';
import type { GitSyncResult } from '../types/generated/GitSyncResult';
import type { GitSyncStatus } from '../types/generated/GitSyncStatus';
import type { MeshSyncNotificationAction } from './notifications';
import type { ToastSeverity } from './toastUtils';

/** Toast/notification provider label. Matches the omnibar command's wording
 *  so the two paths are visibly the same subsystem in the notification list. */
export const SYNC_PROVIDER = 'Git sync';

/** What the surface should do about one sync result. */
export interface SyncFeedback {
  severity: ToastSeverity;
  /** The mesh did not end up in sync with upstream — this is the predicate
   *  behind a "Needs attention" item. A `Skipped` sync is deliberately not
   *  unresolved: a Mesh with no remote is a valid local-only setup, and
   *  nagging about it would train the user to ignore the section. */
  unresolved: boolean;
}

const FEEDBACK: Record<GitSyncStatus, SyncFeedback> = {
  synced: { severity: 'success', unresolved: false },
  up_to_date: { severity: 'info', unresolved: false },
  skipped: { severity: 'info', unresolved: false },
  partial: { severity: 'warning', unresolved: true },
  failed: { severity: 'error', unresolved: true },
};

/** Classifies a sync result. Pure, so the severity table is pinned by tests
 *  rather than by whichever entry point happens to be exercised.
 *
 *  A status this build has no entry for means the app and the backend are out
 *  of step (the wire type is generated and drift-gated, so it cannot happen
 *  within one build). Classifying it as a failure is the safe reading: the
 *  mesh demonstrably did not sync, so the user is told, rather than the
 *  sidebar crashing on `undefined.severity` and telling them nothing. */
export function classifySyncResult(result: GitSyncResult): SyncFeedback {
  return FEEDBACK[result.status] ?? FEEDBACK.failed;
}

/** A mesh identified well enough to report a sync against: the sidebar row and
 *  the omnibar both have a name and the DB-canonical path the sync keys on. */
export interface SyncTarget {
  name: string;
  path: string;
}

function syncAction(target: SyncTarget, detail: string): MeshSyncNotificationAction {
  return { kind: 'mesh-sync-failure', path: target.path, meshName: target.name, detail };
}

/** Record a sync that left the mesh behind. The detail is the backend's own
 *  wording, never a UI reconstruction — the reason a pull was skipped (a dirty
 *  path, diverged history, a timeout) is the thing the user cannot otherwise
 *  see, and it is only known on the Rust side. `severity` is the caller's
 *  classification, so a bulk sync reports each mesh at its own level. */
export function raiseSyncAttention(target: SyncTarget, detail: string, severity: ToastSeverity = 'warning'): void {
  addToast(SYNC_PROVIDER, `${target.name}: ${detail}`, severity, syncAction(target, detail));
}

/** Report one manual sync: always a toast, plus a "Needs attention" item when
 *  the mesh was left behind. Returns the classification so a caller can react
 *  to whether the mesh ended up in sync. */
export function reportMeshSync(target: SyncTarget, result: GitSyncResult): SyncFeedback {
  const feedback = classifySyncResult(result);
  if (feedback.unresolved) {
    raiseSyncAttention(target, result.message, feedback.severity);
    return feedback;
  }
  addToast(SYNC_PROVIDER, `${target.name}: ${result.message}`, feedback.severity);
  // A sync that just succeeded is the resolution of any earlier failure for
  // the same mesh. Without this the "Needs attention" card would outlive the
  // problem it describes — it is only ever cleared by a later good sync or by
  // the user dismissing it.
  useToastStore.getState().resolveMeshSyncNotification(target.path, 'Synced successfully.');
  return feedback;
}

/** Report a sync that never produced a result — the IPC rejected (the command
 *  could not even open the repo) or the promise threw. Treated exactly like a
 *  failed sync: the mesh is still behind and the user is still owed the reason. */
export function reportSyncError(target: SyncTarget, error: unknown): SyncFeedback {
  const detail = error instanceof Error ? error.message : String(error);
  addToast(SYNC_PROVIDER, `${target.name}: Sync failed: ${detail}`, 'error',
    syncAction(target, detail));
  return { severity: 'error', unresolved: true };
}

/** Run a manual sync and report it. The single entry point behind the sidebar's
 *  "Force sync from upstream" and the notification card's "Retry sync", so the
 *  two cannot drift apart on severity, wording or resolution.
 *
 *  `completed` distinguishes "the backend answered and it did not go well"
 *  from "the IPC itself failed": only the former means the fetch ran, so only
 *  the former is worth re-reading the behind count for. */
export async function runMeshSync(target: SyncTarget): Promise<{ feedback: SyncFeedback; completed: boolean }> {
  try {
    const result = await gitSync(target.path);
    return { feedback: reportMeshSync(target, result), completed: true };
  } catch (error) {
    return { feedback: reportSyncError(target, error), completed: false };
  }
}