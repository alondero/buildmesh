import { create } from 'zustand';
import { normalizeRepoInput } from '../lib/githubRepo';
import { useMeshStore } from './meshStore';
import { addToast } from './toastStore';

/**
 * The owner of clone-operation state (issue #2795).
 *
 * A clone outlives the dialog that started it: the backend runs it to
 * completion on the blocking pool, and dismissing the dialog cannot stop it.
 * Keeping that state inside the dialog made the operation invisible — the
 * only trace was a toast that expired, so a user who closed the dialog
 * believed the clone had stopped, started it again, or was surprised when a
 * Mesh appeared.
 *
 * This store is the single owner above the modal. Each operation is keyed by
 * its own identity and keeps the target the user SUBMITTED (repository,
 * destination, colour), so a later edit of the form can never rewrite what
 * is actually running. The dialog is one viewer of this state, not its
 * owner.
 */

/** Where a clone operation is in its life. */
export type CloneOperationStatus = 'running' | 'succeeded' | 'failed';

/**
 * What the user submitted, captured at submit time. The clone is pinned to
 * these values for its whole life: the form may be edited, dismissed or
 * reopened while the clone runs, and none of that may retarget it.
 */
export interface CloneTarget {
  /** The repository input exactly as submitted. */
  repo: string;
  /** The parent directory the clone is written into. */
  parentDir: string;
  /** The full destination path the clone lands on. */
  destination: string;
  /** The Mesh colour submitted with the clone. */
  color: string | null;
}

/** One clone attempt, from submit to outcome. */
export interface CloneOperation extends CloneTarget {
  /** Operation identity — one clone attempt, never reused. */
  id: string;
  /**
   * The normalised repository key two submissions are compared by, so
   * `owner/repo` and its github.com URL name the same clone.
   */
  repoKey: string;
  status: CloneOperationStatus;
  startedAt: number;
  /** When the operation settled; null while it runs. */
  finishedAt: number | null;
  /** The Mesh the clone registered; set on success. */
  meshId: number | null;
  /** That Mesh's name; set on success. */
  meshName: string | null;
  /** The backend's message; set on failure. */
  error: string | null;
}

interface CloneActivityState {
  /** Every operation this session knows about, oldest first. */
  operations: CloneOperation[];
  /**
   * How many surfaces are currently displaying each operation. A COUNT, not a
   * set: two surfaces can show the same operation (the create dialog and the
   * progress surface), and the first one to close must not revoke the other's
   * claim — that would make the store report an outcome the other surface is
   * already showing.
   */
  claimedOperationCounts: Record<string, number>;
  /**
   * Start a clone, or join the identical clone that is already running.
   *
   * Returns the operation the caller must follow, and whether it was joined
   * rather than started. When a clone of the same repository into the same
   * destination is already running, that operation is returned unchanged and
   * NO second clone is launched (issue #2795).
   */
  startClone: (target: CloneTarget) => { operation: CloneOperation; joined: boolean };
  /**
   * Start a new attempt from a settled operation's submitted target. The
   * retry carries the ORIGINAL submitted repository and destination, not
   * whatever the form happens to hold now.
   */
  retryClone: (operationId: string) => { operation: CloneOperation; joined: boolean } | null;
  /** Forget a settled operation. A running operation is never dropped. */
  dismissOperation: (operationId: string) => void;
  /** Mark an operation as displayed by a surface. */
  claimOperation: (operationId: string) => void;
  /** Release one surface's claim on an operation. */
  releaseOperation: (operationId: string) => void;
}

let nextOperationId = 0;

/**
 * The operations the compact activity entries show: a running clone first —
 * that is the one the user must not lose — then the settled ones, newest
 * first. EVERY live operation is listed: two clones can run at once, and a
 * second clone with no entry of its own would be invisible, which is the
 * failure this whole surface exists to prevent.
 */
export function selectVisibleOperations(operations: CloneOperation[]): CloneOperation[] {
  const running = operations.filter(op => op.status === 'running');
  const settled = operations.filter(op => op.status !== 'running').reverse();
  return [...running, ...settled];
}

export const useCloneActivityStore = create<CloneActivityState>((set, get) => {
  /**
   * Run the clone and record its outcome. The IPC itself is delegated to
   * the Mesh store, which owns the Mesh collection; this store owns the
   * operation record and the reporting around it.
   */
  async function runClone(operation: CloneOperation): Promise<void> {
    const result = await useMeshStore
      .getState()
      .cloneMesh(operation.repo, operation.parentDir, operation.color);
    const finishedAt = Date.now();
    const succeeded = 'mesh' in result;
    set(state => ({
      operations: state.operations.map(op =>
        op.id === operation.id
          ? {
              ...op,
              status: succeeded ? 'succeeded' : 'failed',
              finishedAt,
              meshId: succeeded ? result.mesh.id : null,
              meshName: succeeded ? result.mesh.name : null,
              error: succeeded ? null : result.error,
            }
          : op
      ),
    }));

    // Report the outcome only when no surface is already showing it. A
    // dismissed dialog leaves nothing on screen, so the toast is the only
    // announcement — and the operation record outlives it, so a failure
    // stays recoverable after the toast expires.
    if (!get().claimedOperationCounts[operation.id]) {
      addToast(
        'Clone',
        succeeded
          ? `Cloned ${operation.repo} into ${operation.destination}`
          : result.error,
        succeeded ? 'success' : 'error'
      );
    }
  }

  return {
    operations: [],
    claimedOperationCounts: {},

    startClone: (target) => {
      const repoKey = normalizeRepoInput(target.repo);
      const existing = get().operations.find(
        op =>
          op.status === 'running' &&
          op.repoKey === repoKey &&
          // The DESTINATION, not just the parent folder: two spellings of one
          // repository can name different target folders (`Owner/Repo` and
          // `owner/repo`), and the backend writes the folder name as
          // submitted. Joining those would silently skip a clone the user
          // asked for.
          op.destination === target.destination
      );
      // The identical clone is already running — join it rather than
      // cloning the same tree into the same directory a second time.
      if (existing) return { operation: existing, joined: true };

      const operation: CloneOperation = {
        id: `clone-${++nextOperationId}`,
        repoKey,
        ...target,
        status: 'running',
        startedAt: Date.now(),
        finishedAt: null,
        meshId: null,
        meshName: null,
        error: null,
      };
      set(state => ({ operations: [...state.operations, operation] }));
      void runClone(operation);
      return { operation, joined: false };
    },

    retryClone: (operationId) => {
      const previous = get().operations.find(op => op.id === operationId);
      if (!previous) return null;
      // The retry carries the submitted target verbatim. Re-reading the
      // form would let a later edit retarget the retry.
      return get().startClone({
        repo: previous.repo,
        parentDir: previous.parentDir,
        destination: previous.destination,
        color: previous.color,
      });
    },

    dismissOperation: (operationId) =>
      set(state => ({
        // A running operation is never dropped — it is the only place the
        // work is visible.
        operations: state.operations.filter(
          op => op.id !== operationId || op.status === 'running'
        ),
        claimedOperationCounts: omitClaim(state.claimedOperationCounts, operationId),
      })),

    claimOperation: (operationId) =>
      set(state => ({
        claimedOperationCounts: {
          ...state.claimedOperationCounts,
          [operationId]: (state.claimedOperationCounts[operationId] ?? 0) + 1,
        },
      })),

    releaseOperation: (operationId) =>
      set(state => ({
        claimedOperationCounts: omitClaim(state.claimedOperationCounts, operationId),
      })),
  };
});

/**
 * Drop one claim on an operation, removing the key entirely when the last
 * holder lets go.
 */
function omitClaim(
  counts: Record<string, number>,
  operationId: string
): Record<string, number> {
  const current = counts[operationId];
  if (current === undefined) return counts;
  const next = { ...counts };
  if (current <= 1) delete next[operationId];
  else next[operationId] = current - 1;
  return next;
}
