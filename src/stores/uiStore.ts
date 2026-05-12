import { create } from 'zustand';

interface UIState {
  changedFilesOpen: boolean;
  changedFilesNodeId: number | null;
  changedFilesWidth: number;
  toggleChangedFiles: (nodeId: number) => void;
  setChangedFilesNodeId: (nodeId: number) => void;
  closeChangedFiles: () => void;
  setChangedFilesWidth: (width: number) => void;

  propertiesPanelMeshId: number | null;
  openPropertiesPanel: (meshId: number) => void;
  closePropertiesPanel: () => void;
}

export const useUIStore = create<UIState>((set, get) => ({
  changedFilesOpen: false,
  changedFilesNodeId: null,

  toggleChangedFiles: (nodeId: number) => {
    const { changedFilesOpen, changedFilesNodeId } = get();
    if (changedFilesOpen && changedFilesNodeId === nodeId) {
      set({ changedFilesOpen: false, changedFilesNodeId: null });
    } else {
      set({ changedFilesOpen: true, changedFilesNodeId: nodeId });
    }
  },

  setChangedFilesNodeId: (nodeId: number) => {
    if (get().changedFilesOpen) {
      set({ changedFilesNodeId: nodeId });
    }
  },

  closeChangedFiles: () => {
    set({ changedFilesOpen: false, changedFilesNodeId: null });
  },

  changedFilesWidth: 280,

  setChangedFilesWidth: (width: number) => {
    set({ changedFilesWidth: width });
  },

  propertiesPanelMeshId: null,

  openPropertiesPanel: (meshId: number) => {
    set({ propertiesPanelMeshId: meshId });
  },

  closePropertiesPanel: () => {
    set({ propertiesPanelMeshId: null });
  },
}));