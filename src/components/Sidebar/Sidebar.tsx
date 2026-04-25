import { useProjectStore } from '../../stores/projectStore';
import { useSessionStore } from '../../stores/sessionStore';
import type { Project } from '../../stores/projectStore';
import type { Session } from '../../stores/sessionStore';
import { STATUS_CONFIG } from '../../lib/status';

export function Sidebar() {
  const projects = useProjectStore(state => state.projects);
  const addProject = useProjectStore(state => state.addProject);
  const sessions = useSessionStore(state => state.sessions);
  const activeSessionId = useSessionStore(state => state.activeSessionId);
  const setActiveSession = useSessionStore(state => state.setActiveSession);
  const createSession = useSessionStore(state => state.createSession);

  const handleAddProject = async () => {
    await addProject();
  };

  const handleNewSession = async (project: Project) => {
    const branch = prompt('Branch name:', 'main');
    if (!branch) return;
    await createSession(project.id, project.name, project.path, branch);
  };

  return (
    <div className="w-64 bg-[#111] border-r border-[#2a2a2a] flex flex-col h-full">
      {/* Header */}
      <div className="p-3 border-b border-[#2a2a2a]">
        <h1 className="text-sm font-semibold text-[#e0e0e0]">Buildmesh</h1>
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
              const projectSessions = sessions.filter(w => w.project_id === project.id);
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
                  {projectSessions.map(session => (
                    <SessionItem
                      key={session.id}
                      session={session}
                      isActive={activeSessionId === session.id}
                      onSelect={() => setActiveSession(session.id)}
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
        <span>{sessions.filter(w => w.status === 'running').length} active</span>
      </div>
    </div>
  );
}

function SessionItem({ session, isActive, onSelect }: {
  session: Session;
  isActive: boolean;
  onSelect: () => void;
}) {
  const config = STATUS_CONFIG[session.status];
  const isAwaiting = session.status === 'awaiting_input';
  const envBadge = session.env === 'wsl' ? 'WSL' : 'WIN';

  return (
    <div
      onClick={onSelect}
      className={`
        pl-8 pr-2 py-1 rounded cursor-pointer text-sm mb-0.5 flex items-center gap-2
        ${isActive ? 'bg-[#222] border border-[#3b82f6]' : 'hover:bg-[#1a1a1a] border border-transparent'}
        ${isAwaiting ? 'bg-[#2a2010]' : ''}
      `}
    >
      <span className={config.color}>{config.dot}</span>
      <span className="flex-1 truncate text-[#aaa]">{session.name}</span>
      {isAwaiting && (
        <span className="text-[10px] text-[#f59e0b] font-semibold animate-pulse">ATTN</span>
      )}
      <span className="text-[10px] text-[#666] font-mono">{envBadge}</span>
    </div>
  );
}
