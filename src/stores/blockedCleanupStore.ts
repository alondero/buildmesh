// Blocked worktree cleanup store (issue #2139).
//
// A node close defers its worktree removal to a durable queue; a removal that
// cannot complete is persisted there as a *blocked cleanup*. This store is the
// frontend half of that contract:
//
//   * `open()` refreshes the list from the backend and shows the dialog, so
//     what the user acts on is exactly what the drain knows (no optimistic copy
//     that can drift from the queue);
//   * every action re-reads the affected row from the backend's own answer
//     rather than patching local state, because the backend is the one that
//     records the new attempt count, error and backoff;
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
  /** Fetch the queue and open the dialog. */
  open: () => Promise<void>;
  /** Re-read the queue without opening (used after each action). */
  refresh: () => Promise<void>;
  close: () => void;
  retry: (path: string) => Promise<void>;
  dismiss: (path: string) => Promise<void>;
  diagnose: (path: string) => Promise<void>;
  release: (path: string, pid: number) => Promise<void>;
}

export const useBlockedCleanupStore = create<BlockedCleanupState>((set, get) => ({
  entries: [],
  isOpen: false,
  loading: false,
  blockers: {},
  actionStatus: {},

  open: async () => {
    set({ isOpen: true, loading: true });
    try {
      const entries = await api.listPendingWorktreeRemovals();
      set({ entries, loading: false });
      if (entries.length === 0) set({ isOpen: false });
    } catch (error) {
      set({ loading: false });
      addToast('Worktree', `Couldn't read the blocked worktree cleanups: ${String(error)}`, 'error');
    }
  },

  refresh: async () => {
    try {
      const entries = await api.listPendingWorktreeRemovals();
      set({ entries });
      // Nothing left to fix: the dialog's job is done.
      if (entries.length === 0 && get().isOpen) set({ isOpen: false });
    } catch {
      // A failed refresh keeps the current list visible: hiding real blocked
      // cleanups because a read timed out would be its own bug.
    }
  },

  close: () => set({ isOpen: false }),

  retry: async (path) => {
    set((state) => ({ actionStatus: { ...state.actionStatus, [path]: 'Retrying…' } }));
    try {
      const record = await api.retryWorktreeCleanup(path);
      // The backend returns `null` when the worktree is gone, and the updated
      // row when it is still blocked — either answer is authoritative, so the
      // local list follows it rather than being patched by hand.
      if (record === null) {
        const remaining = get().entries.filter((entry) => entry.worktree_path !== path);
        set((state) => ({
          entries: remaining,
          blockers: withoutKey(state.blockers, path),
          actionStatus: { ...state.actionStatus, [path]: 'Removed.' },
          isOpen: remaining.length > 0,
        }));
        addToast('Worktree', `Worktree removed: ${path}`, 'success');
      } else {
        set((state) => ({
          entries: state.entries.map((entry) =>
            entry.worktree_path === path ? record : entry,
          ),
          blockers: withoutKey(state.blockers, path),
          actionStatus: { ...state.actionStatus, [path]: 'Still blocked.' },
        }));
      }
    } catch (error) {
      set((state) => ({ actionStatus: { ...state.actionStatus, [path]: `Retry failed: ${String(error)}` } }));
    }
  },

  dismiss: async (path) => {
    try {
      await api.dismissWorktreeCleanup(path);
      const remaining = get().entries.filter((entry) => entry.worktree_path !== path);
      set((state) => ({
        entries: remaining,
        blockers: withoutKey(state.blockers, path),
        isOpen: remaining.length > 0,
      }));
      addToast('Worktree', `Worktree kept in place: ${path}`, 'info');
    } catch (error) {
      set((state) => ({
        actionStatus: { ...state.actionStatus, [path]: `Couldn't keep the worktree: ${String(error)}` },
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

  release: async (path, pid) => {
    set((state) => ({ actionStatus: { ...state.actionStatus, [path]: `Ending process ${pid}…` } }));
    try {
      await api.releaseWorktreeCleanupBlocker(pid);
      // The blocker is gone, so the previous diagnosis is stale: drop it, then
      // retry the cleanup the user asked to unblock.
      set((state) => ({ blockers: withoutKey(state.blockers, path) }));
      await get().retry(path);
    } catch (error) {
      set((state) => ({
        actionStatus: { ...state.actionStatus, [path]: `Couldn't end process ${pid}: ${String(error)}` },
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
