import { useEffect, useState, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { register, unregister, isRegistered } from '@tauri-apps/plugin-global-shortcut';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Sidebar } from './components/Sidebar/Sidebar';
import { AgentNodeView } from './components/AgentNodeView/AgentNodeView';
import { ProbePanel } from './components/Probe/ProbePanel';
import { WorktreeCloseDialog } from './components/WorktreeCloseDialog/WorktreeCloseDialog';
import { useMeshStore } from './stores/meshStore';
import { useAgentNodeStore } from './stores/agentNodeStore';
import { useUIStore } from './stores/uiStore';
import { createShortcutGuard } from './lib/shortcutGuard';
import { isTextInputFocused } from './lib/focusGuard';
import { arrowTargetIndex } from './lib/gridTraversal';
import { toggleGridMaximize } from './lib/gridShortcuts';
import { isMac } from './lib/platform';
import { getGridRows } from './hooks/useGridLayout';
import { useFileDropToTerminal } from './hooks/useFileDropToTerminal';
import * as api from './lib/tauri';
import {
  applyToastCap,
  dedupToasts,
  TOAST_DEDUP_TTL_MS,
  TOAST_MAX,
  TOAST_TTL_MS,
  type Toast,
  type ToastSeverity,
} from './lib/toastUtils';
import './App.css';

const createNodeGuard = createShortcutGuard(300);
// Cooldown for the Alt+G / Cmd+G grid-toggle (issue #668). Same 300ms budget
// as the new-agent guard: long enough that holding the key doesn't fire a
// burst of toggles, short enough that a deliberate second press feels
// responsive.
const toggleMaximizeGuard = createShortcutGuard(300);

type ErrorToast = Toast;

function App() {
  const { fetchMeshes } = useMeshStore();
  const { fetchAgentNodes, initAttentionListeners } = useAgentNodeStore();
  const storeError = useAgentNodeStore(state => state.error);

  const [toasts, setToasts] = useState<ErrorToast[]>([]);
  const [isReady, setIsReady] = useState(false);

  // Track window focus state for conditional shortcut handling
  const isFocusedRef = useRef(false);

  // Paste absolute file paths into the hovered agent terminal on OS file drop.
  useFileDropToTerminal();

  // Keyboard shortcuts — use Tauri's globalShortcut plugin so they work even when
  // an xterm.js terminal has keyboard focus (xterm intercepts window keydown events).
  // Only register shortcuts when the window is focused so they don't steal from other apps.
  // Ctrl/Cmd+Alt+←/→/↑/↓ traverses the on-screen agent-node grid in the active mesh.
  // We use TWO modifiers here (not bare Ctrl/Cmd+Arrow) because Ctrl+←/→ is
  // the readline `backward-word` / `forward-word` gesture in bash/zsh/fish/
  // PSReadLine and every node/python REPL — the global-shortcut plugin
  // captures at the OS layer (before xterm sees the keydown), so bare
  // Ctrl+Arrow would silently steal word-movement whenever the user tries to
  // fix a typo in a long agent prompt. Adding `Alt+` matches the Windows
  // Snap / tmux prefix+Arrow / i3 / VS Code "Move Editor Group" precedent
  // for pane navigation and collides with nothing.
  // Alt+G (Win/Linux) / Cmd+G (macOS) toggles grid ↔ single-view (issue #668).
  useEffect(() => {
    // Tauri 2's global-shortcut plugin doesn't expose an `AltOrCommand`
    // combinator (only `CommandOrControl`, which means Cmd on Mac and Ctrl
    // elsewhere). Issue #668 explicitly wants `Alt+G` on Windows/Linux —
    // the mnemonic "G for Grid" — and `Cmd+G` on macOS. So we register
    // the platform-appropriate binding only, branched on `isMac`. The
    // handler below is platform-agnostic: same action, same store calls.
    const gridToggleShortcut = isMac
      ? { key: 'CommandOrControl+G', action: 'toggle-maximize-grid' as const }
      : { key: 'Alt+G', action: 'toggle-maximize-grid' as const };

    const shortcuts = [
      { key: 'CommandOrControl+T', action: 'new-agent' },
      { key: 'CommandOrControl+Alt+ArrowLeft', action: 'arrow-left' },
      { key: 'CommandOrControl+Alt+ArrowRight', action: 'arrow-right' },
      { key: 'CommandOrControl+Alt+ArrowUp', action: 'arrow-up' },
      { key: 'CommandOrControl+Alt+ArrowDown', action: 'arrow-down' },
      gridToggleShortcut,
    ];
    const shortcutByKey = new Map(shortcuts.map(s => [s.key, s.action]));

    const handleShortcut = (action: string) => {
      window.dispatchEvent(new CustomEvent('shortcut-triggered', { detail: action }));
    };

    let unlistenFocus: (() => void) | null = null;

    const setupWindowTracking = async () => {
      const win = getCurrentWindow();
      isFocusedRef.current = await win.isFocused();

      unlistenFocus = await win.onFocusChanged(async ({ payload: focused }) => {
        isFocusedRef.current = focused;

        const ops = shortcuts.map(async ({ key }) => {
          try {
            if (focused) {
              if (!(await isRegistered(key))) {
                const action = shortcutByKey.get(key);
                await register(key, () => {
                  if (!isFocusedRef.current) return;
                  if (action) handleShortcut(action);
                });
              }
            } else {
              await unregister(key);
            }
          } catch (e) {
            console.warn(`Failed to update shortcut ${key} on focus change:`, e);
          }
        });
        await Promise.all(ops);
      });
    };

    const registerShortcuts = async () => {
      const ops = shortcuts.map(async ({ key }) => {
        try {
          if (!(await isRegistered(key))) {
            const action = shortcutByKey.get(key);
            await register(key, () => {
              if (!isFocusedRef.current) return;
              if (action) handleShortcut(action);
            });
          }
        } catch (e) {
          console.warn(`Failed to register shortcut ${key}:`, e);
        }
      });
      await Promise.all(ops);
    };

    setupWindowTracking();
    registerShortcuts();

    return () => {
      if (unlistenFocus) unlistenFocus();
      for (const { key } of shortcuts) {
        unregister(key).catch(() => {});
      }
    };
  }, []);

  // Handle shortcut events emitted from Rust (Ctrl+T new-agent; Ctrl/Cmd+Arrow for
  // node traversal). Arrow traversal works in two phases: if a node is currently
  // maximized, the first arrow press restores the grid AND moves focus to the
  // maximized node (so the user "exits" the solo view onto the node they were
  // actually viewing, not whatever was active before the zoom); on the next press
  // (and beyond) we walk the on-screen grid from that node. Edge semantics are
  // defined in `arrowTargetIndex` (src/lib/gridTraversal.ts): Left/Right wrap
  // within the row, Up/Down are no-ops at the grid's vertical edges.
  //
  // Note: this differs from the Escape-to-un-maximize path in AgentNodeView.tsx
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
        createNodeGuard(async () => {
          const activeNode = useAgentNodeStore.getState().getActiveNode();
          const meshId = activeNode?.mesh_id ?? useMeshStore.getState().selectedMeshId;
          if (!meshId) return;
          const mesh = useMeshStore.getState().meshesById.get(meshId);
          if (!mesh) return;
          const provider = activeNode?.provider ?? 'anthropic';
          // Route through the same store action the sidebar uses (issue #283)
          // so the create→activate→select-mesh invariant ("no half-applied
          // mesh-selected-but-no-node state") lives in exactly one place.
          // The action uses branch='main'; the keyboard shortcut previously
          // copied activeNode?.branch — branch wasn't actually wired through
          // create_session here, since the sidebar's spawn path already
          // passes 'main' too.
          await useAgentNodeStore
            .getState()
            .selectProviderForMesh(mesh.id, mesh.name, mesh.path, provider, undefined);
        });
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

      if (!isArrowAction(action)) return;
      // Defensive focus guard (matches the Alt+G handler above). The binding
      // is Ctrl/Cmd+Alt+Arrow, which has no readline collision, but if the
      // user is focused on a real text input (an inline rename, a future
      // search box) we'd rather not move the grid behind their typing. The
      // xterm-helper-textarea carve-out in isTextInputFocused() means this
      // guard is a no-op when an agent terminal has focus — exactly what we
      // want.
      if (isTextInputFocused()) return;
      const direction: ArrowDirection = action.slice('arrow-'.length) as ArrowDirection;

      // Phase 1: if a node is maximized, the first arrow press restores the
      // grid AND moves focus to the maximized node. Capturing the id BEFORE
      // clearMaximizedNode() (which nulls it) is what lets the subsequent
      // setActiveNode see the right id.
      const maximizedId = useUIStore.getState().maximizedNodeId;
      if (maximizedId !== null) {
        useUIStore.getState().clearMaximizedNode();
        useAgentNodeStore.getState().setActiveNode(maximizedId);
        return;
      }

      // Phase 2: walk the active mesh's grid. Sort by `position` because that's
      // the on-screen order the grid renders in (matches `list_agent_nodes`).
      const activeNode = useAgentNodeStore.getState().getActiveNode();
      if (!activeNode) return;
      const meshNodes = useAgentNodeStore.getState().agentNodes
        .filter(s => s.mesh_id === activeNode.mesh_id)
        .sort((a, b) => a.position - b.position);
      const currentIndex = meshNodes.findIndex(s => s.id === activeNode.id);
      if (currentIndex === -1) return;

      const rows = getGridRows(meshNodes.length);
      const targetIndex = arrowTargetIndex(currentIndex, direction, rows);
      const target = meshNodes[targetIndex];
      if (target && target.id !== activeNode.id) {
        useAgentNodeStore.getState().setActiveNode(target.id);
      }
    };

    window.addEventListener('shortcut-triggered', handleShortcut);
    return () => window.removeEventListener('shortcut-triggered', handleShortcut);
  }, []);

  useEffect(() => {
    const unlisten = listen<{ provider: string; message: string }>('provider-error', (event) => {
      addToast(event.payload.provider, event.payload.message, 'error');
    });
    return () => { unlisten.then((fn) => fn()); };
  }, [setToasts]);

  useEffect(() => {
    if (storeError) {
      addToast('System', storeError, 'error');
    }
  }, [storeError, setToasts]);

  useEffect(() => {
    const init = async () => {
      try {
        // No data dependency between these — run the IPC round-trips
        // concurrently so first paint isn't gated on three serial calls.
        await Promise.all([initAttentionListeners(), fetchMeshes(), fetchAgentNodes()]);
        setIsReady(true);

        // Auto-resume suspended sessions after a brief delay to ensure
        // terminals and event listeners are mounted
        setTimeout(async () => {
          try {
            const resumed = await api.autoResumeAgentNodes();
            if (resumed.length > 0) {
              console.log(`[App] Auto-resumed ${resumed.length} sessions`);
              await fetchAgentNodes();
            }
          } catch (e) {
            console.error('[App] Auto-resume failed:', e);
          }
        }, 1000);
      } catch (e) {
        console.error('[App] Init failed:', e);
      }
    };
    init();
  }, []);

  useEffect(() => {
    const unlisten = listen<{ node_id: number; error: string }>('resume-failed', (event) => {
      addToast('Resume', `Node ${event.payload.node_id}: ${event.payload.error}`, 'warning');
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // The node closes instantly; if its worktree directory couldn't be removed in
  // the background, warn here. It stays queued and is retried on next launch.
  useEffect(() => {
    const unlisten = listen<{ node_name: string; worktree_path: string; error: string }>(
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
    const unlisten = listen<{
      node_id: number;
      mesh_path: string;
      outcome: 'diverged' | 'fetch_failed' | 'repo_unusable' | 'pr_head_unfetchable' | 'pr_sha_drift';
      new_commits?: number;
      message: string;
    }>('mesh-sync-warning', (event) => {
      addToast('Sync', event.payload.message, 'warning');
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // Auto-dismiss toasts after TOAST_TTL_MS. A 1s tick is coarse
  // enough that it won't fight React's render cycle, fine enough
  // that the user sees the toast disappear in real time.
  useEffect(() => {
    if (toasts.length === 0) return;
    const tick = () => {
      setToasts((prev) => prev.filter((t) => Date.now() - t.createdAt < TOAST_TTL_MS));
    };
    const interval = setInterval(tick, 1000);
    return () => clearInterval(interval);
  }, [toasts.length]);

  const addToast = (provider: string, message: string, severity: ToastSeverity = 'error') => {
    const now = Date.now();
    const incoming: ErrorToast = { id: now, provider, message, createdAt: now, severity };
    setToasts((prev) => applyToastCap(dedupToasts(prev, incoming, now, TOAST_DEDUP_TTL_MS), TOAST_MAX));
  };

  const dismissToast = (id: number) => {
    setToasts(prev => prev.filter(t => t.id !== id));
  };

  if (!isReady) {
    return (
      <div
        role="status"
        aria-label="Loading Buildmesh"
        className="flex h-screen w-screen items-center justify-center bg-bg-base"
      >
        <div className="text-accent-cyan text-2xl animate-pulse">●</div>
      </div>
    );
  }

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-bg-base text-text-primary">
      <Sidebar />
      <AgentNodeView />
      <ProbePanel />

      <WorktreeCloseDialog />

      {/* Toast notifications. Each toast carries role="status" (implicit
          aria-live=polite) so screen readers announce it on arrival without
          moving focus — the container itself stays silent to avoid double
          announcements. */}
      <div className="fixed bottom-32 right-4 flex flex-col gap-2 z-50">
        {toasts.map((toast) => {
          const isWarning = toast.severity === 'warning';
          return (
            <div
              key={toast.id}
              role="status"
              className={`animate-slide-in-right bg-bg-surface border px-4 py-3 rounded-md flex items-start gap-2 min-w-[280px] max-w-[420px] shadow-md ${
                isWarning ? 'border-status-warning/50' : 'border-status-error/50'
              }`}
            >
              <div className="flex-1 min-w-0">
                <div
                  className={`text-2xs font-bold uppercase ${
                    isWarning ? 'text-status-warning' : 'text-status-error'
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
