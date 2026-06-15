import { create } from 'zustand';

export type FileExplorerContext =
  | { type: 'agent'; nodeId: number; path: string }
  | { type: 'mesh'; meshId: number; path: string }
  | { type: 'userConfig'; path: string };

// Tabs the Probe Panel can show. Kept as a string-literal union (not a
// generated wire enum) because it's a pure UI concern — no backend serialises
// it.
export type ProbeTab = 'files' | 'review' | 'properties' | 'issues' | 'sessions' | 'worktrees';

interface UIState {
  changedFilesOpen: boolean;
  changedFilesNodeId: number | null;
  changedFilesWidth: number;
  toggleChangedFiles: (nodeId: number) => void;
  setChangedFilesNodeId: (nodeId: number) => void;
  closeChangedFiles: () => void;
  setChangedFilesWidth: (width: number) => void;

  // ---- Probe Panel (issue #373) ----
  // The Probe Panel is a unified right-hand surface for a focused context
  // (mesh + optional agent node): files, review, properties, issues, sessions,
  // and worktrees all live behind the same dock and switch by tab. Visibility
  // and active tab are stored here so every consumer (sidebar, global view
  // card, keyboard shortcut) reads/writes the same source of truth.
  probeOpen: boolean;
  probeTab: ProbeTab;
  // Path of the file currently being diffed in the probe's Review tab, or
  // null when no file is open. Cleared by `closeDiff` and on tab change away
  // from `review`.
  activeDiffFile: string | null;
  toggleProbe: () => void;
  setProbeTab: (tab: ProbeTab) => void;
  // Open the probe on a specific tab, opening the panel if it's collapsed.
  // Used by issue #376's call sites (sidebar "File Explorer" menu, agent
  // node git-summary chip) to land the user on the right tab in one call.
  // The "click active tab to collapse" UX is left to ProbePanel's own
  // click handler — this is a pure "make the tab visible" action.
  openProbeTab: (tab: ProbeTab) => void;
  openDiff: (file: string) => void;
  closeDiff: () => void;

  // ---- Legacy (preserved for migration) ----
  // `fileExplorerContext` and `propertiesPanelMeshId` predate the unified
  // Probe Panel and are still read by the existing File Explorer / Mesh
  // Properties components. They stay in the store verbatim so the in-flight
  // migration can swap the consumers over one component at a time without
  // breaking the build.
  fileExplorerContext: FileExplorerContext | null;
  toggleFileExplorer: (context: FileExplorerContext) => void;
  closeFileExplorer: () => void;

  propertiesPanelMeshId: number | null;
  openPropertiesPanel: (meshId: number) => void;
  closePropertiesPanel: () => void;

  // Agent node currently under an OS file-drag, or null. Drives the terminal
  // "drop file to paste path" overlay; set by the window-level drop listener.
  dragTargetNodeId: number | null;
  setDragTargetNodeId: (nodeId: number | null) => void;

  // Node maximized to fill the whole grid area (#65), or null for the normal
  // grid. Double-clicking a node header toggles this; Escape clears it.
  maximizedNodeId: number | null;
  toggleMaximizedNode: (nodeId: number) => void;
  clearMaximizedNode: () => void;
}

export const useUIStore = create<UIState>((set, get) => ({
  changedFilesOpen: false,
  changedFilesNodeId: null,

  probeOpen: false,
  probeTab: 'files',
  activeDiffFile: null,

  toggleProbe: () => {
    set({ probeOpen: !get().probeOpen });
  },

  setProbeTab: (tab: ProbeTab) => {
    // Switching away from `review` implicitly closes any open diff so the
    // next visit to the tab starts blank — the previous file's diff would
    // otherwise linger as stale state behind a non-review tab.
    set((state) => ({
      probeTab: tab,
      activeDiffFile: tab === 'review' ? state.activeDiffFile : null,
    }));
  },

  openProbeTab: (tab: ProbeTab) => {
    // One-call "make this tab visible" helper. Does NOT collapse the panel
    // when called on the active tab — closing stays a separate concern
    // (the activity-bar's click handler does that).
    set({ probeTab: tab, probeOpen: true });
  },

  openDiff: (file: string) => {
    // Open the diff for `file`: also flip the probe to the review tab and
    // make sure the panel is visible, so a file picked from the file tree
    // surfaces its diff without the user hunting for the tab.
    set({ activeDiffFile: file, probeTab: 'review', probeOpen: true });
  },

  closeDiff: () => {
    set({ activeDiffFile: null });
  },

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
    if (!fileExplorerContext || fileExplorerContext.type !== context.type) {
      set({ fileExplorerContext: context });
      return;
    }
    if (context.type === 'agent') {
      const existing = fileExplorerContext as { type: 'agent'; nodeId: number; path: string };
      if (existing.nodeId === context.nodeId) {
        set({ fileExplorerContext: null });
      } else {
        set({ fileExplorerContext: context });
      }
    } else if (context.type === 'mesh') {
      const existing = fileExplorerContext as { type: 'mesh'; meshId: number; path: string };
      if (existing.meshId === context.meshId) {
        set({ fileExplorerContext: null });
      } else {
        set({ fileExplorerContext: context });
      }
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

  dragTargetNodeId: null,

  setDragTargetNodeId: (nodeId: number | null) => {
    if (get().dragTargetNodeId !== nodeId) {
      set({ dragTargetNodeId: nodeId });
    }
  },

  maximizedNodeId: null,

  toggleMaximizedNode: (nodeId: number) => {
    set({ maximizedNodeId: get().maximizedNodeId === nodeId ? null : nodeId });
  },

  clearMaximizedNode: () => {
    if (get().maximizedNodeId !== null) {
      set({ maximizedNodeId: null });
    }
  },
}));