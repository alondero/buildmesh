import { create } from 'zustand';

export type FileExplorerContext =
  | { type: 'agent'; nodeId: number; path: string }
  | { type: 'mesh'; meshId: number; path: string }
  | { type: 'userConfig'; path: string };

interface UIState {
  changedFilesOpen: boolean;
  changedFilesNodeId: number | null;
  changedFilesWidth: number;
  toggleChangedFiles: (nodeId: number) => void;
  setChangedFilesNodeId: (nodeId: number) => void;
  closeChangedFiles: () => void;
  setChangedFilesWidth: (width: number) => void;

  fileExplorerContext: FileExplorerContext | null;
  toggleFileExplorer: (context: FileExplorerContext) => void;
  closeFileExplorer: () => void;

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

  fileExplorerContext: null,

  toggleFileExplorer: (context: FileExplorerContext) => {
    const { fileExplorerContext } = get();
    if (
      fileExplorerContext &&
      fileExplorerContext.type === context.type &&
      'nodeId' in context &&
      'nodeId' in fileExplorerContext &&
      fileExplorerContext.nodeId === context.nodeId
    ) {
      set({ fileExplorerContext: null });
    } else {
      set({ fileExplorerContext: context });
    }
  },

  closeFileExplorer: () => {
    set({ fileExplorerContext: null });
  },

  propertiesPanelMeshId: null,

  openPropertiesPanel: (meshId: number) => {
    set({ propertiesPanelMeshId: meshId });
  },

  closePropertiesPanel: () => {
    set({ propertiesPanelMeshId: null });
  },
}));