import { create } from 'zustand';
import * as api from '../lib/tauri';
import { getCurrentWindow } from '@tauri-apps/api/window';
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
  /** Set once the user confirms, while window destruction is in flight. */
  exiting: boolean;
  showExitPrompt: (activeCount: number, nonResumable: ExitNonResumableEntry[]) => void;
  /**
   * Dismiss the modal ("Keep Working", Escape, backdrop). Clears the
   * two-layer exit attempt and retracts the backend's eager expected-exit
   * marking via `cancel_window_close` — otherwise the stale marker + flag
   * would make a later real crash look expected and suppress the watchdog
   * auto-relaunch (issue #1501 review). Best-effort: a failed retract
   * must never fail the dismiss itself.
   */
  keepWorking: () => void;
  /**
   * Confirm the exit. Two layers (issue #1501 regression, 2026-09-06):
   *
   * 1. The custom `exit_application` backend command — Rust-side
   *    `WebviewWindow::destroy`. Custom commands are NOT gated by the Tauri
   *    ACL (capabilities are compiled into the binary, so a binary built
   *    before `core:window:allow-destroy` landed rejects the webview-side
   *    `destroy` IPC with "not allowed by ACL" — the exact failure that left
   *    the button dead), so this is the path that always ships working.
   * 2. The direct webview-side `getCurrentWindow().destroy()` fallback —
   *    belt-and-braces if the custom command itself fails.
   *
   * If both fail, surface a toast (visible — the old code only warned to
   * the console bridge) and reset `exiting` so the user can retry.
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
    // Layer 1: ACL-proof custom command (Rust-side destroy).
    try {
      await api.exitApplication();
      return;
    } catch (e) {
      console.warn('[ExitPrompt] exit_application failed, falling back to webview destroy:', e);
    }
    // Layer 2: direct webview-side destroy (works when the binary's
    // compiled-in capabilities include `allow-destroy`).
    try {
      await getCurrentWindow().destroy();
      return;
    } catch (e) {
      console.warn('[ExitPrompt] Window destroy failed:', e);
    }
    // Both layers failed — the user must see this, not a devtools-only
    // console.warn (the original bug hid behind exactly that). Reset
    // `exiting` so the button un-flickers and can be pressed again.
    addToast('Exit Buildmesh', 'Exit failed — the window could not be closed. Check buildmesh.log and try again.', 'error');
    set({ exiting: false });
  },
}));
