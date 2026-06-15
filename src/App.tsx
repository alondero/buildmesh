import { useEffect, useState, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { register, unregister, isRegistered } from '@tauri-apps/plugin-global-shortcut';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Sidebar } from './components/Sidebar/Sidebar';
import { SessionView } from './components/SessionView/SessionView';
import { ProbePanel } from './components/Probe/ProbePanel';
import { WorktreeCloseDialog } from './components/WorktreeCloseDialog/WorktreeCloseDialog';
import { useMeshStore } from './stores/meshStore';
import { useAgentNodeStore } from './stores/agentNodeStore';
import { createShortcutGuard } from './lib/shortcutGuard';
import { useFileDropToTerminal } from './hooks/useFileDropToTerminal';
import {
  applyToastCap,
  dedupToasts,
  TOAST_DEDUP_TTL_MS,
  TOAST_MAX,
  TOAST_TTL_MS,
  type Toast,
} from './lib/toastUtils';
import './App.css';

const createNodeGuard = createShortcutGuard(300);

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
  useEffect(() => {
    const shortcuts = [
      { key: 'CommandOrControl+T', action: 'new-agent' },
      { key: 'CommandOrControl+1', action: 'switch-1' },
      { key: 'CommandOrControl+2', action: 'switch-2' },
      { key: 'CommandOrControl+3', action: 'switch-3' },
      { key: 'CommandOrControl+4', action: 'switch-4' },
      { key: 'CommandOrControl+5', action: 'switch-5' },
      { key: 'CommandOrControl+6', action: 'switch-6' },
      { key: 'CommandOrControl+7', action: 'switch-7' },
      { key: 'CommandOrControl+8', action: 'switch-8' },
      { key: 'CommandOrControl+9', action: 'switch-9' },
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

  // Quick switch session: Alt+1..9 (not intercepted by xterm, so window listener is fine)
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.altKey && /^[1-9]$/.test(e.key)) {
        const index = parseInt(e.key) - 1;
        const currentNodes = useAgentNodeStore.getState().agentNodes.filter(s =>
          s.mesh_id === useAgentNodeStore.getState().getActiveNode()?.mesh_id
        );
        if (currentNodes[index]) {
          useAgentNodeStore.getState().setActiveNode(currentNodes[index].id);
        }
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, []);

  // Handle shortcut events emitted from Rust (Ctrl+T, Ctrl+1..9)
  useEffect(() => {
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
      } else if (action.startsWith('switch-')) {
        const index = parseInt(action.replace('switch-', '')) - 1;
        const currentNodes = useAgentNodeStore.getState().agentNodes.filter(s =>
          s.mesh_id === useAgentNodeStore.getState().getActiveNode()?.mesh_id
        );
        if (currentNodes[index]) {
          useAgentNodeStore.getState().setActiveNode(currentNodes[index].id);
        }
      }
    };

    window.addEventListener('shortcut-triggered', handleShortcut);
    return () => window.removeEventListener('shortcut-triggered', handleShortcut);
  }, []);

  useEffect(() => {
    const unlisten = listen<{ provider: string; message: string }>('provider-error', (event) => {
      addToast(event.payload.provider, event.payload.message);
    });
    return () => { unlisten.then((fn) => fn()); };
  }, [setToasts]);

  useEffect(() => {
    if (storeError) {
      addToast('System', storeError);
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
            const resumed = await invoke<number[]>('auto_resume_sessions');
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
    const unlisten = listen<{ session_id: number; error: string }>('resume-failed', (event) => {
      addToast('Resume', `Session ${event.payload.session_id}: ${event.payload.error}`);
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
      session_id: number;
      mesh_path: string;
      outcome: 'diverged' | 'fetch_failed' | 'repo_unusable';
      new_commits?: number;
      message: string;
    }>('mesh-sync-warning', (event) => {
      addToast('Sync', event.payload.message);
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

  const addToast = (provider: string, message: string) => {
    const now = Date.now();
    const incoming: ErrorToast = { id: now, provider, message, createdAt: now };
    setToasts((prev) => applyToastCap(dedupToasts(prev, incoming, now, TOAST_DEDUP_TTL_MS), TOAST_MAX));
  };

  const dismissToast = (id: number) => {
    setToasts(prev => prev.filter(t => t.id !== id));
  };

  if (!isReady) {
    return (
      <div className="flex h-screen w-screen items-center justify-center bg-[#09090f]">
        <div className="text-[#00d4ff] text-2xl animate-pulse">●</div>
      </div>
    );
  }

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-[#09090f] text-[#e0e0e0]">
      <Sidebar />
      <SessionView />
      <ProbePanel />

      {/* Mesh Properties is now the Probe Panel's ⚙️ tab (issue #375).
          The legacy right-rail drawer is preserved on disk for the
          transition period — its `DEFAULT_WORKTREE_MODE` constant is still
          pinned by `tests/unit/mesh-properties-worktree-mode-default.test.ts`
          — but no longer mounted here. The store action
          `openPropertiesPanel` is kept for the same reason and can be
          removed in a follow-up once the file is deleted. */}
      <WorktreeCloseDialog />

      {/* Toast notifications */}
      <div className="fixed bottom-32 right-4 flex flex-col gap-2 z-50">
        {toasts.map((toast) => (
          <div
            key={toast.id}
            className="bg-[#0d0d16] border border-[#ef4444]/50 text-white px-4 py-3 rounded flex items-start gap-2 min-w-[280px] max-w-[420px] shadow-lg"
          >
            <div className="flex-1 min-w-0">
              <div className="text-[10px] font-bold text-red-500 uppercase">{toast.provider} Error</div>
              <div className="text-xs text-[#94a3b8] break-words">{toast.message}</div>
            </div>
            <button
              type="button"
              onClick={() => dismissToast(toast.id)}
              aria-label="Dismiss notification"
              className="shrink-0 -m-1 p-1 rounded text-white/60 hover:text-white hover:bg-white/10 text-base leading-none"
            >
              ×
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}

export default App;
