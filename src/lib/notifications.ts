import type { Toast } from '../lib/toastUtils';

export interface WorktreeNotificationAction {
  kind: 'worktree-cleanup';
  path: string;
  nodeName: string;
  detail: string;
  resolution?: string;
}

/** How much a stored sync failure supports the card claiming.
 *
 *  Deliberately not the same fact as `SyncOutcome.refsFetched` (which says
 *  whether the remote-tracking ref moved, and drives a row's behind-count
 *  refresh). A ref can move without anything being learned about the mesh:
 *  a fetch that succeeds but whose comparison fails leaves the position
 *  unknown, and a record written before this distinction existed cannot say.
 *  Both are `unknown`, and the card says so rather than asserting either.
 */
export type SyncEvidence =
  /** The fetch ran and the fast-forward did not apply: the mesh is behind. */
  | 'behind'
  /** The result proves no fetch ran, so upstream was never contacted. */
  | 'not-fetched'
  /** Nothing in the stored record establishes whether a fetch ran. */
  | 'unknown';

/** A manual mesh sync that did not leave the mesh in sync with upstream —
 *  either the fetch failed or the commits arrived but the working-tree
 *  fast-forward did not apply. Carries the mesh's identity so the card names
 *  which mesh is behind instead of only reporting a git message, and the
 *  backend's own wording as `detail` so the surface never invents a reason. */
export interface MeshSyncNotificationAction {
  kind: 'mesh-sync-failure';
  /** The Mesh's DB-canonical path (the key both sync entry points sync on). */
  path: string;
  meshName: string;
  detail: string;
  /** What the card may claim. Records persisted before this field existed
   *  restore as `unknown`: the record does not say, so the card must not. */
  evidence: SyncEvidence;
  resolution?: string;
}

export type NotificationAction = WorktreeNotificationAction | MeshSyncNotificationAction;

/** The worktree cleanup queue is the only thing that can resolve a cleanup
 *  notice on its own (ADR-0043: the durable queue is the source of truth, and
 *  a row that leaves the queue means the problem ended). A sync failure has no
 *  such queue — it is resolved by a later successful sync or by the user — so
 *  the worktree reconciliation must leave it strictly alone. Without this
 *  predicate, the 30-second poll would mark every sync failure
 *  "No longer awaiting cleanup." within half a minute of it being reported. */
export const isWorktreeCleanup = (
  action: NotificationAction | undefined,
): action is WorktreeNotificationAction => action?.kind === 'worktree-cleanup';

/** The counterpart guard. Together the two partition `NotificationAction`, so
 *  a surface can branch exhaustively and TypeScript proves the sync card is
 *  never handed a cleanup action (whose fields it cannot render). */
export const isMeshSyncFailure = (
  action: NotificationAction | undefined,
): action is MeshSyncNotificationAction => action?.kind === 'mesh-sync-failure';

export interface Notification extends Toast {
  read: boolean;
  action?: NotificationAction;
}

export interface WorktreeNotice {
  path: string;
  nodeName: string;
  message: string;
  detail: string;
  createdAt: number;
}

export const NOTIFICATION_HISTORY_LIMIT = 100;
/** An unresolved problem the user is expected to act on: a blocked worktree
 *  cleanup or a mesh sync that did not complete. Resolution text moves a
 *  notice out of this set and back into ordinary history. */
export const needsAttention = (notice: Notification): boolean =>
  notice.action !== undefined && !notice.action.resolution;

export function limitNotifications(notices: Notification[]): Notification[] {
  let activity = 0;
  return [...notices].sort((a, b) => b.createdAt - a.createdAt)
    .filter(n => needsAttention(n) || ++activity <= NOTIFICATION_HISTORY_LIMIT);
}

/** Structurally validates one persisted action. Returns the narrowed action,
 *  or `null` for a shape we don't recognise — a record written by a newer
 *  build (or a corrupted one) must not be resurrected into the Needs-attention
 *  list as a card that renders nothing. */
function restoreAction(value: unknown): NotificationAction | null {
  if (value === undefined || value === null || typeof value !== 'object') return null;
  const a = value as Record<string, unknown>;
  const resolutionOk = a.resolution === undefined || typeof a.resolution === 'string';
  if (!resolutionOk) return null;
  if (a.kind === 'worktree-cleanup') {
    return typeof a.path === 'string' && typeof a.nodeName === 'string'
      && typeof a.detail === 'string'
      ? { kind: 'worktree-cleanup', path: a.path, nodeName: a.nodeName, detail: a.detail, resolution: a.resolution as string | undefined }
      : null;
  }
  if (a.kind === 'mesh-sync-failure') {
    // A record written before `evidence` existed (and one written by a build
    // whose record we cannot interpret) restores as `unknown`. It may well
    // have been a fetched partial, so `behind` would claim lag that nothing
    // supports, and `not-fetched` would claim the fetch was skipped — also
    // unsupported. Dropping the record instead would lose an unresolved
    // problem the user still has.
    const evidence = a.evidence === 'behind' || a.evidence === 'not-fetched' ? a.evidence : 'unknown';
    return typeof a.path === 'string' && typeof a.meshName === 'string' && typeof a.detail === 'string'
      ? { kind: 'mesh-sync-failure', path: a.path, meshName: a.meshName, detail: a.detail, evidence, resolution: a.resolution as string | undefined }
      : null;
  }
  return null;
}

export function restoreNotifications(value: unknown): Notification[] {
  if (!Array.isArray(value)) return [];
  const notices: Notification[] = [];
  for (const entry of value) {
    if (!entry || typeof entry !== 'object') continue;
    const n = entry as Record<string, unknown>;
    if (!Number.isSafeInteger(n.id)
      || typeof n.provider !== 'string' || typeof n.message !== 'string'
      || typeof n.read !== 'boolean' || !Number.isFinite(n.createdAt) || Math.abs(n.createdAt as number) > 8.64e15
      || !['error', 'warning', 'success', 'info', undefined].includes(n.severity as string | undefined)) continue;
    // An action shape we don't recognise (an older/newer build, or a
    // corrupted record) rejects the whole notification: keeping it would put
    // a card in Needs attention that renders no path and no action.
    let action: NotificationAction | undefined;
    if (n.action !== undefined) {
      const restored = restoreAction(n.action);
      if (!restored) continue;
      action = restored;
    }
    notices.push({
      id: n.id as number,
      provider: n.provider,
      message: n.message,
      createdAt: n.createdAt as number,
      severity: n.severity as Toast['severity'],
      read: n.read as boolean,
      action,
    });
  }
  return limitNotifications(notices);
}