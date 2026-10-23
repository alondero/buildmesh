import { listen } from '@tauri-apps/api/event';
import { createStreamSubscription } from './streamSubscription';
import { pathMatchesGitEvent } from '../paths';
import type { MeshSyncWarningPayload } from '../../types/generated/MeshSyncWarningPayload';

/**
 * Owner of the `mesh-sync-warning` live stream (issue #2531).
 *
 * The backend emits this event for any non-fatal auto-sync failure
 * (network down, diverged history, repo unusable, PR head unfetchable,
 * PR SHA drift). Two surfaces read it: `App.tsx` raises the toast, and
 * the sidebar's mesh row lights its failure icon for the mesh the
 * warning is about. Each used to hold its own `listen` and re-implement
 * the "is this warning about MY mesh?" comparison.
 *
 * This module owns the ONE `listen`, owns the event name, and owns the
 * match rule.
 */
export const MESH_SYNC_WARNING_EVENT = 'mesh-sync-warning';

/**
 * A non-fatal sync failure for one mesh.
 *
 * `meshPath` is the path of the mesh whose sync failed — the event is
 * app-global, so a consumer that renders per mesh has to narrow it.
 */
export interface MeshSyncWarning {
  /** The message the toast shows. */
  readonly message: string;
  /** The path of the mesh the warning is about, or `null` when unknown. */
  readonly meshPath: string | null;
}

/**
 * Convert a raw `mesh-sync-warning` payload into a typed warning.
 *
 * A payload without a `message` is not a warning this stream can
 * describe (the toast would show `undefined`), so it is dropped rather
 * than published half-formed. `mesh_path` is optional here: the
 * generated type carries it, but a warning that names no mesh
 * still belongs in the toast stream.
 */
export function toMeshSyncWarning(payload: unknown): MeshSyncWarning | null {
  if (typeof payload !== 'object' || payload === null) return null;
  const candidate = payload as Partial<MeshSyncWarningPayload>;
  if (typeof candidate.message !== 'string') return null;
  const meshPath = typeof candidate.mesh_path === 'string' && candidate.mesh_path !== ''
    ? candidate.mesh_path
    : null;
  return { message: candidate.message, meshPath };
}

/**
 * The match rule: is this warning about the mesh at `meshPath`?
 *
 * Uses the same path-normalisation helper the app's other mesh-scoped
 * comparisons use (slash/case/worktree-aware), so the rule stays in step
 * with them.
 */
export function concernsMeshPath(warning: MeshSyncWarning, meshPath: string): boolean {
  if (warning.meshPath === null) return false;
  return pathMatchesGitEvent({ path: warning.meshPath }, meshPath);
}

const subscription = createStreamSubscription<MeshSyncWarning>(
  'meshSyncWarningStream',
  async (notify) => {
    const unlisten = await listen<MeshSyncWarningPayload>(
      MESH_SYNC_WARNING_EVENT,
      ({ payload }) => {
        const warning = toMeshSyncWarning(payload);
        if (warning !== null) notify(warning);
      },
    );
    return unlisten;
  },
);

/**
 * Subscribe to mesh sync warnings.
 *
 * Consumers narrow the warning with `concernsMeshPath` — the rule lives
 * here so both subscribers agree on what "about my mesh" means.
 */
export function subscribeMeshSyncWarnings(
  listener: (warning: MeshSyncWarning) => void,
): () => void {
  return subscription.subscribe(listener);
}

/** Test-only: drop the consumers and detach the backend subscription. */
export function __resetMeshSyncWarningStreamForTests(): void {
  subscription.reset();
}
