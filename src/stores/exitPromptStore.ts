import { create } from 'zustand';
import { relaunch } from '@tauri-apps/plugin-process';
import * as api from '../lib/tauri';
import { addToast } from './toastStore';
import type { ExitNonResumableEntry } from '../lib/exitGuard';

/** Which shutdown the pending prompt is gating. The modal renders the
 *  same surface; only the action dispatched on confirm differs:
 *  - `window-close`  → backend's `exit_application` (issue #1501).
 *  - `update-restart` → the updater's `relaunch()`, which swaps the
 *    staged binary on top of the running process. The updater route
 *    is the same seam, not a second one — `showExitPrompt` is the
 *    single source of truth for "should we prompt before tearing down
 *    agent state". Issue #1526. */
export type ExitPromptMode = 'window-close' | 'update-restart';

interface ExitPendingPrompt {
  mode: ExitPromptMode;
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
  /**
   * Open the exit-confirmation modal. `mode` picks which action the
   * modal's "Confirm" button dispatches (issue #1526):
   * `window-close` → `exitApplication()` (issue #1501),
   * `update-restart` → `relaunch()` (the updater plugin's swap).
   */
  showExitPrompt: (
    mode: ExitPromptMode,
    activeCount: number,
    nonResumable: ExitNonResumableEntry[],
  ) => void;
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
   * Confirm the exit: dispatch to the action chosen by the prompt's
   * `mode` (issue #1526):
   *  - `window-close`  → backend's `exit_application` (`AppHandle::exit`
   *    with the suspend sweep); the running process ends without
   *    relaunching the staged binary.
   *  - `update-restart` → the updater plugin's `relaunch()`, which
   *    swaps the staged binary and restarts the process.
   *
   *  On any failure the running app is still up, so the expected-exit
   *  marking the backend recorded on `CloseRequested` must be retracted
   *  the same way "Keep Working" does — a stale marker would make a
   *  later real crash look intentional and skip the watchdog auto-
   *  relaunch. The failure is surfaced as a toast and `exiting` resets
   *  so the user can retry. */
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

  showExitPrompt: (mode, activeCount, nonResumable) =>
    set({ pending: { mode, activeCount, nonResumable } }),

  keepWorking: () => {
    if (!get().pending) return;
    set({ pending: null });
    void api.cancelWindowClose().catch((e) => {
      console.warn('[ExitPrompt] Failed to retract expected-exit marking:', e);
    });
  },

  confirmExit: async () => {
    const prompt = get().pending;
    if (!prompt) return;
    set({ exiting: true });
    try {
      if (prompt.mode === 'update-restart') {
        // Issue #1526 — updater restart. The plugin swaps the staged
        // binary on top of the running process; `relaunch()` ends the
        // current process and starts the new one. On success the
        // window closes anyway, so no post-success retract is needed.
        await relaunch();
        return;
      }
      // window-close: hand shutdown to the backend's lifecycle owner
      // (`exit_application` → `AppHandle::exit`, which fires the
      // `ExitRequested` sweep). Window commands are ACL-gated so we
      // can't `destroy()` the window from the webview.
      await api.exitApplication();
    } catch (e) {
      console.warn('[ExitPrompt] shutdown failed:', e);
      // Still running — undo the backend's eager expected-exit marking
      // (same retract "Keep Working" performs). For `update-restart`
      // there is no backend marking yet (the updater hasn't started
      // a close sweep), but the retract is a safe no-op.
      void api.cancelWindowClose().catch((retractError) => {
        console.warn('[ExitPrompt] Failed to retract expected-exit marking:', retractError);
      });
      // Issue #1501 — toast title kept verbatim for window-close so the
      // existing toast contract (and its consumers) is stable. The
      // updater-restart path surfaces its own title to distinguish
      // "your exit was interrupted" from "your restart was interrupted".
      if (prompt.mode === 'update-restart') {
        addToast(
          'Restart failed',
          'Restart failed — the app is still running. Check buildmesh.log and try again.',
          'error',
        );
      } else {
        addToast(
          'Exit Buildmesh',
          'Exit failed — the app is still running. Check buildmesh.log and try again.',
          'error',
        );
      }
      set({ exiting: false });
    }
  },
}));