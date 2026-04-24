import { useEffect } from 'react';
import { Sidebar } from './components/Sidebar/Sidebar';
import { SessionView } from './components/SessionView/SessionView';
import { useProjectStore } from './stores/projectStore';
import { useSessionStore } from './stores/sessionStore';
import './App.css';

function App() {
  const { fetchProjects } = useProjectStore();
  const { fetchSessions } = useSessionStore();

  useEffect(() => {
    fetchProjects();
    fetchSessions();
  }, []);

  return (
    <div className="flex h-screen w-screen overflow-hidden">
      <Sidebar />
      <SessionView />
    </div>
  );
}

export default App;
