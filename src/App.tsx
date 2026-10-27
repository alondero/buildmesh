import { useCallback, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import type { ProviderErrorPayload } from './types/generated/ProviderErrorPayload';
import type { ResumeFailedPayload } from './types/generated/ResumeFailedPayload';
import type { WorktreeCleanupFailedPayload } from './types/generated/WorktreeCleanupFailedPayload';
import type { CircuitAgentBlockedPayload } from './types/generated/CircuitAgentBlockedPayload';
import { subscribeMeshSyncWarnings } from './lib/streams/meshSyncWarningStream';
import { useGlobalShortcuts } from './hooks/useGlobalShortcuts';
import { Sidebar } from './components/Sidebar/Sidebar';
import { TitleBar } from './components/TitleBar/TitleBar';
import { AgentNodeView } from './components/AgentNodeView/AgentNodeView';
import { ProbePanel } from './components/Probe/ProbePanel';
import { WorktreeCloseDialog } from './components/WorktreeCloseDialog/WorktreeCloseDialog';
import { WindowCloseGuard } from './components/WindowCloseGuard/WindowCloseGuard';
import { CanvasSpawnMenu } from './components/AgentNodeView/CanvasSpawnMenu';
import { MeshCreateModal } from './components/Mesh/MeshCreateModal';
import { ShortcutCheatsheet } from './components/ShortcutCheatsheet/ShortcutCheatsheet';
import { CommandOmnibar } from './components/CommandOmnibar/CommandOmnibar';
import { UpdatePrompt } from './components/UpdatePrompt/UpdatePrompt';
import { BootErrorPanel } from './components/BootErrorPanel/BootErrorPanel';
import { formatError } from './lib/errorUtils';
import { runBoot } from './lib/bootSequence';
import { useMeshStore } from './stores/meshStore';
import { useAgentNodeStore } from './stores/agentNodeStore';
import { useExitPromptStore } from './stores/exitPromptStore';
import { useUIStore } from './stores/uiStore';
import { isTerminalFocused } from './lib/focusGuard';
import { dispatchShortcut, shortcutBindings } from './lib/shortcutDispatch';
import { isMac } from './lib/platform';
import { useFileDropToTerminal } from './hooks/useFileDropToTerminal';
import { useNamingBackendFailureToast } from './hooks/useNamingBackendFailureToast';
import { useIdleAnimationGate } from './hooks/useIdleAnimationGate';
import * as api from './lib/tauri';
import { addToast, dismissToast, useToastStore } from './stores/toastStore';
import type { CircuitNotificationPayload } from './types/generated/CircuitEvents';
import './App.css';

function App() {
  // Issue #1246 — select actions individually. Subscribing to the bare
  // store (no selector) subscribes the component to the *entire* state
  // object, so every patchAgentNode (attention flip, status update, etc.)
  // re-rendered the whole app even though App only consumes action refs.
  // Action refs are stable across renders in Zustand, so per-action
  // selectors short-circuit change detection and App stays quiet during
  // high-frequency agent events. `storeError` is the one state field we
  // actually render on, so it stays as a direct selector.
  //
  // Issue #1524 — boot uses `refreshMeshes`, the rejecting variant of the
  // mesh loader: a snapshot that never loaded must fail the boot, not
  // paint an empty workspace. `fetchMeshes` stays the fire-and-forget
  // loader for background refreshes.
  const refreshMeshes = useMeshStore((s) => s.refreshMeshes);
  const fetchAgentNodes = useAgentNodeStore((s) => s.fetchAgentNodes);
  const initAttentionListeners = useAgentNodeStore((s) => s.initAttentionListeners);
  const storeError = useAgentNodeStore(state => state.error);

  // Issue #1001 — toast list lives in the shared store.
  const toasts = useToastStore((s) => s.toasts);
  const [isReady, setIsReady] = useState(false);
  // Issue #1250 — when one of the boot IPCs rejects, we surface the
  // formatted error via `<BootErrorPanel>` and let the user retry.
  // Previously a rejected invoke was caught, logged, and discarded —
  // `isReady` stayed false and the pulsing splash was the only signal,
  // making the app look hung. `initBusy` gates the Retry button so a
  // panicking backend can't be hit with overlapping inits.
  const [initError, setInitError] = useState<string | null>(null);
  const [initBusy, setInitBusy] = useState(false);
  // Cheatsheet open state (issue #731). The <ShortcutCheatsheet> component
  // mounts only while true, which is what arms the <Modal>-owned Escape
  // listener — otherwise Escape would be stolen from agent terminals in
  // the grid. Same mount/unmount discipline as WorktreeCloseDialog.
  // Issue #1411 review: hoisted into `uiStore` (not local state) so the
  // Omnibar's "Show Cheatsheet" command opens the same modal without a
  // window-event side channel.
  const cheatsheetOpen = useUIStore((s) => s.cheatsheetOpen);
  // Issue #1536 — the App-level mounts for the canvas empty state's
  // Mesh Create modal and Spawn Menu dialog. Subscribing (NOT calling
  // `useUIStore.getState()` inside the render body) keeps the mounts
  // reactive to external opens from the canvas empty state without a
  // round-trip through the Sidebar.
  const createMeshOpen = useUIStore((s) => s.createMeshOpen);
  const closeCreateMesh = useUIStore((s) => s.closeCreateMesh);
  const canvasSpawnMenuMeshId = useUIStore((s) => s.canvasSpawnMenuMeshId);
  // Paste absolute file paths into the hovered agent terminal on OS file drop.
  useFileDropToTerminal();

  // Issue #2599 — pause the decorative infinite CSS animations while the app
  // is not the foreground surface. An infinite animation makes the renderer
  // Compositor produce a frame every vsync even when nothing on the page
  // changed, which pinned the compositor thread near one core on a long
  // running hub. The gate writes a root attribute and holds no React state,
  // so a focus change never re-renders the workspace.
  useIdleAnimationGate();

  // Keyboard shortcuts — Tauri's globalShortcut plugin, so they work even
  // when an xterm.js terminal has keyboard focus (xterm intercepts window
  // keydown events). Only register while the window is focused so they don't
  // steal from other apps; that focus-tracking + register/unregister
  // bookkeeping is owned by `useGlobalShortcuts` (issue #1249), which papers
  // over the StrictMode mount→unmount→mount races.
  //
  // Issue #2240 — the bindings, their guards and their handlers are ONE table
  // in `lib/shortcutDispatch.ts`; this is the only wiring App does. The table
  // is also where the cheatsheet rows come from, so a binding and its
  // discoverability surface cannot drift, and tests fire
  // `dispatchShortcut(action)` instead of regexing this file. Platform
  // branching (Alt+G vs ⌘+G, Ctrl+Shift+K vs ⌘+K, the macOS ⌘+⌥+F carve-out)
  // lives in each row's `accelerator`.
  useGlobalShortcuts({
    bindings: shortcutBindings(isMac),
    onTrigger: dispatchShortcut,
  });

  // ?-key cheatsheet toggle (issue #731). Plain window keydown listener
  // (NOT a Tauri global-shortcut registration) for two reasons:
  //
  //   1. The `?` key is shifted on US layouts and unmodified on some
  //      European layouts — registering it as a Tauri global-shortcut
  //      would tie us to a single key-shape string per platform and force
  //      two registrations. A window keydown listener matches `e.key`
  //      semantically, which is what the user means.
  //
  //   2. The listener is intentionally NOT in the Tauri global-shortcut
  //      list, so a focused xterm terminal sees the keystroke first
  //      (terminals are the only place the user might want to type `?`
  //      as a literal character).
  //
  // The two keydown-shape guards stay here because they are about the
  // *keystroke*, not the action; the action's own guards (text-input focus
  // + 300ms cooldown) and its handler live in the dispatch table:
  //   - `e.repeat`: a held `?` autorepeats at ~30 Hz after ~500 ms on
  //     Windows; without this guard the modal flickers open/closed for
  //     as long as the user keeps the key down.
  //   - `isTerminalFocused()`: typing `?` in an agent terminal is the
  //     user asking the agent a question — the `?` is content, not a
  //     help request. This is the inverse of the `isTextInputFocused`
  //     xterm carve-out used by Alt+G (which DOES want to fire from a
  //     terminal prompt).
  //
  // `preventDefault()` is keyed off the dispatch outcome rather than a
  // duplicated focus check: blocking it on `blocked-focus` is what keeps a
  // `?` typed into an inline rename (or the terminal search box) from being
  // swallowed by the handler.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== '?') return;
      if (e.repeat) return;
      if (isTerminalFocused()) return;
      if (dispatchShortcut('open-cheatsheet') !== 'blocked-focus') {
        e.preventDefault();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  useEffect(() => {
    const unlisten = listen<ProviderErrorPayload>('provider-error', (event) => {
      addToast(event.payload.provider, event.payload.message, 'error');
    });
    return () => { unlisten.then((fn) => fn()); };
    // `addToast` is a module-level function that delegates to
    // `useToastStore.getState().addToast` — stable across renders,
    // so it doesn't belong in the dep array.
  }, []);

  useEffect(() => {
    if (storeError) {
      addToast('System', storeError, 'error');
    }
    // `addToast` is a module-level function — see the provider-error
    // listener effect above.
  }, [storeError]);

  // Issue #1250 — extract init into a callback so the BootErrorPanel's
  // Retry button can re-run it without unmounting the whole App.
  // Issue #1524 — the verdict comes from `runBoot`, which owns the
  // failure contract: it treats a rejected loader AND a stored store
  // error as a failure, so a Mesh or Agent Node snapshot that never
  // loaded can't be mistaken for an empty workspace. `isReady` is set
  // only on a fully clean run. `initBusy` gates the Retry button so a
  // panicking backend can't be hit with overlapping inits.
  const init = useCallback(async () => {
    setInitError(null);
    setInitBusy(true);
    // Issue #1501: hydrate the exit-confirm prompt preference. Deliberately
    // NOT in the boot gate — a prefs-read failure must neither block boot
    // nor flip the guard off; the store keeps its fail-closed `true`
    // default and the guard decides synchronously from it.
    void useExitPromptStore.getState().initConfirmBeforeQuit();
    try {
      // No data dependency between the loaders — they run concurrently so
      // first paint isn't gated on three serial calls.
      const outcome = await runBoot({
        attachListeners: initAttentionListeners,
        // `refreshMeshes` (not `fetchMeshes`): the boot loader must reject
        // so a Mesh failure is caught by the rejection channel too, not
        // only by the stored error `runBoot` cross-checks.
        loadMeshes: refreshMeshes,
        loadAgentNodes: fetchAgentNodes,
        readSnapshotErrors: () => ({
          meshes: useMeshStore.getState().error,
          agentNodes: useAgentNodeStore.getState().error,
        }),
      });
      if (!outcome.ok) {
        setInitError(outcome.errors.join('\n'));
        return;
      }
      setIsReady(true);

      // Auto-resume suspended sessions after a brief delay to ensure
      // terminals and event listeners are mounted
      setTimeout(async () => {
        try {
          const resumed = await api.autoResumeAgentNodes();
          if (resumed.length > 0) {
            console.info(`[App] Auto-resumed ${resumed.length} sessions`);
          }
          // Recovery can update identity even when the subsequent launch fails.
          await fetchAgentNodes();
        } catch (e) {
          console.error('[App] Auto-resume failed:', e);
        }
      }, 1000);
    } catch (e) {
      // Defense in depth: `runBoot` never rejects for a loader failure, but
      // if the failure contract itself breaks (e.g. a store read throws), we
      // still surface the error instead of leaving the splash up forever.
      console.error('[App] Init failed:', e);
      setInitError(formatError(e));
    } finally {
      setInitBusy(false);
    }
  }, [initAttentionListeners, refreshMeshes, fetchAgentNodes]);

  useEffect(() => {
    init();
  }, [init]);

  useEffect(() => {
    const unlisten = listen<ResumeFailedPayload>('resume-failed', (event) => {
      addToast('Resume', `Node ${event.payload.node_id}: ${event.payload.error}`, 'warning');
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // Background failure records attention without taking focus from the workspace.
  useEffect(() => {
    const unlisten = listen<WorktreeCleanupFailedPayload>(
      'worktree-cleanup-failed',
      (event) => {
        void import('./lib/worktreeCleanupAlerts').then((module) =>
          module.reportBlockedWorktreeCleanup(event.payload),
        );
      },
    );
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  useEffect(() => {
    let active = true;
    let stop: (() => void) | undefined;
    void import('./lib/worktreeNotifications').then(module => {
      if (active) stop = module.watchWorktreeNotifications();
    });
    return () => { active = false; stop?.(); };
  }, []);

  // The auto-sync (issue #213) couldn't fully reconcile the parent
  // mesh with origin — either the fetch failed (network down) or
  // the local history has diverged from the remote. The Agent Node
  // is still being spawned from the local HEAD; this is a heads-up,
  // not an error. The label is `Sync` to keep it distinct from
  // `provider-error` (red, fatal-ish) and `Worktree` (cleanup
  // failures). All three share the same toast stack — only the
  // provider label differs — so the user gets a consistent visual
  // treatment for any non-blocking runtime issue.
  useEffect(() => subscribeMeshSyncWarnings((warning) => {
    addToast('Sync', warning.message, 'warning');
  }), []);

  useEffect(() => {
    const unlisten = listen<CircuitNotificationPayload>('circuit-notification', ({ payload }) => {
      const severity = payload.severity === 'success' || payload.severity === 'info'
        ? payload.severity
        : payload.severity === 'error' ? 'error' : 'warning';
      addToast(`Circuit #${payload.run_id}`, payload.message, severity);
    });
    return () => { void unlisten.then(fn => fn()); };
  }, []);

  // Circuit needs-input notifications share the toast stack with Sync and
  // Worktree. The inline action remains attached to the owning agent node.
  useEffect(() => {
    const unlistenBlocked = listen<CircuitAgentBlockedPayload>(
      'circuit-agent-blocked',
      (event) => {
        addToast(
          'Circuit',
          `Agent on node ${event.payload.node_id} needs your input (issue #${event.payload.issue}).`,
          'warning',
        );
      },
    );
    return () => {
      unlistenBlocked.then((fn) => fn());
    };
  }, []);

  // Auto-dismiss toasts after TOAST_TTL_MS. A 1s tick is coarse
  // enough that it won't fight React's render cycle, fine enough
  // that the user sees the toast disappear in real time.
  useEffect(() => {
    if (toasts.length === 0) return;
    const tick = () => useToastStore.getState().dismissExpired(Date.now());
    const interval = setInterval(tick, 1000);
    return () => clearInterval(interval);
  }, [toasts.length]);

  // Issue #846: surface the sticky-lockout `naming-backend-failed` event as a
  // toast. The backend has already burned MAX_RENAME_ATTEMPTS=3 retries on
  // the LLM call before emitting (see `session_naming::on_turn_with`), so the
  // severity is `error`, not `warning` — this is a real failure, not a hiccup.
  // The toast points the user at Settings → Auto-naming (issue #824) so they
  // can pick a working backend instead of giving up silently.
  const handleNamingBackendFailure = useCallback(
    ({ node_id, reason }: { node_id: number; reason: string }) => {
      addToast(
        'Auto-naming',
        `Couldn't auto-name node ${node_id} after several retries (${reason}). Pick a name manually, or check Settings → Auto-naming.`,
        'error',
      );
    },
    [],
  );
  useNamingBackendFailureToast(handleNamingBackendFailure);

  if (initError) {
    // Issue #1250 — instead of an infinite pulsing splash, surface the
    // formatted init error with a Retry button. The user can re-trigger
    // `init` (the same callback the mount effect ran) without unmounting
    // the whole App.
    // The close guard mounts here too (issue #1501 review): a close
    // during boot must still prompt rather than silently kill spawns.
    return (
      <>
        <BootErrorPanel
          error={initError}
          onRetry={init}
          busy={initBusy}
        />
        <WindowCloseGuard />
      </>
    );
  }

  if (!isReady) {
    // Close guard mounted alongside the splash (issue #1501 review) —
    // same reason as the error branch above.
    return (
      <div className="flex flex-col h-screen w-screen bg-bg-base">
        <TitleBar />
        <div
          role="status"
          aria-label="Loading Buildmesh"
          className="flex flex-1 items-center justify-center"
        >
          <div className="text-accent-cyan text-2xl animate-pulse">●</div>
        </div>
        <WindowCloseGuard />
      </div>
    );
  }

    return (
    <div className="flex flex-col h-screen w-screen overflow-hidden bg-bg-base text-text-primary">
      {/* Bespoke window chrome (replaces the native title bar): wordmark,
          the ViewModeSwitcher toolbar, settings/remote icons and the
          minimize/maximize/close controls. The window runs frameless
          ("decorations": false in tauri.conf.json); dragging and
          double-click maximize are handled by the bar's drag regions. */}
      <TitleBar />
      <div className="flex flex-1 overflow-hidden">
        <Sidebar />
        <div className="flex-1 flex flex-col overflow-hidden">
          <AgentNodeView />
        </div>
        <ProbePanel />
      </div>

      <WorktreeCloseDialog />
      {/* Issue #1536 — the canvas empty state needs to summon this
          modal from outside the Sidebar's render tree. Both call sites
          (the Sidebar's mesh-creation CTA + the canvas empty state's
          "New mesh" CTA) hit `useUIStore.openCreateMesh`. The
          modal mounts only when explicitly opened — the flag stays
          false on startup so a fresh app boot never flashes the
          dialog. */}
      {createMeshOpen && (
        // `MeshCreateModal` owns its own `defaultColor` derivation
        // (subscribes to `meshes.length` inside the modal, where the
        // subscription is conditional on the modal being mounted).
        // App.tsx used to read `useMeshStore.getState().meshes.length`
        // inside the JSX render — a concurrent-render tear-safety
        // anti-pattern. Senior-review round 4 fixed.
        <MeshCreateModal onClose={closeCreateMesh} />
      )}
      {/* `canvasSpawnMenuMeshId !== null` is the ONLY mount trigger.
          The modal's internal fallback-to-first-mesh logic (an earlier
          iteration) was the UI lockout trap the senior review caught:
          closing the modal would set the id back to null, the
          fallback would re-resolve to a real mesh, modal would
          re-render, modal would be inescapable. Now the call site
          always sets a specific mesh id (resolved in AgentNodeView's
          onOpenSpawnMenu callback) and `null` strictly means "closed". */}
      {canvasSpawnMenuMeshId !== null && (
        <CanvasSpawnMenu meshId={canvasSpawnMenuMeshId} />
      )}
      <WindowCloseGuard />
      <ShortcutCheatsheet open={cheatsheetOpen} onClose={() => useUIStore.getState().closeCheatsheet()} />
      {/* Universal Command Omnibar (issue #1411). Same mount/unmount
          discipline as the cheatsheet: it renders only while
          `omnibarOpen` is true, so the overlay never touches the terminal
          grid underneath — xterm instances stay mounted and focused. */}
      <CommandOmnibar />
      <UpdatePrompt />

      {/* Toast notifications. Each toast carries role="status" (implicit
          aria-live=polite) so screen readers announce it on arrival without
          moving focus — the container itself stays silent to avoid double
          announcements. */}
      <div className="fixed bottom-32 right-4 flex flex-col gap-2 z-50">
        {toasts.map((toast) => {
          const isWarning = toast.severity === 'warning';
          const isSuccess = toast.severity === 'success';
          const isInfo = toast.severity === 'info';
          return (
            <div
              key={toast.id}
              role="status"
              className={`animate-slide-in-right bg-bg-surface border px-4 py-3 rounded-md flex items-start gap-2 min-w-[280px] max-w-[420px] shadow-md ${
                isWarning ? 'border-status-warning/50' : isSuccess ? 'border-status-success/50' : isInfo ? 'border-border-strong' : 'border-status-error/50'
              }`}
            >
              <div className="flex-1 min-w-0">
                <div
                  className={`text-2xs font-bold uppercase ${
                    isWarning ? 'text-status-warning' : isSuccess ? 'text-status-success' : isInfo ? 'text-text-secondary' : 'text-status-error'
                  }`}
                >
                  {toast.provider}
                </div>
                <div className="text-xs text-text-secondary break-words">{toast.message}</div>
              </div>
              <button
                type="button"
                onClick={() => dismissToast(toast.id)}
                aria-label="Dismiss notification"
                className="shrink-0 -m-1 p-1 rounded-md text-text-secondary hover:text-text-primary hover:bg-bg-card-hover text-base leading-none transition-colors"
              >
                ×
              </button>
            </div>
          );
        })}
      </div>
    </div>
  );
}

export default App;
