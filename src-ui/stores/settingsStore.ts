import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

interface SettingsState {
  defaultProjectsRoot: string;
  loading: boolean;
  setProjectsRoot: (path: string) => Promise<void>;
  loadSettings: () => Promise<void>;
}

export const useSettingsStore = create<SettingsState>((set) => ({
  defaultProjectsRoot: '',
  loading: false,

  setProjectsRoot: async (path) => {
    try {
      await invoke('set_setting', { key: 'default_projects_root', value: path });
      set({ defaultProjectsRoot: path });
    } catch (e) {
      console.error('Failed to save projects root:', e);
    }
  },

  loadSettings: async () => {
    set({ loading: true });
    try {
      const root = await invoke<string | null>('get_setting', { key: 'default_projects_root' });
      set({ defaultProjectsRoot: root || '', loading: false });
    } catch (e) {
      set({ loading: false });
    }
  },
}));
