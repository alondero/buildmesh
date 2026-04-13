import { useProjectStore } from '../../stores/projectStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import type { Project } from '../../stores/projectStore';
import type { Workspace } from '../../stores/workspaceStore';

export function Sidebar() {
  const { projects, addProject } = useProjectStore();
  const { workspaces, activeWorkspaceId, setActiveWorkspace, createWorkspace } = useWorkspaceStore();

  const handleAddProject = async () => {
    await addProject();
  };

  const handleNewSession = async (project: Project) => {
    const branch = prompt('Branch name:', 'main');
    if (!branch) return;
    await createWorkspace(project.id, project.name, project.path, branch);
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
            <span className="text-xs font-medium text-[#888] uppercase">Sessions</span>
            <button
              onClick={handleAddProject}
              className="text-xs text-[#3b82f6] hover:text-[#60a5fa]"
            >
              + Add
            </button>
          </div>

          {projects.length === 0 ? (
            <p className="text-xs text-[#666] px-2 py-4 text-center">
              No sessions yet.{'\n'}Click + Add to get started.
            </p>
          ) : (
            projects.map(project => {
              const projectWorkspaces = workspaces.filter(w => w.project_id === project.id);
              return (
                <div key={project.id} className="mb-2">
                  <div className="flex items-center gap-1">
                    <button
                      onClick={() => handleNewSession(project)}
                      className="text-xs text-[#3b82f6] hover:text-[#60a5fa] px-1"
                      title="New session"
                    >
                      +
                    </button>
                    <div className="flex-1 px-2 py-1.5 rounded cursor-pointer text-sm text-[#ccc] hover:bg-[#1a1a1a]">
                      {project.name}
                    </div>
                  </div>
                  {projectWorkspaces.map(ws => (
                    <WorkspaceItem
                      key={ws.id}
                      workspace={ws}
                      isActive={activeWorkspaceId === ws.id}
                      onSelect={() => setActiveWorkspace(ws.id)}
                    />
                  ))}
                </div>
              );
            })
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
        pl-8 pr-2 py-1 rounded cursor-pointer text-sm mb-0.5 flex items-center gap-2
        ${isActive ? 'bg-[#222] border border-[#3b82f6]' : 'hover:bg-[#1a1a1a] border border-transparent'}
      `}
    >
      <span className={statusColor}>{statusDot}</span>
      <span className="flex-1 truncate text-[#aaa]">{workspace.name}</span>
      <span className="text-[10px] text-[#666] font-mono">{envBadge}</span>
    </div>
  );
}
