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
import { create } from 'zustand';
import { gitSync } from './tauri';
import type { GitSyncResult } from '../types/generated/GitSyncResult';
import type { GitSyncStatus } from '../types/generated/GitSyncStatus';
import type { MeshSyncNotificationAction } from './notifications';
import type { ToastSeverity } from './toastUtils';

/** Per-mesh record of what the last completed sync did, with a counter so a
 *  repeat is a new event rather than an equal one.
 *
 *  The sidebar row's failure badge and cached behind-count are component-local,
 *  and a sync can be run from two places the row knows nothing about — this
 *  probe's Retry button, and the omnibar's bulk command — so what a sync did has
 *  to be published for rows to react to.
 *
 *  The two signals are separate because they end different things. `verified`
 *  ends the problem: it clears the failure badge and resolves the card. A
 *  `Partial` result verifies nothing but still fetches, so the remote-tracking
 *  ref the behind-count is measured against has moved and the count is stale
 *  even though nothing was resolved — the original reported symptom (commits
 *  arrive, the working tree does not move, the marker does not budge). */
interface MeshSyncCompletion {
  revision: number;
  /** The mesh is confirmed in sync with upstream. */
  verified: boolean;
  /** The fetch ran, so the remote-tracking ref may have moved. */
  refsFetched: boolean;
}

/** What one completed sync did to a mesh. */
export interface SyncOutcome {
  verified: boolean;
  refsFetched: boolean;
}

interface MeshSyncCompletionState {
  completions: Record<string, MeshSyncCompletion>;
  noteCompletion: (path: string, outcome: SyncOutcome) => void;
}

export const useMeshSyncCompletion = create<MeshSyncCompletionState>()(set => ({
  completions: {},
  noteCompletion: (path, outcome) => set(s => ({
    completions: { ...s.completions, [path]: { revision: (s.completions[path]?.revision ?? 0) + 1, ...outcome } },
  })),
}));

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
  /** The backend actually established that the mesh is in sync. `skipped` is
   *  neither unresolved nor verified: nothing was checked, so an earlier
   *  failure must NOT be marked resolved by it, and a summary that counted it
   *  as synced would claim a result the command never obtained. */
  verified: boolean;
}

const FEEDBACK: Record<GitSyncStatus, SyncFeedback> = {
  synced: { severity: 'success', unresolved: false, verified: true },
  up_to_date: { severity: 'info', unresolved: false, verified: true },
  skipped: { severity: 'info', unresolved: false, verified: false },
  partial: { severity: 'warning', unresolved: true, verified: false },
  failed: { severity: 'error', unresolved: true, verified: false },
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

/** How loudly to report. A command that syncs N meshes reports the problems in
 *  Needs attention but shows ONE summary toast, so it records its per-mesh
 *  problems silently rather than putting N entries on a three-deep stack where
 *  the last ones would be invisible anyway. */
export interface ReportOptions {
  silent?: boolean;
}

/** Record a sync that left the mesh behind. The detail is the backend's own
 *  wording, never a UI reconstruction — the reason a pull was skipped (a dirty
 *  path, diverged history, a timeout) is the thing the user cannot otherwise
 *  see, and it is only known on the Rust side. `severity` is the caller's
 *  classification, so a bulk sync reports each mesh at its own level. */
export function raiseSyncAttention(target: SyncTarget, detail: string, severity: ToastSeverity = 'warning', options?: ReportOptions): void {
  addToast(SYNC_PROVIDER, `${target.name}: ${detail}`, severity, syncAction(target, detail), options);
}

/** Publish what one completed sync did to a mesh, for rows to react to. Two
 *  effects in one place, because both belong to the same fact and every entry
 *  point must produce both:
 *  - resolves an earlier "Needs attention" item when the mesh was *verified*,
 *    so a fixed mesh does not keep a card describing a problem it no longer has;
 *  - publishes the outcome so a sidebar row clears its own failure badge and
 *    re-reads the cached behind-count even though it did not run the sync.
 *
 *  A result that neither verified nor fetched (a skipped mesh, an IPC that
 *  never reached the backend) publishes nothing: nothing changed for any row.
 *  `refsFetched` is read from the backend's own `fetched` flag rather than
 *  inferred from the status, because a `Partial` run fetches without
 *  fast-forwarding and that is exactly the case whose behind-count goes stale. */
export function noteSyncOutcome(target: SyncTarget, outcome: SyncOutcome): void {
  if (!outcome.verified && !outcome.refsFetched) return;
  if (outcome.verified) {
    useToastStore.getState().resolveMeshSyncNotification(target.path, 'Synced successfully.');
  }
  useMeshSyncCompletion.getState().noteCompletion(target.path, outcome);
}

/** The outcome of a `git_sync` result, in the two signals rows react to. */
export function outcomeOf(result: GitSyncResult): SyncOutcome {
  const feedback = classifySyncResult(result);
  return { verified: feedback.verified, refsFetched: result.fetched };
}

/** Report one manual sync: always a toast, plus a "Needs attention" item when
 *  the mesh was left behind. Returns the classification so a caller can react
 *  to whether the mesh ended up in sync. */
export function reportMeshSync(target: SyncTarget, result: GitSyncResult): SyncFeedback {
  const feedback = classifySyncResult(result);
  if (feedback.unresolved) {
    raiseSyncAttention(target, result.message, feedback.severity);
  } else {
    addToast(SYNC_PROVIDER, `${target.name}: ${result.message}`, feedback.severity);
  }
  // Publishes both what the run verified and whether it moved the refs, so a
  // row re-reads its behind-count for a fetched-but-not-fast-forwarded result
  // and only clears its badge for a verified one.
  noteSyncOutcome(target, outcomeOf(result));
  return feedback;
}

/** Report a sync that never produced a result — the IPC rejected (the command
 *  could not even open the repo) or the promise threw. Treated exactly like a
 *  failed sync: the mesh is still behind and the user is still owed the reason. */
export function reportSyncError(target: SyncTarget, error: unknown, options?: ReportOptions): SyncFeedback {
  const detail = error instanceof Error ? error.message : String(error);
  addToast(SYNC_PROVIDER, `${target.name}: Sync failed: ${detail}`, 'error',
    syncAction(target, detail), options);
  return { severity: 'error', unresolved: true, verified: false };
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