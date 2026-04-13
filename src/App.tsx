import { useEffect } from 'react';
import { Sidebar } from './components/Sidebar/Sidebar';
import { SessionView } from './components/SessionView/SessionView';
import { useProjectStore } from './stores/projectStore';
import { useWorkspaceStore } from './stores/workspaceStore';
import './App.css';

function App() {
  const { fetchProjects } = useProjectStore();
  const { fetchWorkspaces } = useWorkspaceStore();

  useEffect(() => {
    fetchProjects();
    fetchWorkspaces();
  }, []);

  return (
    <div className="flex h-screen w-screen overflow-hidden">
      <Sidebar />
      <SessionView />
    </div>
  );
}

export default App;
