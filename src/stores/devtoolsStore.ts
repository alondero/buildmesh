import { create } from 'zustand';
import { getDevtoolsStatus } from '../lib/tauri/devtools';
import type { DevtoolsStatus } from '../types/generated/DevtoolsStatus';

/**
 * The DevTools (CDP) port this process opened (issue #2836).
 *
 * The port is a **startup** switch, so the answer does not change while the
 * app runs: WebView2 reads its browser arguments when the window is
 * created. That makes a one-shot read enough, and it makes the store a
 * mirror rather than a live feed — `status` is `null` until the first read
 * lands, and stays `null` if the read fails.
 *
 * Two surfaces read it: the app-chrome banner (which says out loud that a
 * local process can drive the window) and the settings row (which has to
 * distinguish "the port you asked for" from "the port that is open").
 */
interface DevtoolsState {
  /** The realized status, or `null` before the first read lands. */
  status: DevtoolsStatus | null;
  /** Read the status once. Safe to call more than once. */
  initDevtoolsStatus: () => Promise<void>;
}

export const useDevtoolsStore = create<DevtoolsState>((set, get) => ({
  status: null,

  initDevtoolsStatus: async () => {
    if (get().status) return;
    try {
      const status = await getDevtoolsStatus();
      set({ status });
    } catch (e) {
      // Non-fatal: the port may well be closed, and a failed read must not
      // put a banner on screen claiming a port is open.
      console.warn('[Devtools] Failed to read the DevTools port status:', e);
    }
  },
}));
