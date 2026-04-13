import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';

export interface Project {
  id: number;
  name: string;
  path: string;
  created_at: string;
}

interface ProjectState {
  projects: Project[];
  selectedProjectId: number | null;
  loading: boolean;
  error: string | null;
  fetchProjects: () => Promise<void>;
  createProject: (name: string, path: string) => Promise<void>;
  deleteProject: (id: number) => Promise<void>;
  selectProject: (id: number | null) => void;
}

export const useProjectStore = create<ProjectState>((set) => ({
  projects: [],
  selectedProjectId: null,
  loading: false,
  error: null,

  fetchProjects: async () => {
    set({ loading: true, error: null });
    try {
      const projects = await invoke<Project[]>('list_projects');
      set({ projects, loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
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
}));
