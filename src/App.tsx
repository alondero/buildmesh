import { useCallback, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import type { ProviderErrorPayload } from './types/generated/ProviderErrorPayload';
import type { ResumeFailedPayload } from './types/generated/ResumeFailedPayload';
import type { WorktreeCleanupFailedPayload } from './types/generated/WorktreeCleanupFailedPayload';
import type { MeshSyncWarningPayload } from './types/generated/MeshSyncWarningPayload';
import type { AutopilotBlockedPayload } from './types/generated/AutopilotBlockedPayload';
import type { AutopilotPrCreatedPayload } from './types/generated/AutopilotPrCreatedPayload';
import type { AutopilotFinishFailedPayload } from './types/generated/AutopilotFinishFailedPayload';
import type { AutopilotSubmittedPayload } from './types/generated/AutopilotSubmittedPayload';
import type { AutopilotNodeClosedPayload } from './types/generated/AutopilotNodeClosedPayload';
import { useGlobalShortcuts } from './hooks/useGlobalShortcuts';
import { Sidebar } from './components/Sidebar/Sidebar';
import { TitleBar } from './components/TitleBar/TitleBar';
import { AgentNodeView } from './components/AgentNodeView/AgentNodeView';
import { ProbePanel } from './components/Probe/ProbePanel';
import { WorktreeCloseDialog } from './components/WorktreeCloseDialog/WorktreeCloseDialog';
import { WindowCloseGuard } from './components/WindowCloseGuard/WindowCloseGuard';
import { ShortcutCheatsheet } from './components/ShortcutCheatsheet/ShortcutCheatsheet';
import { CommandOmnibar } from './components/CommandOmnibar/CommandOmnibar';
import { UpdatePrompt } from './components/UpdatePrompt/UpdatePrompt';
import { BootErrorPanel } from './components/BootErrorPanel/BootErrorPanel';
import { formatError } from './lib/errorUtils';
import { useMeshStore } from './stores/meshStore';
import { useAgentNodeStore } from './stores/agentNodeStore';
import { useExitPromptStore } from './stores/exitPromptStore';
import { useUIStore } from './stores/uiStore';
import { createShortcutGuard } from './lib/shortcutGuard';
import { createKeyRepeatThrottle } from './lib/keyRepeatThrottle';
import { isTextInputFocused, isTerminalFocused } from './lib/focusGuard';
import { traversalTargetId } from './lib/gridTraversal';
import { toggleGridMaximize, cycleGridMode, buildFocusGridSearchBinding, triggerNewAgentShortcut } from './lib/gridShortcuts';
import { scopeNodesForMode } from './lib/viewModes';
import { activityRootId, groupActivityNodes } from './lib/nodeActivities';
import type { NonSingleViewMode } from './stores/uiStore';
import { jumpToNextAwaitingNode } from './lib/awaitingInputShortcuts';
import { isMac } from './lib/platform';
import { useFileDropToTerminal } from './hooks/useFileDropToTerminal';
import { useNamingBackendFailureToast } from './hooks/useNamingBackendFailureToast';
import * as api from './lib/tauri';
import { addToast, dismissToast, useToastStore } from './stores/toastStore';
import type { CircuitNotificationPayload } from './types/generated/CircuitEvents';
import './App.css';

const createNodeGuard = createShortcutGuard(300);
// Cooldown for the Alt+G / Cmd+G grid-toggle (issue #668). Same 300ms budget
// as the new-agent guard: long enough that holding the key doesn't fire a
// burst of toggles, short enough that a deliberate second press feels
// responsive.
const toggleMaximizeGuard = createShortcutGuard(300);
// Cooldown for the Ctrl+Alt+G / Cmd+Alt+G view-mode cycle (ticket #987). Same
// 300ms budget as the grid-toggle guard: a held key must not burst-rotate
// through Mesh → Pinned → All faster than the user can read the switcher.
const cycleViewModeGuard = createShortcutGuard(300);
// Cooldown for the ?-key cheatsheet toggle (issue #731). Belt-and-braces
// against accidental double-taps (e.g. finger-roll on the keyboard) firing
// the modal twice within 300 ms; the `e.repeat` guard inside the handler
// is the primary defense against OS autorepeat (held `?`).
const cheatsheetGuard = createShortcutGuard(300);
// Leading-edge throttle (DIFFERENT primitive from the guards above —
// block-all vs first-press-passes-then-rate-limit) for the arrow-traversal
// handler below. 200ms = ~5 moves/sec on a held key; 100ms snappier than the
// 300ms cooldowns because navigation benefits from responsiveness, while
// still comfortably catching the OS autorepeat flood (~30 Hz Windows,
// ~15 Hz macOS).
const arrowThrottle = createKeyRepeatThrottle(200);

function App() {
  // Issue #1246 — select actions individually. Subscribing to the bare
  // store (no selector) subscribes the component to the *entire* state
  // object, so every patchAgentNode (attention flip, status update, etc.)
  // re-rendered the whole app even though App only consumes action refs.
  // Action refs are stable across renders in Zustand, so per-action
  // selectors short-circuit change detection and App stays quiet during
  // high-frequency agent events. `storeError` is the one state field we
  // actually render on, so it stays as a direct selector.
  const fetchMeshes = useMeshStore((s) => s.fetchMeshes);
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

  // Paste absolute file paths into the hovered agent terminal on OS file drop.
  useFileDropToTerminal();

  // Keyboard shortcuts — use Tauri's globalShortcut plugin so they work even when
  // an xterm.js terminal has keyboard focus (xterm intercepts window keydown events).
  // Only register shortcuts when the window is focused so they don't steal from other apps.
  // Ctrl/Cmd+Alt+←/→/↑/↓ traverses the on-screen agent-node grid of the active
  // View Mode (Mesh / Pinned / All / Filtered — tickets #987, #1609).
  // We use TWO modifiers here (not bare Ctrl/Cmd+Arrow) because Ctrl+←/→ is
  // the readline `backward-word` / `forward-word` gesture in bash/zsh/fish/
  // PSReadLine and every node/python REPL — the global-shortcut plugin
  // captures at the OS layer (before xterm sees the keydown), so bare
  // Ctrl+Arrow would silently steal word-movement whenever the user tries to
  // fix a typo in a long agent prompt. Adding `Alt+` matches the Windows
  // Snap / tmux prefix+Arrow / i3 / VS Code "Move Editor Group" precedent
  // for pane navigation and collides with nothing.
  // Alt+G (Win/Linux) / Cmd+G (macOS) toggles grid ↔ single-view (issue #668).
  // The focus-tracking + register/unregister bookkeeping is owned by
  // `useGlobalShortcuts` (issue #1249); it papers over the StrictMode
  // mount→unmount→mount races that the previous inline effect hit
  // (focus listener leak + stranded shortcut bindings).
  //
  // Tauri 2's global-shortcut plugin doesn't expose an `AltOrCommand`
  // combinator (only `CommandOrControl`, which means Cmd on Mac and Ctrl
  // elsewhere). Issue #668 explicitly wants `Alt+G` on Windows/Linux —
  // the mnemonic "G for Grid" — and `Cmd+G` on macOS. So we register
  // the platform-appropriate binding only, branched on `isMac`. The
  // handler below is platform-agnostic: same action, same store calls.
  const gridToggleShortcut = isMac
    ? { key: 'CommandOrControl+G', action: 'toggle-maximize-grid' as const }
    : { key: 'Alt+G', action: 'toggle-maximize-grid' as const };

  // Issue #1409 — the Universal Command Omnibar. The chord MUST fire while
  // an xterm terminal has focus, so it lives here as a Tauri global
  // shortcut (same reason as Ctrl+T / Ctrl+. above). But bare
  // CommandOrControl+K / +P would steal readline's Ctrl+K (kill-line) and
  // Ctrl+P (previous-history) from every Win/Linux shell — the same war
  // the Alt+Grid-toggle and Ctrl/Cmd+Alt+Arrow bindings already fought
  // (see the arrow comment above). So like the grid toggle, we branch the
  // binding on platform: Cmd+K / Cmd+P on macOS (meta is free), and
  // Ctrl+Shift+K / Ctrl+Shift+P elsewhere (Shift-augmented, no readline
  // collision). Both dispatch the same two actions.
  const omnibarShortcuts = isMac
    ? [
        { key: 'CommandOrControl+K', action: 'open-omnibar' as const },
        { key: 'CommandOrControl+P', action: 'open-omnibar-commands' as const },
      ]
    : [
        { key: 'CommandOrControl+Shift+K', action: 'open-omnibar' as const },
        { key: 'CommandOrControl+Shift+P', action: 'open-omnibar-commands' as const },
      ];

  // Issue #998 — focus the grid search input. The platform-branch logic
  // and the macOS `⌘+⌥+F` collision carve-out (vs. `term-find` on bare
  // `⌘+F`) live in `buildFocusGridSearchBinding` so the branch can be
  // unit-tested directly without regexing this file. See
  // `src/lib/gridShortcuts.ts` for the full rationale.
  const focusGridSearchShortcut = buildFocusGridSearchBinding(isMac);

  // Controls snapshot for the arrow-traversal + focus-grid-search handlers
  // below. Read at dispatch time (not subscribed) so App re-renders stay
  // independent of filter keystrokes — #1609 moved search-driven narrowing
  // into the Filtered view, and traversal must match that grid exactly.
  const filteredControls = () => {
    const s = useUIStore.getState();
    return {
      gridSearchQuery: s.gridSearchQuery,
      gridProviderFilter: s.gridProviderFilter,
      gridStatusFilter: s.gridStatusFilter,
    };
  };

  const shortcuts = [
    { key: 'CommandOrControl+T', action: 'new-agent' },
    ...omnibarShortcuts,
    { key: 'CommandOrControl+Alt+ArrowLeft', action: 'arrow-left' },
    { key: 'CommandOrControl+Alt+ArrowRight', action: 'arrow-right' },
    { key: 'CommandOrControl+Alt+ArrowUp', action: 'arrow-up' },
    { key: 'CommandOrControl+Alt+ArrowDown', action: 'arrow-down' },
    // Cycle to the next agent node with `status === 'awaiting_input'`
    // (issue #64). Registered as a Tauri global shortcut (not a window
    // keydown listener) so the binding fires while an xterm terminal has
    // focus — the typical state when the user notices a node waiting for
    // input and wants to jump to it without clicking. The cycle logic
    // (next-after-current + wrap, scoped to the active node's mesh) lives
    // in src/lib/awaitingInputShortcuts.ts as a pure store mutator so it
    // can be unit-tested without standing up the plugin.
    { key: 'CommandOrControl+Period', action: 'jump-to-next-awaiting' },
    // Ctrl+Alt+G (Win/Linux) / Cmd+Alt+G (macOS) cycles the grid View Modes
    // Mesh → Pinned → All (ticket #987) — the keyboard peer to the mouse-only
    // ViewModeSwitcher. Platform-uniform `CommandOrControl+Alt+G`: it can't
    // collide with the Single solo toggle (`Alt+G` on Win/Linux, `Cmd+G` on
    // macOS) because it carries the extra Alt/⌥ modifier.
    { key: 'CommandOrControl+Alt+G', action: 'cycle-grid-modes' },
    focusGridSearchShortcut,
    gridToggleShortcut,
  ];

  useGlobalShortcuts({
    bindings: shortcuts,
    onTrigger: (action) => {
      window.dispatchEvent(new CustomEvent('shortcut-triggered', { detail: action }));
    },
  });

  // Handle shortcut events (Ctrl+T new-agent; Ctrl/Cmd+Alt+Arrow for node
  // traversal). Arrow traversal works in two phases: in Single view mode the
  // first arrow press exits the solo view back onto the node the user was
  // viewing (activeNodeId already points at it); on the next press (and beyond)
  // we walk the on-screen grid of the active View Mode from that node. Edge
  // semantics are defined in `arrowTargetIndex` (src/lib/gridTraversal.ts):
  // Left/Right wrap within the row, Up/Down are no-ops at the grid's vertical
  // edges.
  //
  // Note: this differs from the Escape-to-exit-Single path in AgentNodeView.tsx
  // (which leaves activeNodeId alone). Esc is a passive exit; Ctrl+Arrow is an
  // exit-and-move, so it makes sense to refocus the node the user was on.
  useEffect(() => {
    const ARROW_DIRECTIONS = ['left', 'right', 'up', 'down'] as const;
    type ArrowDirection = (typeof ARROW_DIRECTIONS)[number];
    const isArrowAction = (a: string): a is `arrow-${ArrowDirection}` =>
      a.startsWith('arrow-') && (ARROW_DIRECTIONS as readonly string[]).includes(a.slice('arrow-'.length));

    const handleShortcut = (e: Event) => {
      const action = (e as CustomEvent<string>).detail;

      if (action === 'new-agent') {
        createNodeGuard(() => triggerNewAgentShortcut());
        return;
      }

      if (action === 'toggle-maximize-grid') {
        // Issue #668 — Alt+G (Win/Linux) / Cmd+G (macOS). The toggle logic
        // (restore if maximized, else maximize active node) is a pure
        // store-mutator extracted to `src/lib/gridShortcuts.ts` for unit
        // testing; here we only own the platform wiring: cooldown so a held
        // key doesn't burst-toggle, and a text-input focus guard so typing
        // an inline-renamed node header or a future search box doesn't
        // accidentally collapse the grid.
        //
        // Check the focus guard BEFORE wrapping in `toggleMaximizeGuard`:
        // the guard sets `blocked = true` on entry, so a no-op press would
        // burn the 300ms cooldown and silently drop the user's next
        // deliberate Alt+G inside that window.
        if (isTextInputFocused()) return;
        toggleMaximizeGuard(async () => {
          toggleGridMaximize();
        });
        return;
      }

      if (action === 'cycle-grid-modes') {
        // Ticket #987 — Ctrl+Alt+G / Cmd+Alt+G rotates the grid View Modes; the
        // rotation itself is the pure `cycleGridMode` mutator. Same wiring
        // discipline as Alt+G: focus guard so the canvas doesn't rotate behind
        // an inline rename, cooldown so a held key can't burst-cycle. Guard
        // BEFORE the cooldown wrapper — a no-op press must not burn the window.
        if (isTextInputFocused()) return;
        cycleViewModeGuard(async () => {
          cycleGridMode();
        });
        return;
      }

      if (action === 'focus-grid-search') {
        // Issue #998 — Ctrl+F (Win/Linux) / ⌘+⌥+F (macOS) focuses the grid
        // search input. Since #1609 the search lives in the Filtered view
        // (its controls are hidden in other modes), so the shortcut first
        // lands the canvas there — matching every editor's "find in files"
        // gesture — then bumps the request counter. The actual `.focus()`
        // + `.select()` calls live in the `GridControls` component's
        // `useLayoutEffect` (which subscribes to
        // `useUIStore.focusGridSearchRequest`). A request counter is the
        // React-idiomatic channel for "imperative command from outside
        // the component tree" — no ref forwarding, no module-level
        // singleton, no leaky DOM registration. Ordering is guaranteed by
        // React, not by defensive no-ops: `setViewMode('filtered')` and
        // the counter bump happen in this same event handler, so React
        // re-renders ONCE (18 batching) with both values — TitleBar mounts
        // GridControls, the input ref attaches during commit, and
        // GridControls' `useLayoutEffect` runs after the DOM mutation but
        // before paint, reading the already-bumped counter. There is no
        // intermediate committed frame in which the input is absent, so
        // no press can be dropped; no "input not mounted yet" state
        // exists to defend against.
        //
        // No cooldown: a held key must not burn a window — the user
        // re-pressing while already focused is a harmless re-focus, and
        // the counter pattern (0 → 1 → 2) naturally fires the effect
        // on every distinct press.
        const ui = useUIStore.getState();
        if (ui.viewMode !== 'filtered') ui.setViewMode('filtered');
        ui.requestFocusGridSearch();
        return;
      }

      if (action === 'jump-to-next-awaiting') {
        // Issue #64 — Ctrl/Cmd+. cycles to the next agent node whose
        // `status === 'awaiting_input'` in the active node's mesh, wrapping
        // at the end. No focus guard: the user is most likely focused on an
        // xterm prompt (the reason they're noticing the agent needs input),
        // and `isTextInputFocused` carves out the xterm-helper-textarea so
        // this fires from a terminal prompt — exactly what we want. No
        // cooldown either: rapid presses are a legitimate "skip through"
        // gesture, and the cycle is purely synchronous so there's no IPC
        // round-trip to rate-limit.
        jumpToNextAwaitingNode();
        return;
      }

      if (action === 'open-omnibar' || action === 'open-omnibar-commands') {
        // Issue #1409 — ⌘/Ctrl+K and ⌘/Ctrl+P (Cmd on macOS, Ctrl+Shift on
        // Win/Linux — see the binding split above) open the Universal
        // Command Omnibar. The K/P distinction is carried in the action so
        // the palette can preselect its mode. `toggleOmnibar` gives the
        // universal "activation chord closes the open modal" behaviour.
        const mode = action === 'open-omnibar-commands' ? 'commands' : 'files';
        useUIStore.getState().toggleOmnibar(mode);
        return;
      }

      if (!isArrowAction(action)) return;
      // Defensive focus guard (matches the Alt+G handler above). The binding
      // is Ctrl/Cmd+Alt+Arrow, which has no readline collision, but if the
      // user is focused on a real text input (an inline rename, a future
      // search box) we'd rather not move the grid behind their typing. The
      // xterm-helper-textarea carve-out in isTextInputFocused() means this
      // guard is a no-op when an agent terminal has focus — exactly what we
      // want.
      if (isTextInputFocused()) return;
      // Throttle sits BELOW the focus guard: a held arrow inside a rename box
      // must not consume the budget, or the user's next real traversal press
      // (after they tab out of the input) would also be blocked.
      if (!arrowThrottle()) return;
      const direction: ArrowDirection = action.slice('arrow-'.length) as ArrowDirection;

      // Phase 1: in Single view mode (which subsumes the old maximize —
      // wayfinder #982), the first arrow press restores the grid AND keeps
      // focus on the soloed node. Single renders the active node, so
      // exiting the mode is all that's needed — activeNodeId already points
      // at the node the user was viewing.
      const ui = useUIStore.getState();
      if (ui.viewMode === 'single') {
        ui.exitSingleMode();
        return;
      }
      // Capture the narrowed mode here — right after the 'single' guard, before
      // any intervening call — so TS keeps `ui.viewMode` as NonSingleViewMode.
      const mode: NonSingleViewMode = ui.viewMode;

      // Phase 2: walk the grid the active View Mode actually renders. Ticket
      // #987 — `scopeNodesForMode` returns exactly the on-screen node set (Mesh
      // for the resolved mesh, Pinned cross-mesh, All, or Filtered narrowed by
      // the Grid Controls — #1609) in the store's
      // canonical (mesh_id, position) order, so traversal matches AgentNodeView
      // cell-for-cell. This replaces the old `mesh_id === activeNode.mesh_id`
      // filter, which stranded Ctrl+Alt+Arrow on the active node's mesh in
      // Pinned/All. `setActiveNode` only writes `activeNodeId` (not
      // `selectedMeshId`), so a cross-mesh hop in Pinned/All won't trip the
      // sidebar-sync subscription back into Mesh mode.
      const activeNode = useAgentNodeStore.getState().getActiveNode();
      if (!activeNode) return;
      // Issue #1384 — `getAgentNodes()` is the store's derived getter; it
      // allocates a fresh ordered array from `nodesById` + `nodeIds` on
      // each call. Cheaper than reading the old (now-removed) `agentNodes`
      // field, and matches the imperative-reader pattern used by
      // `awaitingInputShortcuts` / `meshStore.deleteMesh`.
      const agentNodes = useAgentNodeStore.getState().getAgentNodes();
      const selectedMeshId = useMeshStore.getState().selectedMeshId;
      // #1609 — pass the Grid Controls so traversal in Filtered walks the
      // same narrowed set the grid renders (the other scopes ignore them).
      const ownerships = useAgentNodeStore.getState().circuitOwnerships;
      const visibleNodes = groupActivityNodes(scopeNodesForMode(mode, agentNodes, selectedMeshId, activeNode.id, filteredControls()), agentNodes, ownerships);
      const targetId = traversalTargetId(visibleNodes, activityRootId(activeNode.id, agentNodes, ownerships), direction);
      if (targetId !== null) {
        useAgentNodeStore.getState().setActiveNode(targetId);
      }
    };

    window.addEventListener('shortcut-triggered', handleShortcut);
    return () => window.removeEventListener('shortcut-triggered', handleShortcut);
  }, []);

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
  // Three early-exit guards protect the user's keystroke:
  //   - `e.repeat`: a held `?` autorepeats at ~30 Hz after ~500 ms on
  //     Windows; without this guard the modal flickers open/closed for
  //     as long as the user keeps the key down.
  //   - `isTerminalFocused()`: typing `?` in an agent terminal is the
  //     user asking the agent a question — the `?` is content, not a
  //     help request. This is the inverse of the `isTextInputFocused`
  //     xterm carve-out used by Alt+G (which DOES want to fire from a
  //     terminal prompt).
  //   - `isTextInputFocused()`: catches inline-renames, the terminal
  //     search box, and any future text input — same as the other
  //     handlers.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== '?') return;
      if (e.repeat) return;
      if (isTerminalFocused()) return;
      if (isTextInputFocused()) return;
      e.preventDefault();
      cheatsheetGuard(async () => {
        useUIStore.getState().toggleCheatsheet();
      });
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  useEffect(() => {
    const unlisten = listen<ProviderErrorPayload>('provider-error', (event) => {
      addToast(event.payload.provider, event.payload.message, 'error');
    });
    return () => { unlisten.then((fn) => fn()); };
  }, [addToast]);

  useEffect(() => {
    if (storeError) {
      addToast('System', storeError, 'error');
    }
  }, [storeError, addToast]);

  // Issue #1250 — extract init into a callback so the BootErrorPanel's
  // Retry button can re-run it without unmounting the whole App.
  // Promise.allSettled (not Promise.all) so a single rejection doesn't
  // mask the other two outcomes — we need to know exactly which call
  // failed so the error message is meaningful. If one succeeds and
  // another fails, the partial state in the stores is fine: a retry
  // re-runs all three and overwrites whatever was loaded.
  const init = useCallback(async () => {
    setInitError(null);
    setInitBusy(true);
    // Issue #1501: hydrate the exit-confirm prompt preference. Deliberately
    // NOT in the `allSettled` gate below — a prefs-read failure must neither
    // block boot nor flip the guard off; the store keeps its fail-closed
    // `true` default and the guard decides synchronously from it.
    void useExitPromptStore.getState().initConfirmBeforeQuit();
    try {
      // No data dependency between these — run the IPC round-trips
      // concurrently so first paint isn't gated on three serial calls.
      const results = await Promise.allSettled([
        initAttentionListeners(),
        fetchMeshes(),
        fetchAgentNodes(),
      ]);
      const failures = results.filter(
        (r): r is PromiseRejectedResult => r.status === 'rejected',
      );
      if (failures.length > 0) {
        // Surface the first rejection. formatError strips the
        // "Error: " prefix that String(e) would otherwise prepend
        // (issue #663).
        setInitError(formatError(failures[0].reason));
        return;
      }
      setIsReady(true);

      // Auto-resume suspended sessions after a brief delay to ensure
      // terminals and event listeners are mounted
      setTimeout(async () => {
        try {
          const resumed = await api.autoResumeAgentNodes();
          if (resumed.length > 0) {
            console.log(`[App] Auto-resumed ${resumed.length} sessions`);
          }
          // Recovery can update identity even when the subsequent launch fails.
          await fetchAgentNodes();
        } catch (e) {
          console.error('[App] Auto-resume failed:', e);
        }
      }, 1000);
    } catch (e) {
      // Defense in depth: Promise.allSettled never rejects, but if a
      // future refactor swaps it back to Promise.all or any of these
      // calls throw synchronously, we still surface the error instead
      // of leaving the splash up forever.
      console.error('[App] Init failed:', e);
      setInitError(formatError(e));
    } finally {
      setInitBusy(false);
    }
  }, [initAttentionListeners, fetchMeshes, fetchAgentNodes]);

  useEffect(() => {
    init();
  }, [init]);

  useEffect(() => {
    const unlisten = listen<ResumeFailedPayload>('resume-failed', (event) => {
      addToast('Resume', `Node ${event.payload.node_id}: ${event.payload.error}`, 'warning');
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // The node closes instantly; if its worktree directory couldn't be removed in
  // the background, warn here. It stays queued and is retried on next launch.
  useEffect(() => {
    const unlisten = listen<WorktreeCleanupFailedPayload>(
      'worktree-cleanup-failed',
      (event) => {
        addToast(
          'Worktree',
          `Couldn't remove worktree for ${event.payload.node_name} — it'll be retried on next launch.`,
          'warning',
        );
      },
    );
    return () => { unlisten.then((fn) => fn()); };
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
  useEffect(() => {
    const unlisten = listen<MeshSyncWarningPayload>('mesh-sync-warning', (event) => {
      addToast('Sync', event.payload.message, 'warning');
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  useEffect(() => {
    const unlisten = listen<CircuitNotificationPayload>('circuit-notification', ({ payload }) => {
      const severity = payload.severity === 'success' || payload.severity === 'info'
        ? payload.severity
        : payload.severity === 'error' ? 'error' : 'warning';
      addToast(`Circuit #${payload.run_id}`, payload.message, severity);
    });
    return () => { void unlisten.then(fn => fn()); };
  }, []);

  // Autopilot lifecycle notifications (PRD #480 story 14). Same toast stack
  // as Sync/Worktree; the `Autopilot` label groups all three outcomes. The
  // node list refetch keeps status badges (Completed / Error) in step with
  // the backend's direct DB writes, which emit no dedicated status event.
  useEffect(() => {
    const unlistenBlocked = listen<AutopilotBlockedPayload>(
      'autopilot-blocked',
      (event) => {
        addToast(
          'Autopilot',
          `Agent on node ${event.payload.node_id} needs your input (issue #${event.payload.issue}).`,
          'warning',
        );
      },
    );
    const unlistenPr = listen<AutopilotPrCreatedPayload>(
      'autopilot-pr-created',
      (event) => {
        addToast(
          'Autopilot',
          event.payload.pr_url
            ? `Wrap-up complete — PR opened: ${event.payload.pr_url}`
            : `Wrap-up complete for node ${event.payload.node_id}.`,
          'warning',
        );
        void useAgentNodeStore.getState().fetchAgentNodes();
      },
    );
    const unlistenFailed = listen<AutopilotFinishFailedPayload>(
      'autopilot-finish-failed',
      (event) => {
        addToast(
          'Autopilot',
          `Node ${event.payload.node_id} failed its wrap-up after 3 attempts: ${event.payload.reasons.join('; ')}`,
          'error',
        );
        void useAgentNodeStore.getState().fetchAgentNodes();
      },
    );
    // Launch watcher pressed Enter — the agent has actually started the
    // task (the prefill alone only stages it). No refetch needed: nothing
    // about the node row changed.
    const unlistenSubmitted = listen<AutopilotSubmittedPayload>(
      'autopilot-submitted',
      (event) => {
        addToast(
          'Autopilot',
          `Started work on issue #${event.payload.issue} (node ${event.payload.node_id}).`,
          'warning',
        );
      },
    );
    // Merged-PR sweep archived a finished node (the store refetches the
    // node list on this same event; here we just tell the user why a card
    // vanished from the grid).
    const unlistenClosed = listen<AutopilotNodeClosedPayload>(
      'autopilot-node-closed',
      (event) => {
        addToast(
          'Autopilot',
          `PR #${event.payload.pr_number} merged — node ${event.payload.node_id} closed.`,
          'warning',
        );
      },
    );
    return () => {
      unlistenBlocked.then((fn) => fn());
      unlistenPr.then((fn) => fn());
      unlistenFailed.then((fn) => fn());
      unlistenSubmitted.then((fn) => fn());
      unlistenClosed.then((fn) => fn());
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
                className="shrink-0 -m-1 p-1 rounded-md text-text-secondary hover:text-text-primary hover:bg-white/10 text-base leading-none transition-colors"
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
