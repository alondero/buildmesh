import { useState, useEffect } from 'react';
import { useProjectStore } from '../../stores/projectStore';
import { useSessionStore } from '../../stores/sessionStore';
import type { Project } from '../../stores/projectStore';
import type { Session } from '../../stores/sessionStore';
import { getStatusConfig } from '../../lib/status';
import {
  DndContext,
  type DragEndEvent,
} from '@dnd-kit/core';
import {
  SortableContext,
  useSortable,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';

const ALL_PROVIDERS = [
  { id: 'anthropic', label: 'Anthropic', color: 'bg-blue-500' },
  { id: 'minimax', label: 'Minimax', color: 'bg-indigo-500' },
  { id: 'gemini', label: 'Gemini', color: 'bg-emerald-500' },
  { id: 'opencode', label: 'OpenCode', color: 'bg-amber-500' },
];

const isMac = navigator.platform.toUpperCase().includes('MAC');
const PROVIDERS = isMac
  ? ALL_PROVIDERS.filter(p => p.id === 'anthropic')
  : ALL_PROVIDERS;

export function Sidebar() {
  const projects = useProjectStore(state => state.projects);
  const addProject = useProjectStore(state => state.addProject);
  const selectedProjectId = useProjectStore(state => state.selectedProjectId);
  const selectProject = useProjectStore(state => state.selectProject);
  const reorderProjects = useProjectStore(state => state.reorderProjects);
  const sessions = useSessionStore(state => state.sessions);
  const activeSessionId = useSessionStore(state => state.activeSessionId);
  const setActiveSession = useSessionStore(state => state.setActiveSession);
  const createSession = useSessionStore(state => state.createSession);
  const deleteSession = useSessionStore(state => state.deleteSession);

  const [openDropdownFor, setOpenDropdownFor] = useState<number | null>(null);

  const handleSelectProject = (projectId: number) => {
    if (selectedProjectId === projectId) {
      selectProject(null);
    } else {
      selectProject(projectId);
    }
  };

  // Close dropdown when clicking outside
  useEffect(() => {
    function handleClickOutside(e: MouseEvent) {
      const target = e.target as HTMLElement;
      const clickedInsideDropdown = target.closest('[data-dropdown-for]');
      if (openDropdownFor !== null && !clickedInsideDropdown) {
        setOpenDropdownFor(null);
      }
    }
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, [openDropdownFor]);

  const handleAddProject = async () => {
    await addProject();
  };

  const handleNewSession = async (project: Project) => {
    setOpenDropdownFor(openDropdownFor === project.id ? null : project.id);
  };

  const handleSelectProvider = async (project: Project, providerId: string) => {
    setOpenDropdownFor(null);
    try {
      const session = await createSession(project.id, project.name, project.path, 'main', providerId);
      await setActiveSession(session.id);
      selectProject(project.id);
    } catch (e) {
      console.error('Failed to create session:', e);
    }
  };

  const handleDeleteSession = async (e: React.MouseEvent, sessionId: number) => {
    e.stopPropagation();
    await deleteSession(sessionId);
  };

  const handleDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const activeIndex = projects.findIndex(p => p.id === active.id);
    const overIndex = projects.findIndex(p => p.id === over.id);
    if (activeIndex === -1 || overIndex === -1) return;
    reorderProjects(active.id as number, overIndex);
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
            <DndContext onDragEnd={handleDragEnd}>
              <SortableContext items={projects.map(p => p.id)} strategy={verticalListSortingStrategy}>
                {projects.map(project => {
                  const projectSessions = sessions.filter(w => w.project_id === project.id);
                  const isDropdownOpen = openDropdownFor === project.id;
                  return (
                    <SortableProject
                      key={project.id}
                      project={project}
                      isSelected={selectedProjectId === project.id}
                      isDropdownOpen={isDropdownOpen}
                      onSelectProject={handleSelectProject}
                      onNewSession={handleNewSession}
                      onSelectProvider={handleSelectProvider}
                      projectSessions={projectSessions}
                      activeSessionId={activeSessionId}
                      setActiveSession={setActiveSession}
                      selectProject={selectProject}
                      onDeleteSession={handleDeleteSession}
                    />
                  );
                })}
              </SortableContext>
            </DndContext>
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

function SessionItem({ session, isActive, onSelect, onDelete }: {
  session: Session;
  isActive: boolean;
  onSelect: () => void;
  onDelete: (e: React.MouseEvent) => void;
}) {
  const config = getStatusConfig(session.status);
  const isAwaiting = session.status === 'awaiting_input';
  const envBadge = session.env === 'wsl' ? 'WSL' : isMac ? 'MAC' : 'WIN';

  return (
    <div
      data-session-item
      data-session-id={session.id}
      onClick={onSelect}
      className={`
        pl-8 pr-1 py-1 rounded cursor-pointer text-sm mb-0.5 flex items-center gap-2
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
      <button
        onClick={onDelete}
        className="text-[#555] hover:text-[#ef4444] text-xs px-1 transition-colors"
        title="Delete session"
      >
        ×
      </button>
    </div>
  );
}

interface SortableProjectProps {
  project: Project;
  isSelected: boolean;
  isDropdownOpen: boolean;
  onSelectProject: (id: number) => void;
  onNewSession: (project: Project) => void;
  onSelectProvider: (project: Project, providerId: string) => void;
  projectSessions: Session[];
  activeSessionId: number | null;
  setActiveSession: (id: number) => void;
  selectProject: (id: number | null) => void;
  onDeleteSession: (e: React.MouseEvent, sessionId: number) => void;
}

function SortableProject({
  project,
  isSelected,
  isDropdownOpen,
  onSelectProject,
  onNewSession,
  onSelectProvider,
  projectSessions,
  activeSessionId,
  setActiveSession,
  selectProject,
  onDeleteSession,
}: SortableProjectProps) {
  const {
    attributes,
    listeners,
    setNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({ id: project.id });

  const style = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.5 : 1,
  };

  return (
    <div ref={setNodeRef} style={style} className="mb-2">
      <div className="flex items-center gap-1">
        {/* Drag handle */}
        <button
          {...attributes}
          {...listeners}
          className="text-[#444] hover:text-[#888] cursor-grab active:cursor-grabbing text-xs px-1"
          title="Drag to reorder"
        >
          ⋮⋮
        </button>

        <div className="relative">
          <button
            onClick={() => onNewSession(project)}
            className={`text-xs px-1 ${isDropdownOpen ? 'text-[#60a5fa]' : 'text-[#3b82f6] hover:text-[#60a5fa]'}`}
            title="New session"
          >
            +
          </button>
          {isDropdownOpen && (
            <div data-dropdown-for={project.id} className="absolute left-0 top-full mt-1 z-50 bg-[#1a1a1a] border border-[#3a3a3a] rounded shadow-lg py-1 min-w-[120px]">
              {PROVIDERS.map(p => (
                <button
                  key={p.id}
                  onClick={() => onSelectProvider(project, p.id)}
                  className="w-full text-left px-3 py-1.5 text-xs text-[#ccc] hover:bg-[#252525] flex items-center gap-2"
                >
                  <span className={`w-2 h-2 rounded-full ${p.color}`} />
                  {p.label}
                </button>
              ))}
            </div>
          )}
        </div>
        <div
          onClick={() => onSelectProject(project.id)}
          className={`flex-1 px-2 py-1.5 rounded cursor-pointer text-sm hover:bg-[#1a1a1a] ${
            isSelected ? 'text-[#3b82f6] font-semibold' : 'text-[#ccc]'
          }`}
        >
          {project.name}
        </div>
      </div>
      {projectSessions.map(session => (
        <SessionItem
          key={session.id}
          session={session}
          isActive={activeSessionId === session.id}
          onSelect={() => {
            setActiveSession(session.id);
            selectProject(session.project_id);
          }}
          onDelete={(e) => onDeleteSession(e, session.id)}
        />
      ))}
    </div>
  );
}
