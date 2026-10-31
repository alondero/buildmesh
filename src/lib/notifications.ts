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
 *  Deliberately not the same fact as `SyncOutcome.refsMayHaveChanged` (the
 *  pessimistic signal that drives a row's behind-count refresh). The two can
 *  disagree in both directions: a completed fetch may have moved no ref at all,
 * and an unusable repository warrants the refresh without any fetch having
 * run. What this records is narrower — whether the *outcome* establishes where
 *  the mesh stands — and only a fetched fast-forward that did not apply does.
 *  Everything else is `unknown`, and the card says so rather than asserting.
 */
export type SyncEvidence =
  /** The fetch ran and the fast-forward did not apply: the mesh is behind. */
  | 'behind'
  /** Nothing was ever fetched, because there is no remote to fetch from.
   *  Only a mesh with no remote can carry this, and that outcome raises no
   *  card at all — so in practice it survives on records written by earlier
   *  builds, and the card still has to render it. */
  | 'not-fetched'
  /** Nothing in the stored record establishes the mesh's position. */
  | 'unknown';

/** A manual mesh sync whose outcome left the mesh's position unresolved: a
 *  fast-forward that did not apply, a fetch that did not complete, a comparison
 *  that could not be made, or an IPC that never returned. Only the first of
 *  those establishes that the mesh is behind; the rest establish nothing, which
 *  is why `evidence` exists. Carries the mesh's identity so the card names
 *  which mesh is affected instead of only reporting a git message, and the
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

export interface AgentInputNotificationAction {
  kind: 'agent-input';
  nodeId: number;
  agentName: string;
  meshName: string;
  resolution?: string;
}

export type NotificationAction = WorktreeNotificationAction | MeshSyncNotificationAction | AgentInputNotificationAction;

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

/** Narrow a sync action to the fields its recovery card renders. */
export const isMeshSyncFailure = (
  action: NotificationAction | undefined,
): action is MeshSyncNotificationAction => action?.kind === 'mesh-sync-failure';

export const isAgentInput = (
  action: NotificationAction | undefined,
): action is AgentInputNotificationAction => action?.kind === 'agent-input';

export interface Notification extends Toast {
  read: boolean;
}

export interface WorktreeNotice {
  path: string;
  nodeName: string;
  message: string;
  detail: string;
  createdAt: number;
}

export const NOTIFICATION_HISTORY_LIMIT = 100;
/** An unresolved problem the user is expected to act on: an agent input
 *  request, blocked worktree cleanup or incomplete mesh sync. Resolution text moves a
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
  if (a.kind === 'agent-input') {
    return Number.isSafeInteger(a.nodeId) && (a.nodeId as number) > 0
      && typeof a.agentName === 'string' && typeof a.meshName === 'string'
      ? { kind: 'agent-input', nodeId: a.nodeId as number, agentName: a.agentName,
        meshName: a.meshName, resolution: a.resolution as string | undefined }
      : null;
  }
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
