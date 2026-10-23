import { listen } from '@tauri-apps/api/event';
import { createStreamSubscription } from './streamSubscription';

/**
 * Owner of the `worktree-directory-changed` live stream (issue #2531).
 *
 * The backend emits this event when a worktree container directory
 * changes: `update_mesh_worktree_directory` (payload = the mesh id)
 * and `set_app_worktree_directory` (payload = `null`, because every
 * mesh inheriting the app default may have moved).
 *
 * Two surfaces re-resolve their effective directory on it — the
 * mesh-health hook and the Probe's Project Settings tab — and each one
 * used to re-implement the same "is this change about MY mesh?" filter.
 * This module owns the ONE `listen`, owns the event name, and owns the
 * match rule.
 *
 * The Rust half declares the same string as `WORKTREE_DIR_CHANGED_EVENT`
 * in `src-tauri/src/commands/mesh_properties.rs`.
 */
export const WORKTREE_DIR_CHANGED_EVENT = 'worktree-directory-changed';

/**
 * A worktree container directory moved.
 *
 * `meshId === null` means the app-wide default moved: every mesh that
 * inherits it may have moved, so every mesh has to re-resolve.
 */
export interface WorktreeDirectoryChange {
  /** The mesh whose directory changed, or `null` when the app default moved. */
  readonly meshId: number | null;
}

/**
 * Convert a raw `worktree-directory-changed` payload into a typed
 * change.
 *
 * `null` / `undefined` mean "the app-wide default moved". A number
 * names the mesh whose own directory moved. Anything else is not a
 * payload this stream owns and is dropped.
 */
export function toWorktreeDirectoryChange(payload: unknown): WorktreeDirectoryChange | null {
  if (payload === null || payload === undefined) return { meshId: null };
  if (typeof payload !== 'number' || !Number.isFinite(payload)) return null;
  return { meshId: payload };
}

/**
 * The match rule: does this change concern `meshId`?
 *
 * A `null` mesh id in the change means the app-wide default moved, so
 * every mesh must re-resolve; otherwise only the named mesh does.
 */
export function concernsMesh(change: WorktreeDirectoryChange, meshId: number): boolean {
  return change.meshId === null || change.meshId === meshId;
}

const subscription = createStreamSubscription<WorktreeDirectoryChange>(
  'worktreeDirectoryStream',
  async (notify) => {
    const unlisten = await listen<unknown>(WORKTREE_DIR_CHANGED_EVENT, ({ payload }) => {
      const change = toWorktreeDirectoryChange(payload);
      if (change !== null) notify(change);
    });
    return unlisten;
  },
);

/**
 * Subscribe to worktree-directory changes.
 *
 * Consumers narrow the change with `concernsMesh` — the rule lives
 * here so both subscribers agree on what "about my mesh" means.
 */
export function subscribeWorktreeDirectoryChanges(
  listener: (change: WorktreeDirectoryChange) => void,
): () => void {
  return subscription.subscribe(listener);
}

/** Test-only: drop every consumer and detach the backend subscription. */
export function __resetWorktreeDirectoryStreamForTests(): void {
  subscription.reset();
}
