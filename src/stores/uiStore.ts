import { create } from 'zustand';

interface UIState {
  changedFilesOpen: boolean;
  changedFilesNodeId: number | null;
  toggleChangedFiles: (nodeId: number) => void;
  closeChangedFiles: () => void;
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

  closeChangedFiles: () => {
    set({ changedFilesOpen: false, changedFilesNodeId: null });
  },
}));