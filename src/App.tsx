import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { register, unregister, isRegistered } from '@tauri-apps/plugin-global-shortcut';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Sidebar } from './components/Sidebar/Sidebar';
import { SessionView } from './components/SessionView/SessionView';
import { MeshPropertiesPanel } from './components/MeshPropertiesPanel/MeshPropertiesPanel';
import { useMeshStore } from './stores/meshStore';
import { useAgentNodeStore } from './stores/agentNodeStore';
import { useUIStore } from './stores/uiStore';
import { createShortcutGuard } from './lib/shortcutGuard';
import './App.css';

const createNodeGuard = createShortcutGuard(300);

interface ErrorToast {
  id: number;
  provider: string;
  message: string;
}

function App() {
  const { fetchMeshes } = useMeshStore();
  const { fetchAgentNodes, initAttentionListeners } = useAgentNodeStore();
  const storeError = useAgentNodeStore(state => state.error);

  const [toasts, setToasts] = useState<ErrorToast[]>([]);
  const [isReady, setIsReady] = useState(false);
  const propertiesPanelMeshId = useUIStore((s) => s.propertiesPanelMeshId);

  // Keyboard shortcuts — use Tauri's globalShortcut plugin so they work even when
  // an xterm.js terminal has keyboard focus (xterm intercepts window keydown events)
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

    const registerShortcuts = async () => {
      for (const { key, action } of shortcuts) {
        try {
          if (!(await isRegistered(key))) {
            await register(key, async () => {
              const focused = await getCurrentWindow().isFocused();
              if (!focused) return;
              window.dispatchEvent(new CustomEvent('shortcut-triggered', { detail: action }));
            });
          }
        } catch (e) {
          console.warn(`Failed to register shortcut ${key}:`, e);
        }
      }
    };

    registerShortcuts();

    return () => {
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
          const branch = activeNode?.branch ?? 'main';
          const path = activeNode?.path ?? mesh.path;
          const node = await useAgentNodeStore.getState().createAgentNode(mesh.id, mesh.name, path, branch, provider);
          useAgentNodeStore.getState().setActiveNode(node.id);
          useMeshStore.getState().selectMesh(mesh.id);
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
      const toast: ErrorToast = {
        id: Date.now(),
        provider: event.payload.provider,
        message: event.payload.message,
      };
      setToasts((prev) => [...prev, toast]);
    });
    return () => { unlisten.then((fn) => fn()); };
  }, [setToasts]);

  useEffect(() => {
    if (storeError) {
      const toast: ErrorToast = { id: Date.now(), provider: 'System', message: storeError };
      setToasts((prev) => [...prev, toast]);
    }
  }, [storeError, setToasts]);

  useEffect(() => {
    const init = async () => {
      try {
        await initAttentionListeners();
        await fetchMeshes();
        await fetchAgentNodes();
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
      const toast: ErrorToast = {
        id: Date.now(),
        provider: 'Resume',
        message: `Session ${event.payload.session_id}: ${event.payload.error}`,
      };
      setToasts((prev) => [...prev, toast]);
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

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

      {propertiesPanelMeshId != null && <MeshPropertiesPanel />}

      {/* Toast notifications */}
      <div className="fixed bottom-32 right-4 flex flex-col gap-2 z-50">
        {toasts.map((toast) => (
          <div key={toast.id} className="bg-[#0d0d16] border border-[#ef4444]/50 text-white px-4 py-3 rounded flex items-center gap-2">
            <div className="flex-1">
              <div className="text-[10px] font-bold text-red-500 uppercase">{toast.provider} Error</div>
              <div className="text-xs text-[#94a3b8]">{toast.message}</div>
            </div>
            <button onClick={() => dismissToast(toast.id)} className="text-white/50 hover:text-white">&times;</button>
          </div>
        ))}
      </div>
    </div>
  );
}

export default App;
