// Blocked worktree cleanup store (issue #2139).
//
// A node close defers its worktree removal to a durable queue; a removal that
// cannot complete is a *blocked cleanup*. This store is the frontend half of
// that contract:
//
//   * `open()` refreshes the list from the backend and shows the dialog, so
//     what the user acts on is exactly what the drain knows (no optimistic copy
//     that can drift from the queue);
//   * every action follows the backend's own answer rather than patching local
//     state, because the backend is the one that records the new attempt count,
//     error and backoff, and the one that knows whether the worktree was
//     actually removed, left alone because a live spawn claimed it, or never
//     even attempted because the queue could not be read;
//   * a dismissed ("keep worktree") or successfully removed entry leaves the
//     list, and the dialog closes when nothing is left.
//
// The backend suppresses repeated events for an unchanged blocker, so the
// dialog is opened by an event *and* can be reopened later (the list is always
// live).

import { create } from 'zustand';
import * as api from '../lib/tauri';
import { addToast } from './toastStore';
import type { PendingWorktreeRemoval } from '../types/generated/PendingWorktreeRemoval';
import type { BlockingProcess } from '../types/generated/BlockingProcess';
import type { WorktreeCleanupRetry } from '../types/generated/WorktreeCleanupRetry';

interface BlockedCleanupState {
  entries: PendingWorktreeRemoval[];
  /** Whether the dialog is showing. Named `isOpen` so it cannot shadow the
   *  `open()` action that populates it. */
  isOpen: boolean;
  loading: boolean;
  /** path -> processes diagnosed as pinning it. Deliberately per-path and
   *  cleared on every retry: a stale diagnosis is worse than none, because the
   *  user would close the wrong process. */
  blockers: Record<string, BlockingProcess[]>;
  /** path -> transient status line for the last action on it. */
  actionStatus: Record<string, string>;
  /** The process the user is being asked to confirm ending, if any. */
  pendingKill: { path: string; process: BlockingProcess } | null;
  /** Fetch the queue and open the dialog. */
  open: () => Promise<void>;
  close: () => void;
  retry: (path: string) => Promise<void>;
  dismiss: (path: string) => Promise<void>;
  diagnose: (path: string) => Promise<void>;
  /** Ask the user to confirm before ending a diagnosed process. */
  requestRelease: (path: string, process: BlockingProcess) => void;
  cancelRelease: () => void;
  confirmRelease: () => Promise<void>;
}

export const useBlockedCleanupStore = create<BlockedCleanupState>((set, get) => ({
  entries: [],
  isOpen: false,
  loading: false,
  blockers: {},
  actionStatus: {},
  pendingKill: null,

  open: async () => {
    set({ isOpen: true, loading: true });
    try {
      const entries = await api.listPendingWorktreeRemovals();
      set({ entries, loading: false });
      if (entries.length === 0) set({ isOpen: false });
    } catch (error) {
      // A queue that cannot be read is not an empty queue. Keeping the dialog
      // up and saying so is the difference between "nothing is blocked" and
      // "we don't know" — the second must not hide the recovery surface for a
      // worktree that really is stuck (issue #2139 review round 2).
      set({ loading: false });
      addToast('Worktree', `Couldn't read the blocked worktree cleanups: ${String(error)}`, 'error');
    }
  },

  close: () => set({ isOpen: false }),

  retry: async (path) => {
    set((state) => ({ actionStatus: { ...state.actionStatus, [path]: 'Retrying…' } }));
    let result;
    try {
      result = await api.retryWorktreeCleanup(path);
    } catch (error) {
      set((state) => ({
        actionStatus: { ...state.actionStatus, [path]: `Retry failed: ${String(error)}` },
      }));
      return;
    }
    // The backend's status is the truth about what happened to the folder.
    // Only `removed` justifies saying the worktree is gone (issue #2139 review
    // round 1: the old UI collapsed every non-blocked outcome into "removed").
    const status: WorktreeCleanupRetry = result.status;
    if (status === 'removed') {
      const remaining = get().entries.filter((entry) => entry.worktree_path !== path);
      set((state) => ({
        entries: remaining,
        blockers: withoutKey(state.blockers, path),
        actionStatus: { ...state.actionStatus, [path]: 'Removed.' },
        isOpen: remaining.length > 0,
      }));
      addToast('Worktree', `Worktree removed: ${path}`, 'success');
      return;
    }
    if (status === 'still-blocked' && result.record) {
      const record = result.record;
      set((state) => ({
        entries: state.entries.map((entry) => (entry.worktree_path === path ? record : entry)),
        blockers: withoutKey(state.blockers, path),
        actionStatus: { ...state.actionStatus, [path]: 'Still blocked.' },
      }));
      return;
    }
    if (status === 'gone') {
      const remaining = get().entries.filter((entry) => entry.worktree_path !== path);
      set((state) => ({
        entries: remaining,
        blockers: withoutKey(state.blockers, path),
        actionStatus: { ...state.actionStatus, [path]: 'Already gone.' },
        isOpen: remaining.length > 0,
      }));
      addToast('Worktree', `Nothing left to retry at ${path} — it was already cleaned.`, 'info');
      return;
    }
    if (status === 'claimed') {
      // A live agent now owns this directory (issue #653): the tombstone was
      // dequeued and the directory deliberately left alone.
      const remaining = get().entries.filter((entry) => entry.worktree_path !== path);
      set((state) => ({
        entries: remaining,
        blockers: withoutKey(state.blockers, path),
        actionStatus: {
          ...state.actionStatus,
          [path]: 'Left alone: a live agent now owns this worktree.',
        },
        isOpen: remaining.length > 0,
      }));
      addToast('Worktree', `Worktree left alone: a live agent now owns ${path}`, 'info');
      return;
    }
    // queue-read-failed: the row is untouched, so it stays visible and the
    // user is told nothing was retried.
    set((state) => ({
      actionStatus: {
        ...state.actionStatus,
        [path]: "Couldn't read the cleanup queue; nothing was retried.",
      },
    }));
    addToast(
      'Worktree',
      `Couldn't read the cleanup queue for ${path}; nothing was retried.`,
      'error',
    );
  },

  dismiss: async (path) => {
    try {
      // The reply says what was actually done to the disk, and whether the
      // cleanup intent was cancelled. Only a cancelled intent removes the entry:
      // a failed restore leaves the row queued, so the drain keeps trying and
      // the entry stays visible (issue #2139 review round 2).
      const result = await api.dismissWorktreeCleanup(path);
      if (result.cancelled) {
        const remaining = get().entries.filter((entry) => entry.worktree_path !== path);
        set((state) => ({
          entries: remaining,
          blockers: withoutKey(state.blockers, path),
          isOpen: remaining.length > 0,
        }));
      }
      addToast('Worktree', result.message, 'info');
    } catch (error) {
      set((state) => ({
        actionStatus: {
          ...state.actionStatus,
          [path]: `Couldn't keep the worktree: ${String(error)}`,
        },
      }));
    }
  },

  diagnose: async (path) => {
    try {
      const processes = await api.diagnoseWorktreeCleanupBlockers(path);
      set((state) => ({
        blockers: { ...state.blockers, [path]: processes },
        actionStatus: {
          ...state.actionStatus,
          [path]:
            processes.length === 0
              ? 'No process was found holding the folder — the blocker may already be gone.'
              : `${processes.length} process(es) may be holding the folder.`,
        },
      }));
    } catch (error) {
      set((state) => ({
        actionStatus: { ...state.actionStatus, [path]: `Diagnosis failed: ${String(error)}` },
      }));
    }
  },

  requestRelease: (path, process) => set({ pendingKill: { path, process } }),
  cancelRelease: () => set({ pendingKill: null }),

  confirmRelease: async () => {
    const pending = get().pendingKill;
    if (!pending) return;
    const { path, process } = pending;
    set({ pendingKill: null });
    set((state) => ({ actionStatus: { ...state.actionStatus, [path]: `Ending process ${process.pid}…` } }));
    try {
      // The backend re-diagnoses and refuses a pid that is not a current
      // blocker of this worktree, which is the defence against id reuse.
      await api.releaseWorktreeCleanupBlocker(path, process.pid);
      set((state) => ({ blockers: withoutKey(state.blockers, path) }));
      await get().retry(path);
    } catch (error) {
      set((state) => ({
        actionStatus: {
          ...state.actionStatus,
          [path]: `Couldn't end process ${process.pid}: ${String(error)}`,
        },
      }));
    }
  },
}));

/** A copy of `map` without `key`. Purity keeps the reducer honest under React's
 *  double-invocation and makes the "stale diagnosis is dropped" rule obvious. */
function withoutKey<T>(map: Record<string, T>, key: string): Record<string, T> {
  const next = { ...map };
  delete next[key];
  return next;
}

/** Opens the blocked-cleanup dialog from a non-React caller (the
 *  `worktree-cleanup-failed` event listener in App.tsx), mirroring
 *  `requestWorktreeCloseAction`. */
export function openBlockedCleanups(): void {
  void useBlockedCleanupStore.getState().open();
}
