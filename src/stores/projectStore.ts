import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';

export interface Project {
  id: number;
  name: string;
  path: string;
  layout: 'grid' | 'single';
  position: number;
  created_at: string;
}

interface ProjectState {
  projects: Project[];
  projectsById: Map<number, Project>;
  selectedProjectId: number | null;
  loading: boolean;
  error: string | null;
  fetchProjects: () => Promise<void>;
  addProject: () => Promise<void>;
  addTestProject: (name: string) => Promise<Project | null>;
  createProject: (name: string, path: string) => Promise<void>;
  deleteProject: (id: number) => Promise<void>;
  selectProject: (id: number | null) => void;
  updateProjectLayout: (id: number, layout: 'grid' | 'single') => Promise<void>;
  reorderProjects: (projectId: number, newPosition: number) => Promise<void>;
}

export const useProjectStore = create<ProjectState>((set) => ({
  projects: [],
  projectsById: new Map(),
  selectedProjectId: null,
  loading: false,
  error: null,

  fetchProjects: async () => {
    set({ loading: true, error: null });
    try {
      const projects = await invoke<Project[]>('list_projects');
      const projectsById = new Map(projects.map((p) => [p.id, p]));
      set({ projects, projectsById, loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  addProject: async () => {
    try {
      const project = await invoke<Project>('add_project');
      set((state) => ({
        projects: [...state.projects, project],
        projectsById: new Map([...state.projectsById, [project.id, project]])
      }));
    } catch (e) {
      set({ error: String(e) });
    }
  },

  addTestProject: async (name) => {
    try {
      const project = await invoke<Project>('create_test_project', { name });
      set((state) => ({
        projects: [...state.projects, project],
        projectsById: new Map([...state.projectsById, [project.id, project]])
      }));
      return project;
    } catch (e) {
      set({ error: String(e) });
      return null;
    }
  },

  createProject: async (name, path) => {
    try {
      await invoke('create_project', { name, path });
      await useProjectStore.getState().fetchProjects();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  deleteProject: async (id) => {
    try {
      await invoke('delete_project', { projectId: id });
      await useProjectStore.getState().fetchProjects();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  selectProject: (id) => set({ selectedProjectId: id }),

  updateProjectLayout: async (id, layout) => {
    try {
      await invoke('update_project_layout', { projectId: id, layout });
      set((state) => {
        const existing = state.projectsById.get(id);
        if (!existing) return state;
        const updated = { ...existing, layout };
        return {
          projects: state.projects.map((p) => (p.id === id ? updated : p)),
          projectsById: new Map([...state.projectsById, [id, updated]])
        };
      });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  reorderProjects: async (projectId, newPosition) => {
    let updatedProjects: Project[];
    set((state) => {
      const projects = [...state.projects];
      const draggedIdx = projects.findIndex((p) => p.id === projectId);
      if (draggedIdx === -1) return state;
      const [dragged] = projects.splice(draggedIdx, 1);
      projects.splice(newPosition, 0, dragged);
      updatedProjects = projects.map((p, idx) => ({ ...p, position: idx }));
      const projectsById = new Map(updatedProjects.map((p) => [p.id, p]));
      return { projects: updatedProjects, projectsById };
    });
    try {
      // Send ALL projects' positions so DB stays in sync with optimistic update
      const currentProjects = updatedProjects!;
      const updates = currentProjects.map((p) => [p.id, p.position] as [number, number]);
      await invoke('update_project_positions', { updates });
    } catch (e) {
      set({ error: String(e) });
      await useProjectStore.getState().fetchProjects();
    }
  },
}));
