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
import type { MeshSyncNotificationAction, SyncEvidence } from './notifications';
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
  // No remote at all: nothing was ever fetched, and nothing is wrong.
  skipped: { severity: 'info', unresolved: false, verified: false },
  // A remote was fetched but the branch tracks nothing, so no comparison ran.
  // Also nothing wrong — and also not evidence the mesh is up to date.
  no_upstream: { severity: 'info', unresolved: false, verified: false },
  partial: { severity: 'warning', unresolved: true, verified: false },
  // Fetched, but the backend never established where this mesh stands. It is
  // a problem — the mesh may be arbitrarily far behind — and it is emphatically
  // not a verified success, so it must not resolve a card or clear a badge.
  unverified: { severity: 'warning', unresolved: true, verified: false },
  // The fetch was attempted and did not complete. `fetched: false` here means
  // "did not finish", NOT "was never tried" — `git fetch` had already been
  // launched and may have contacted upstream before failing.
  failed: { severity: 'error', unresolved: true, verified: false },
};

/** What a stored failure record may claim about the mesh's position. Total by
 *  construction: any status this build does not recognise falls to `unknown`
 *  rather than defaulting to a claim.
 *
 *  The single thing this must not do is treat `fetched: false` as "no fetch
 *  was attempted". That flag means the fetch did not *complete*, and Rust
 *  returns `failed` after launching `git fetch` — so a network failure proves
 *  nothing about whether upstream was contacted. Only `skipped` (no remote
 *  configured) proves nothing was ever fetched. */
export function evidenceOf(result: GitSyncResult): SyncEvidence {
  switch (result.status) {
    // Fetched, commits arrived, the fast-forward did not apply: the mesh is
    // behind, and that much is established.
    case 'partial':
      return 'behind';
    // The one case where "nothing was fetched" is a fact rather than an
    // inference: there is no remote to fetch from.
    case 'skipped':
      return 'not-fetched';
    // `no_upstream` fetched but nobody compared it; `unverified` fetched and
    // the comparison failed; `failed` attempted a fetch that did not complete.
    // An unrecognised status is not this build's to interpret. None of them
    // supports a claim about the mesh's position.
    default:
      return 'unknown';
  }
}

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

function syncAction(target: SyncTarget, detail: string, evidence: SyncEvidence): MeshSyncNotificationAction {
  return { kind: 'mesh-sync-failure', path: target.path, meshName: target.name, detail, evidence };
}

/** How loudly to report. A command that syncs N meshes reports the problems in
 *  Needs attention but shows ONE summary toast, so it records its per-mesh
 *  problems silently rather than putting N entries on a three-deep stack where
 *  the last ones would be invisible anyway. */
export interface ReportOptions {
  silent?: boolean;
}

/** Record a sync that left the mesh behind. Takes the result rather than a
 *  pre-digested message and severity: the card's detail is the backend's own
 *  wording (the reason a pull was skipped — a dirty path, diverged history, a
 *  timeout — is only known on the Rust side), and its severity and
 *  `refsFetched` both derive from that same result, so passing them separately
 *  would only create a way for the wording and the state claim to disagree. */
export function raiseSyncAttention(target: SyncTarget, result: GitSyncResult, options?: ReportOptions): void {
  addToast(SYNC_PROVIDER, `${target.name}: ${result.message}`, classifySyncResult(result).severity,
    syncAction(target, result.message, evidenceOf(result)), options);
}

/** Publish what one completed sync did to a mesh, for rows to react to. Two
 *  effects in one place, because both belong to the same fact and every entry
 *  point must produce both:
 *  - resolves an earlier "Needs attention" item when the mesh was *verified*,
 *    so a fixed mesh does not keep a card describing a problem it no longer has;
 *  - publishes the outcome so a sidebar row clears its own failure badge and
 *    re-reads the cached behind-count even though it did not run the sync.
 *
 *  A result that neither verified nor fetched publishes nothing, so every row
 *  keeps what it already had: a `skipped` mesh, a `failed` result that came back
 *  with `fetched: false` (the fetch failed, or the path was not a usable
 *  repository — a completed command result, not an error), and an IPC that never
 *  reached the backend all land here. Those still raise a toast and a
 *  Needs-attention card; they simply changed nothing for another mesh's row.
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
    raiseSyncAttention(target, result);
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
    syncAction(target, detail, 'unknown'), options);
  return { severity: 'error', unresolved: true, verified: false };
}

/** Run a manual sync and report it. The single entry point behind the sidebar's
 *  "Force sync from upstream" and the notification card's "Retry sync", so the
 *  two cannot drift apart on severity, wording or resolution.
 *
 *  `answered` means only that the backend returned a `GitSyncResult` — NOT that
 *  the fetch ran. A `failed` or `skipped` result comes back having contacted
 *  nothing. It is the right signal for "re-read the row's cached branch
 *  status": a returned result means the refs may have moved (and the
 *  row-refresh trigger for a fetched-but-not-applied pull rides on the
 *  published outcome), while a thrown IPC means nothing could have. Callers
 *  needing the fetch fact itself read it off the result's own `fetched` flag
 *  via `outcomeOf`. */
export async function runMeshSync(target: SyncTarget): Promise<{ feedback: SyncFeedback; answered: boolean }> {
  try {
    const result = await gitSync(target.path);
    return { feedback: reportMeshSync(target, result), answered: true };
  } catch (error) {
    return { feedback: reportSyncError(target, error), answered: false };
  }
}