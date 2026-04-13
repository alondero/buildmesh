import { useProjectStore } from '../stores/projectStore';
import { useWorkspaceStore } from '../stores/workspaceStore';
import type { Project } from '../stores/projectStore';
import type { Workspace } from '../stores/workspaceStore';

export function Sidebar() {
  const { projects, selectedProjectId, selectProject, createProject, fetchProjects } = useProjectStore();
  const { workspaces, setActiveWorkspace, activeWorkspaceId, createWorkspace } = useWorkspaceStore();

  const selectedWorkspaces = workspaces.filter(w => {
    if (!selectedProjectId) return true;
    return w.project_id === selectedProjectId;
  });

  const handleNewProject = async () => {
    const name = prompt('Project name:');
    if (!name) return;
    const path = prompt('Project path:');
    if (!path) return;
    await createProject(name, path);
  };

  const handleNewWorkspace = async () => {
    if (!selectedProjectId) {
      alert('Select a project first');
      return;
    }
    const name = prompt('Workspace name:');
    if (!name) return;
    const path = prompt('Workspace path:');
    if (!path) return;
    const branch = prompt('Branch (default: main):') || 'main';
    await createWorkspace(selectedProjectId, name, path, branch);
  };

  return (
    <div className="w-64 bg-[#111] border-r border-[#2a2a2a] flex flex-col h-full">
      {/* Header */}
      <div className="p-3 border-b border-[#2a2a2a]">
        <h1 className="text-sm font-semibold text-[#e0e0e0]">Conductor Clone</h1>
      </div>

      {/* Projects list */}
      <div className="flex-1 overflow-y-auto">
        <div className="p-2">
          <div className="flex items-center justify-between mb-1 px-2">
            <span className="text-xs font-medium text-[#888] uppercase">Projects</span>
            <button
              onClick={handleNewProject}
              className="text-xs text-[#3b82f6] hover:text-[#60a5fa]"
            >
              + New
            </button>
          </div>

          {projects.map(project => (
            <ProjectItem
              key={project.id}
              project={project}
              isSelected={selectedProjectId === project.id}
              onSelect={() => selectProject(selectedProjectId === project.id ? null : project.id)}
            />
          ))}

          {projects.length === 0 && (
            <p className="text-xs text-[#666] px-2 py-2">No projects yet</p>
          )}
        </div>

        {/* Workspaces section */}
        <div className="p-2 border-t border-[#2a2a2a]">
          <div className="flex items-center justify-between mb-1 px-2">
            <span className="text-xs font-medium text-[#888] uppercase">Workspaces</span>
            <button
              onClick={handleNewWorkspace}
              className="text-xs text-[#3b82f6] hover:text-[#60a5fa]"
            >
              + New
            </button>
          </div>

          {selectedWorkspaces.map(ws => (
            <WorkspaceItem
              key={ws.id}
              workspace={ws}
              isActive={activeWorkspaceId === ws.id}
              onSelect={() => setActiveWorkspace(ws.id)}
            />
          ))}

          {selectedWorkspaces.length === 0 && (
            <p className="text-xs text-[#666] px-2 py-2">No workspaces</p>
          )}
        </div>
      </div>

      {/* Footer */}
      <div className="p-2 border-t border-[#2a2a2a] text-xs text-[#666]">
        <span>{workspaces.filter(w => w.status === 'running').length} active</span>
      </div>
    </div>
  );
}

function ProjectItem({ project, isSelected, onSelect }: {
  project: Project;
  isSelected: boolean;
  onSelect: () => void;
}) {
  return (
    <div
      onClick={onSelect}
      className={`
        px-2 py-1.5 rounded cursor-pointer text-sm mb-0.5
        ${isSelected ? 'bg-[#1e3a5f] text-[#60a5fa]' : 'text-[#ccc] hover:bg-[#1a1a1a]'}
      `}
    >
      <span className="mr-1">{isSelected ? '▼' : '▶'}</span>
      {project.name}
    </div>
  );
}

function WorkspaceItem({ workspace, isActive, onSelect }: {
  workspace: Workspace;
  isActive: boolean;
  onSelect: () => void;
}) {
  const statusColor = {
    running: 'text-[#22c55e]',
    idle: 'text-[#888]',
    error: 'text-[#ef4444]',
    archived: 'text-[#666]',
  }[workspace.status];

  const statusDot = {
    running: '●',
    idle: '○',
    error: '✗',
    archived: '⊗',
  }[workspace.status];

  const envBadge = workspace.env === 'wsl' ? 'WSL' : 'WIN';

  return (
    <div
      onClick={onSelect}
      className={`
        px-2 py-1.5 rounded cursor-pointer text-sm mb-0.5 flex items-center gap-2
        ${isActive ? 'bg-[#222] border border-[#3b82f6]' : 'hover:bg-[#1a1a1a] border border-transparent'}
      `}
    >
      <span className={`${statusColor}`}>{statusDot}</span>
      <span className="flex-1 truncate">{workspace.name}</span>
      <span className="text-[10px] text-[#666] font-mono">{envBadge}</span>
    </div>
  );
}
