import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
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

function App() {
  const { fetchProjects } = useProjectStore();
  const { fetchSessions } = useSessionStore();
  const [toasts, setToasts] = useState<ErrorToast[]>([]);

  useEffect(() => {
    fetchProjects();
    fetchSessions();

    const unlisten = listen<{ provider: string; message: string }>('provider-error', (event) => {
      const toast: ErrorToast = {
        id: Date.now(),
        provider: event.payload.provider,
        message: event.payload.message,
      };
      setToasts((prev) => [...prev, toast]);
      setTimeout(() => {
        setToasts((prev) => prev.filter((t) => t.id !== toast.id));
      }, 5000);
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const dismissToast = (id: number) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  };

  return (
    <div className="flex h-screen w-screen overflow-hidden">
      <Sidebar />
      <SessionView />
      {toasts.map((toast) => (
        <div
          key={toast.id}
          className="fixed bottom-4 right-4 bg-red-600 text-white px-4 py-3 rounded-lg shadow-lg flex items-center gap-3 max-w-md z-50"
        >
          <span className="font-semibold">{(toast.provider).toUpperCase()} Error:</span>
          <span className="text-sm">{toast.message}</span>
          <button
            onClick={() => dismissToast(toast.id)}
            className="ml-2 text-white/70 hover:text-white text-lg leading-none"
          >
            ×
          </button>
        </div>
      ))}
    </div>
  );
}

export default App;
