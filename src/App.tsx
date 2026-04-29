import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { Sidebar } from './components/Sidebar/Sidebar';
import { SessionView } from './components/SessionView/SessionView';
import { useProjectStore } from './stores/projectStore';
import { useSessionStore } from './stores/sessionStore';
import './App.css';

interface ErrorToast {
  id: number;
  provider: string;
  message: string;
}

interface BackendAgentState {
  session_id: number;
  is_alive: boolean;
}

function App() {
  const { fetchProjects } = useProjectStore();
  const { fetchSessions, initAttentionListeners } = useSessionStore();
  const storeError = useSessionStore(state => state.error);
  
  const [toasts, setToasts] = useState<ErrorToast[]>([]);
  const [isReady, setIsReady] = useState(false);
  const [backendAgents, setBackendAgents] = useState<BackendAgentState[]>([]);
  const [eventCounts, setEventCounts] = useState<Record<number, number>>({});
  const [uiErrors, setUiErrors] = useState<string[]>([]);

  const [_showDebug, setShowDebug] = useState(false);

  // Keyboard shortcuts
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      // Toggle Debug: Ctrl+Alt+D
      if (e.ctrlKey && e.altKey && e.key === 'd') {
        setShowDebug(prev => !prev);
      }

      // Quick switch session: Alt+1..9
      if (e.altKey && /^[1-9]$/.test(e.key)) {
        const index = parseInt(e.key) - 1;
        const currentSessions = useSessionStore.getState().sessions.filter(s =>
          s.project_id === useSessionStore.getState().getActiveSession()?.project_id
        );
        if (currentSessions[index]) {
          useSessionStore.getState().setActiveSession(currentSessions[index].id);
        }
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, []);

  useEffect(() => {
    const unlisten = listen<{ session_id: number; line: string }>('agent-output', (event) => {
      setEventCounts(prev => ({
        ...prev,
        [event.payload.session_id]: (prev[event.payload.session_id] || 0) + event.payload.line.length
      }));
    });
    return () => { unlisten.then(f => f()); };
  }, []);

  useEffect(() => {
    const timer = setInterval(async () => {
      try {
        const agents = await invoke<BackendAgentState[]>('debug_list_agents');
        setBackendAgents(agents);
      } catch (e) { 
        setUiErrors(prev => [...prev.slice(-4), `Backend Fetch Error: ${e}`]);
      }
    }, 2000);
    return () => clearInterval(timer);
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
        await fetchProjects();
        await fetchSessions();
        setIsReady(true);
      } catch (e) {
        setUiErrors(prev => [...prev, `Init Error: ${e}`]);
      }
    };
    init();
  }, []);

  const dismissToast = (id: number) => {
    setToasts(prev => prev.filter(t => t.id !== id));
  };

  if (!isReady && uiErrors.length === 0) {
    return (
      <div className="flex h-screen w-screen items-center justify-center bg-[#0f0f0f]">
        <div className="text-[#3b82f6] text-2xl animate-pulse">●</div>
      </div>
    );
  }

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-[#0a0a0a] text-[#e0e0e0]">
      <Sidebar />
      <SessionView />
      
      {/* DEBUG OVERLAY */}
      <div className="fixed bottom-0 left-0 right-0 bg-black/90 border-t border-red-500/30 flex flex-col z-[100] font-mono text-[10px]">
        <div className="h-8 flex items-center px-4 gap-4">
          <span className="text-red-500 font-bold">SYSTEM DEBUG:</span>
          <div className="flex gap-3">
            {backendAgents.map(a => (
              <div key={a.session_id} className="flex items-center gap-1">
                <span className={a.is_alive ? 'text-green-500' : 'text-gray-500'}>
                  ID:{a.session_id} {a.is_alive ? 'RUN' : 'DEAD'}
                </span>
                <span className="text-[#888]">({eventCounts[a.session_id] || 0} bytes)</span>
              </div>
            ))}
          </div>
          <button onClick={() => window.location.reload()} className="ml-auto bg-red-900/40 px-2 py-0.5 rounded text-red-200">RELOAD</button>
        </div>
        
        {uiErrors.length > 0 && (
          <div className="p-2 bg-red-950/20 border-t border-red-900/30 max-h-24 overflow-y-auto">
            {uiErrors.map((err, i) => (
              <div key={i} className="text-red-400 border-b border-red-900/10 py-0.5">{err}</div>
            ))}
          </div>
        )}
      </div>

      {/* Toast notifications */}
      <div className="fixed bottom-32 right-4 flex flex-col gap-2 z-50">
        {toasts.map((toast) => (
          <div key={toast.id} className="bg-[#1a1a1a] border border-red-500/50 text-white px-4 py-3 rounded flex items-center gap-2">
            <div className="flex-1">
              <div className="text-[10px] font-bold text-red-500 uppercase">{toast.provider} Error</div>
              <div className="text-xs text-[#ccc]">{toast.message}</div>
            </div>
            <button onClick={() => dismissToast(toast.id)} className="text-white/50 hover:text-white">&times;</button>
          </div>
        ))}
      </div>
    </div>
  );
}

export default App;
