// Updater state machine — shared Zustand store (issue #1526 + #1654 review).
//
// The pre-review design lifted the state machine into a plain custom hook
// (`useUpdateCheck`) and both `UpdatePrompt` (App.tsx) and
// `<UpdateAboutSection>` (Settings > About) called it. Every caller got
// its own `useState`, so dismissing the auto-launch prompt wiped the
// machine that the Settings button had been writing to; closing Settings
// while a download was in flight dropped the progress callbacks into a
// dead instance. The reviewer flagged this as a cross-instance desync
// (finding 1): state must live in a singleton the two surfaces
// subscribe to.
//
// Other review fixes layered on top of the store:
//   - finding 2 — `downloading` and `installing` now map to real
//     operations (`downloadUpdate` / `installUpdate`); no settle sleep.
//   - finding 3 — `failedAt` is narrowed to `'download' | 'install'`
//     (the only two phases that produce a `failed` state) and a new
//     `'enabled' | 'disabled' | null` reflects `updaterEnabled()` so
//     the Settings button stops lying to dev-build users.
//   - finding 4 — restart delegates to `useExitPromptStore.requestExit`
//     so the policy lives in one place.
//   - finding 5 — `cancelInstall()` increments the seq guard and
//     transitions to `failed { failedAt: 'download' }`; ProgressPrompt
//     gets a real escape hatch (with the seq guard making the in-flight
//     download's progress callbacks inert).
//   - finding 7 — the Settings button's `disabled` set includes
//     `'downloading'` and `'installing'` (consumer side).

import { create } from 'zustand';
import { relaunch } from '@tauri-apps/plugin-process';
import type { Update } from '@tauri-apps/plugin-updater';
import {
  runUpdateCheck,
  downloadUpdate,
  installUpdate,
  updaterEnabled,
  type UpdateSummary,
  type DownloadProgress,
  type CheckResult,
} from '../lib/updater';
import { useExitPromptStore } from './exitPromptStore';

export type UpdatePhase =
  | { kind: 'idle' }
  | { kind: 'checking'; manual: boolean }
  | { kind: 'current' }
  | { kind: 'unreachable'; error: string }
  | { kind: 'available'; update: Update; summary: UpdateSummary }
  | { kind: 'downloading'; update: Update; summary: UpdateSummary; progress: DownloadProgress }
  | { kind: 'installing'; update: Update; summary: UpdateSummary }
  | { kind: 'ready_to_restart'; update: Update; summary: UpdateSummary }
  // `restarting` is the brief "click registered, awaiting the modal
  // decision" window. The UpdatePrompt renders nothing here so the
  // ExitConfirmationModal can stack on top without competing for input
  // (finding 4 follow-up).
  | { kind: 'restarting'; update: Update; summary: UpdateSummary }
  | { kind: 'failed'; failedAt: 'download' | 'install'; update: Update | null; summary: UpdateSummary | null; error: string };

export interface UpdateCheckApi {
  state: UpdatePhase;
  /** Whether the updater is enabled in this build (false in dev profile,
   *  non-Tauri page loads, and non-production builds). Computed once on
   *  mount; the Settings > About button uses this to decide whether to
   *  render at all (issue #1654 finding 3). */
  enabled: boolean;
  check: () => Promise<void>;
  install: () => Promise<void>;
  retry: () => Promise<void>;
  restart: () => Promise<void>;
  /** Cancel an in-flight download. Bumps the seq guard so the
   *  download's progress callbacks bail, then transitions to
   *  `failed { failedAt: 'download' }`. The Tauri plugin has no
   *  documented cancel seam, so the request continues in the
   *  background — its terminal callback also bails on the seq guard,
   *  leaving the staged `Update` handle unused. A retry resumes the
   *  staged binary. Finding 5. */
  cancelInstall: () => void;
  dismiss: () => void;
}

// Quiet checks (mount) treat `current`, `unreachable`, and `disabled` as
// no-ops — a launching app doesn't nag with "you're up to date" or "feed
// down". Only `available` flips state into the prompt.
function applyQuietCheck(_prev: UpdatePhase, result: CheckResult): UpdatePhase {
  switch (result.phase) {
    case 'available':
      return { kind: 'available', update: result.update, summary: result.summary };
    case 'current':
    case 'unreachable':
    case 'disabled':
      return { kind: 'idle' };
  }
}

// Manual checks surface every result so the Settings > About surface
// can report "You're up to date" vs "Couldn't reach the update feed".
function applyManualCheck(_prev: UpdatePhase, result: CheckResult): UpdatePhase {
  switch (result.phase) {
    case 'available':
      return { kind: 'available', update: result.update, summary: result.summary };
    case 'current':
      return { kind: 'current' };
    case 'unreachable':
      return { kind: 'unreachable', error: result.error };
    case 'disabled':
      return { kind: 'idle' };
  }
}

interface UpdaterState {
  phase: UpdatePhase;
  /** `null` until the boot-time `updaterEnabled()` probe resolves. The
   *  Settings > About button waits for this so it can either render the
   *  check button (true) or the disabled notice (false), instead of
   *  briefly showing the button and then flickering to "disabled". */
  enabled: boolean | null;
  /** Monotonic version sequence. Stale callbacks (download progress,
   *  check results) compare against this and bail if their captured
   *  version no longer matches. Replaces the previous per-component
   *  `seqRef` since the store is shared across surfaces. */
  seq: number;
  /** Boot-time quiet check, idempotent across React StrictMode double-
   *  mount and across both surfaces mounting their own copies. Mirrors
   *  the `listenersAttached` flag in `agentNodeStore` (issue #1054). */
  quietCheckStarted: boolean;
  enabledCheckStarted: boolean;
  startEnabledCheck: () => Promise<void>;
  startQuietCheck: () => Promise<void>;
  setPhase: (next: UpdatePhase) => void;
  bumpSeq: () => number;
  check: () => Promise<void>;
  install: () => Promise<void>;
  retry: () => Promise<void>;
  restart: () => Promise<void>;
  cancelInstall: () => void;
  dismiss: () => void;
}

export const useUpdaterStore = create<UpdaterState>((set, get) => {
  // Shared runner for both `install()` and `retry()` resume paths. The
  // seq bump at entry makes any earlier download's progress callbacks
  // inert; the per-callback `seq !== get().seq` check covers the inverse
  // race where this round finishes first and a stale callback from a
  // prior round tries to overwrite our settled state.
  const runDownloadAndInstall = async (update: Update, summary: UpdateSummary) => {
    const startSeq = get().seq + 1;
    set({ seq: startSeq, phase: { kind: 'downloading', update, summary, progress: { downloaded: 0, total: null } } });
    try {
      await downloadUpdate(update, (progress) => {
        if (get().seq !== startSeq) return;
        set({ phase: { kind: 'downloading', update, summary, progress } });
      });
      if (get().seq !== startSeq) return;
      // Phase flips BEFORE `installUpdate` so the UI's "Installing…"
      // surface matches the actual operation (finding 2 — pre-review
      // the install ran under the downloading phase).
      set({ phase: { kind: 'installing', update, summary } });
      await installUpdate(update);
      if (get().seq !== startSeq) return;
      set({ phase: { kind: 'ready_to_restart', update, summary } });
    } catch (e) {
      if (get().seq !== startSeq) return;
      const message = e instanceof Error ? e.message : String(e);
      // Distinguish download-time vs install-time failures: if the
      // stored phase is still `downloading`, this catch fired during
      // download; otherwise it fired during install. Finding 3 — the
      // pre-review code hardcoded `'install'` regardless of where the
      // failure actually landed.
      const failedAt: 'download' | 'install' = get().phase.kind === 'installing' ? 'install' : 'download';
      set({ phase: { kind: 'failed', failedAt, update, summary, error: message } });
    }
  };

  return {
    phase: { kind: 'idle' },
    enabled: null,
    seq: 0,
    quietCheckStarted: false,
    enabledCheckStarted: false,

    startEnabledCheck: async () => {
      if (get().enabledCheckStarted) return;
      set({ enabledCheckStarted: true });
      const enabled = await updaterEnabled();
      set({ enabled });
    },

    startQuietCheck: async () => {
      if (get().quietCheckStarted) return;
      set({ quietCheckStarted: true });
      const startSeq = get().seq;
      const result = await runUpdateCheck();
      if (get().seq !== startSeq) return;
      set((s) => ({ phase: applyQuietCheck(s.phase, result) }));
    },

    setPhase: (next) => set({ phase: next }),
    bumpSeq: () => {
      const next = get().seq + 1;
      set({ seq: next });
      return next;
    },

    check: async () => {
      // Bump seq so any in-flight quiet check's result bails instead of
      // overwriting the manual result with a stale `available` / `idle`.
      const startSeq = get().seq + 1;
      set({ seq: startSeq, phase: { kind: 'checking', manual: true } });
      const result = await runUpdateCheck();
      if (get().seq !== startSeq) return;
      set((s) => ({ phase: applyManualCheck(s.phase, result) }));
    },

    install: async () => {
      const current = get().phase;
      if (current.kind !== 'available') return;
      await runDownloadAndInstall(current.update, current.summary);
    },

    retry: async () => {
      const failed = get().phase;
      if (failed.kind !== 'failed') return;
      if (failed.update && failed.summary) {
        await runDownloadAndInstall(failed.update, failed.summary);
        return;
      }
      await get().check();
    },

    restart: async () => {
      const current = get().phase;
      if (current.kind !== 'ready_to_restart') return;
      // Gate relaunch on the shared Exit Readiness policy (#1501 / #1526).
      // The store owns no second policy: it just delegates to the exit
      // prompt store, which encapsulates the active-nodes / providers /
      // partition lookup (finding 4). The decision is a single boolean
      // (`prompted`) — `true` means the modal is up and will dispatch
      // `relaunch` on confirm; `false` means the policy says relaunch
      // directly. Either way the update phase advances out of
      // `ready_to_restart` so the UpdatePrompt can yield input focus
      // to the modal.
      set({ phase: { kind: 'restarting', update: current.update, summary: current.summary } });
      const prompted = await useExitPromptStore.getState().requestExit('update-restart');
      if (!prompted) {
        try {
          await relaunch();
        } catch (e) {
          // Relaunch failed — fall back to `ready_to_restart` so the
          // user can retry from the prompt surface rather than being
          // stuck in `restarting`.
          console.error('[updater] direct relaunch failed:', e);
          set({ phase: { kind: 'ready_to_restart', update: current.update, summary: current.summary } });
        }
      }
    },

    cancelInstall: () => {
      // Bumping the seq makes the in-flight download's progress
      // callbacks bail and any terminal callback (success or failure)
      // also bail — the `runDownloadAndInstall` early-returns when
      // `get().seq !== startSeq`. We transition to `failed` so the
      // user sees a clear "Cancelled" surface and can retry from the
      // preserved `update` handle.
      const current = get().phase;
      if (current.kind !== 'downloading') return;
      const { update, summary } = current;
      const next = get().seq + 1;
      set({ seq: next, phase: { kind: 'failed', failedAt: 'download', update, summary, error: 'Cancelled' } });
    },

    dismiss: () => {
      const startSeq = get().seq + 1;
      set({ seq: startSeq, phase: { kind: 'idle' } });
    },
  };
});

/** Imperative wrapper for non-React callers. Mirrors the
 *  `requestWorktreeCloseAction` pattern in `worktreeClosePromptStore`. */
export function resetUpdaterStateForTests(): void {
  useUpdaterStore.setState({
    phase: { kind: 'idle' },
    enabled: null,
    seq: 0,
    quietCheckStarted: false,
    enabledCheckStarted: false,
  });
}