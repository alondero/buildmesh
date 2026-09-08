import { useCallback, useEffect, useRef, useState } from 'react';
import type { Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import {
  runUpdateCheck,
  downloadAndInstallUpdate,
  describeUpdate,
  type UpdateSummary,
  type DownloadProgress,
  type CheckResult,
} from '../lib/updater';
import { useExitPromptStore } from '../stores/exitPromptStore';
import { useAgentNodeStore } from '../stores/agentNodeStore';
import {
  buildSupportsResumeMap,
  getActiveExitNodes,
  partitionExitNodes,
  exitNodeProviderDisplay,
} from '../lib/exitGuard';
import * as api from '../lib/tauri';
import type { ProviderInfo } from '../types/generated/ProviderInfo';

// Updater state machine (issue #1526).
//
// The previous hook tracked two bits (`update`, `installing`); this one
// owns an eight-phase discriminated union. The hook is the single
// source of truth that `UpdatePrompt` (auto-prompt on launch) and
// Settings > About (manual check button) both subscribe to — they see
// the same phase, so a user who clicks "Check for updates" in Settings
// and then dismisses the auto-prompt on launch can't end up looking at
// stale state.
//
// Quiet auto-check (mount) and explicit manual check (Settings /
// "Retry") share one `runUpdateCheck()`; the hook suppresses
// non-`available` results for quiet checks so the prompt doesn't
// bother the user with "you're up to date" noise.
//
// `ready_to_restart` preserves the staged `Update` handle — the user
// can dismiss the prompt and re-trigger Restart later without losing
// the install state. The plugin's `install()` has already staged the
// binary; relaunch picks it up regardless.

export type UpdatePhase =
  | { kind: 'idle' }
  | { kind: 'checking'; manual: boolean }
  | { kind: 'current' }
  | { kind: 'unreachable'; error: string }
  | { kind: 'available'; update: Update; summary: UpdateSummary }
  | { kind: 'downloading'; update: Update; summary: UpdateSummary; progress: DownloadProgress }
  | { kind: 'installing'; update: Update; summary: UpdateSummary }
  | { kind: 'ready_to_restart'; update: Update; summary: UpdateSummary }
  // `update` is preserved on `failed` for download/install so retry can
  // resume the staged handle instead of re-probing the feed.
  | { kind: 'failed'; failedAt: 'check' | 'download' | 'install'; update: Update | null; summary: UpdateSummary | null; error: string };

export interface UpdateCheckApi {
  state: UpdatePhase;
  /** Re-run the feed probe (manual). Surfaces `current` / `unreachable`
   *  even if the mount-time quiet check suppressed them. */
  check: () => Promise<void>;
  /** Download + install the staged update. Promotes `state` through
   *  `downloading` → `installing` → `ready_to_restart`. */
  install: () => Promise<void>;
  /** Retry the most recent failed phase (`check`, `download`, or
   *  `install`) without re-entering earlier phases. */
  retry: () => Promise<void>;
  /** Relaunch into the new version, gated by the shared Exit Readiness
   *  seam from #1501 (active non-resumable nodes trigger the same
   *  confirmation modal the window-close flow uses). */
  restart: () => Promise<void>;
  /** Dismiss the prompt for this session. Resets to `idle` so the
   *  modal disappears; the user can re-trigger via `check()` from
   *  Settings > About. */
  dismiss: () => void;
}

// Quietly run the mount-time check. Quiet checks treat `current`,
// `unreachable`, and `disabled` as no-ops so a launching app doesn't
// surface "no update" or "feed down" noise. Only `available` flips
// state into the prompt.
function applyQuietCheck(prev: UpdatePhase, result: CheckResult): UpdatePhase {
  switch (result.phase) {
    case 'available':
      return { kind: 'available', update: result.update, summary: result.summary };
    case 'current':
    case 'unreachable':
    case 'disabled':
      return prev;
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

// Settle window between `installing` and `ready_to_restart` so the UI
// renders the "Installing…" surface before flipping to "Restart now".
// Tauri's `install()` returns immediately on success (the binary is
// staged but not yet swapped onto disk) — without this pause the user
// sees the install as silently dropped.
const INSTALL_SETTLE_MS = 150;

/** Drive the updater state machine and expose phase + actions.
 *
 *  - Quiet check on first mount (issue #826 behaviour).
 *  - Manual checks via Settings > About.
 *  - Phase-aware install with progress.
 *  - Restart is routed through the shared `useExitPromptStore` so the
 *    active-Node policy from #1501 is honored once, not duplicated.
 *  - `dismiss` clears the prompt for the session without throwing
 *    away a downloaded update (the plugin has staged the binary;
 *    relaunch will pick it up regardless). */
export function useUpdateCheck(): UpdateCheckApi {
  const [state, setState] = useState<UpdatePhase>({ kind: 'idle' });
  // Monotonic version sequence — guards against a stale `install`
  // callback resolving after the user has dismissed / cancelled. Each
  // action increments; closures capture the version they were started
  // at and bail when their captured version no longer matches.
  const seqRef = useRef(0);

  // Mount-time quiet check (issue #826). Single fire — settings has a
  // dedicated manual button. The `cancelled` flag covers the React
  // strict-mode double-invoke case.
  useEffect(() => {
    let cancelled = false;
    void runUpdateCheck().then((result) => {
      if (cancelled) return;
      setState((prev) => applyQuietCheck(prev, result));
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const check = useCallback(async () => {
    const version = ++seqRef.current;
    setState({ kind: 'checking', manual: true });
    const result = await runUpdateCheck();
    if (version !== seqRef.current) return;
    setState((prev) => applyManualCheck(prev, result));
  }, []);

  // Shared install body for both the initial `install()` action and the
  // `retry()` resume path (Tauri's updater keeps partial downloads on
  // disk, so retry can call `download()` again on the same handle).
  // Sequence bumps the version so a stale install callback can't
  // overwrite a newer `dismiss` / `retry` round.
  const runDownloadAndInstall = useCallback(
    async (update: Update, summary: UpdateSummary) => {
      const version = ++seqRef.current;
      setState({
        kind: 'downloading',
        update,
        summary,
        progress: { downloaded: 0, total: null },
      });
      try {
        await downloadAndInstallUpdate(update, (progress) => {
          if (version !== seqRef.current) return;
          setState({ kind: 'downloading', update, summary, progress });
        });
        if (version !== seqRef.current) return;
        setState({ kind: 'installing', update, summary });
        await new Promise((resolve) => setTimeout(resolve, INSTALL_SETTLE_MS));
        if (version !== seqRef.current) return;
        setState({ kind: 'ready_to_restart', update, summary });
      } catch (e) {
        if (version !== seqRef.current) return;
        const message = e instanceof Error ? e.message : String(e);
        // Preserve `update` so retry can resume from the same staged
        // handle — Tauri's updater keeps partial downloads on disk and
        // re-validates on resume.
        setState({ kind: 'failed', failedAt: 'install', update, summary, error: message });
      }
    },
    [],
  );

  const install = useCallback(async () => {
    // Snapshot the update handle before transitioning so the closure
    // doesn't read stale state when the in-flight `download` callback
    // fires after `setState({ kind: 'failed', ... })` has cleared it.
    const startState = stateRef.current;
    if (startState.kind !== 'available') return;
    await runDownloadAndInstall(startState.update, startState.summary);
  }, [runDownloadAndInstall]);

  const retry = useCallback(async () => {
    const failed = stateRef.current;
    if (failed.kind !== 'failed') return;
    if (failed.failedAt === 'check') {
      const version = ++seqRef.current;
      setState({ kind: 'checking', manual: true });
      const result = await runUpdateCheck();
      if (version !== seqRef.current) return;
      setState((prev) => applyManualCheck(prev, result));
      return;
    }
    // download / install: resume from the staged handle. If the
    // handle was lost (e.g. the original feed probe resolved to a
    // summary without persisting the native object), fall back to a
    // fresh feed probe.
    if (failed.update && failed.summary) {
      await runDownloadAndInstall(failed.update, failed.summary);
      return;
    }
    await check();
  }, [check, runDownloadAndInstall]);

  const restart = useCallback(async () => {
    if (stateRef.current.kind !== 'ready_to_restart') return;
    // Gate relaunch on the shared Exit Readiness policy (#1501 /
    // #1526). The policy: confirm-before-quit preference + at least
    // one active node → show the exit-confirmation modal in
    // `update-restart` mode (which dispatches `relaunch()` on
    // confirm, not `exitApplication()`). Otherwise fire `relaunch()`
    // directly. This mirrors WindowCloseGuard's flow so the updater
    // does NOT own a divergent active-node policy.
    const confirmBeforeQuit = useExitPromptStore.getState().confirmBeforeQuit;
    try {
      const nodes = useAgentNodeStore.getState().getAgentNodes();
      const active = getActiveExitNodes(nodes);
      if (!confirmBeforeQuit || active.length === 0) {
        await relaunch();
        return;
      }
      let providers: ProviderInfo[] = [];
      try {
        providers = await api.listProviders();
      } catch {
        // Fail-closed: unknown harnesses partition as non-resumable
        // (mirror WindowCloseGuard), so an unreachable provider list
        // widens the warning rather than narrowing it.
        providers = [];
      }
      const supportsMap = buildSupportsResumeMap(providers);
      const { nonResumable } = partitionExitNodes(active, supportsMap);
      useExitPromptStore.getState().showExitPrompt(
        'update-restart',
        active.length,
        nonResumable.map((n) => ({
          id: n.id,
          name: n.name,
          providerDisplay: exitNodeProviderDisplay(n, providers),
        })),
      );
      // The actual relaunch is dispatched by `useExitPromptStore`'s
      // `confirmExit` once the user confirms; no further work here.
    } catch (e) {
      console.error('[updater] failed to evaluate exit readiness:', e);
      // On any unexpected error, fall back to the direct relaunch so
      // the user isn't stuck in `ready_to_restart` because the exit-
      // readiness machinery tripped on a network blip.
      try {
        await relaunch();
      } catch (relaunchError) {
        console.error('[updater] relaunch fallback failed:', relaunchError);
      }
    }
  }, []);

  const dismiss = useCallback(() => {
    setState({ kind: 'idle' });
  }, []);

  // Keep a ref of the latest state so the action closures always read
  // the freshest value. Without this, `install()` and `restart()`
  // capture whichever state was committed at render time and skip
  // updates that arrive between action invocations.
  const stateRef = useRef(state);
  stateRef.current = state;

  return { state, check, install, retry, restart, dismiss };
}

// Re-export `describeUpdate` for the UI layer (kept here so the
// `UpdatePrompt` import surface is one symbol per concern).
export { describeUpdate };