import { create } from 'zustand';
import * as api from '../lib/tauri';
import { addToast } from './toastStore';
import type { ExitNonResumableEntry } from '../lib/exitGuard';

interface ExitPendingPrompt {
  activeCount: number;
  nonResumable: ExitNonResumableEntry[];
}

interface ExitPromptState {
  /**
   * In-memory mirror of `AppPreferences.confirm_before_quit` (issue #1501).
   * `true` until boot hydration says otherwise — fail-closed so a close
   * during the first second still prompts, and so the close handler can
   * decide synchronously without cold IPC on the veto path.
   */
  confirmBeforeQuit: boolean;
  setConfirmBeforeQuit: (value: boolean) => void;
  /** Boot hydration (fire-and-forget from `App.init`): a failed read keeps
   *  the `true` default rather than blocking startup or flipping to `false`. */
  initConfirmBeforeQuit: () => Promise<void>;
  /** Set while the exit-confirmation modal is showing. */
  pending: ExitPendingPrompt | null;
  /** Set once the user confirms, while shutdown is in flight. */
  exiting: boolean;
  showExitPrompt: (activeCount: number, nonResumable: ExitNonResumableEntry[]) => void;
  /**
   * Dismiss the modal ("Keep Working", Escape, backdrop). Clears the
   * pending state AND retracts the backend's eager expected-exit marking
   * via `cancel_window_close` — otherwise the stale marker + flag would
   * make a later real crash look expected and suppress the watchdog
   * auto-relaunch (issue #1501 review). Best-effort: a failed retract
   * must never fail the dismiss itself.
   */
  keepWorking: () => void;
  /**
   * Confirm the exit: hand shutdown to the backend's lifecycle owner
   * (`exit_application` → `AppHandle::exit`, which fires the
   * `ExitRequested` sweep) rather than destroying a raw window through an
   * ACL-gated IPC from the webview.
   *
   * If the command fails, the app is still running — so the expected-exit
   * state the backend recorded on `CloseRequested` must be retracted the
   * same way "Keep Working" does (a stale marker would make a later real
   * crash look intentional and skip the watchdog auto-relaunch), the
   * failure is surfaced as a toast, and `exiting` resets so the user can
   * retry.
   */
  confirmExit: () => Promise<void>;
}

export const useExitPromptStore = create<ExitPromptState>((set, get) => ({
  confirmBeforeQuit: true,

  setConfirmBeforeQuit: (value) => set({ confirmBeforeQuit: value }),

  initConfirmBeforeQuit: async () => {
    try {
      const prefs = await api.getAppPreferences();
      // Older backends predate the field — `?? true` keeps the safe
      // default (prompt) for those installs.
      get().setConfirmBeforeQuit(prefs.confirm_before_quit ?? true);
    } catch (e) {
      console.warn('[ExitPrompt] Failed to load confirm_before_quit, keeping default (prompt):', e);
    }
  },

  pending: null,
  exiting: false,

  showExitPrompt: (activeCount, nonResumable) =>
    set({ pending: { activeCount, nonResumable } }),

  keepWorking: () => {
    if (!get().pending) return;
    set({ pending: null });
    void api.cancelWindowClose().catch((e) => {
      console.warn('[ExitPrompt] Failed to retract expected-exit marking:', e);
    });
  },

  confirmExit: async () => {
    if (!get().pending) return;
    set({ exiting: true });
    try {
      await api.exitApplication();
    } catch (e) {
      console.warn('[ExitPrompt] exit_application failed:', e);
      // Still running — undo the backend's eager expected-exit marking
      // (same retract "Keep Working" performs), then tell the user and
      // let them retry.
      void api.cancelWindowClose().catch((retractError) => {
        console.warn('[ExitPrompt] Failed to retract expected-exit marking:', retractError);
      });
      addToast(
        'Exit Buildmesh',
        'Exit failed — the app is still running. Check buildmesh.log and try again.',
        'error',
      );
      set({ exiting: false });
    }
  },
}));
