import { create } from 'zustand';

export type FileExplorerContext =
  | { type: 'agent'; nodeId: number; path: string }
  | { type: 'mesh'; meshId: number; path: string }
  | { type: 'userConfig'; path: string };

// Tabs the Probe Panel can show. Kept as a string-literal union (not a
// generated wire enum) because it's a pure UI concern — no backend serialises
// it.
export type ProbeTab = 'files' | 'review' | 'properties' | 'issues' | 'sessions' | 'worktrees';

// Which baseline a single-file diff is taken against:
//   'head' — uncommitted working-tree changes vs HEAD (Project Files tab,
//            `diff_file_against_head`).
//   'base' — every change since the agent branched, vs the merge-base
//            (Agent Changes tab, ADR 0005, `diff_node_file_against_base`).
export type DiffSource = 'head' | 'base';

// Everything the Center Workspace Diff Overlay (issue #379) needs to fetch,
// label, and auto-close a single-file diff. Captured when the user clicks a
// changed file in the Probe; consumed by `CenterDiffOverlay`.
export interface DiffContext {
  /** Path of the file being diffed, relative to `rootPath`. */
  filePath: string;
  /** Repo/worktree root the diff resolves against — also the path watched for
   *  live refresh while the overlay is open. */
  rootPath: string;
  /** Owning agent node — also the focused-lens node captured at open time. Used
   *  for the toolbar's "parent node" label and the auto-close comparison
   *  (criterion: close when the user focuses a different node). Null for a
   *  mesh-scoped diff opened from Project Files with no node focused. */
  nodeId: number | null;
  /** Mesh the diff belongs to (the lens mesh captured at open time). Drives the
   *  auto-close when the user selects a different project in the sidebar. */
  meshId: number;
  /** Baseline to diff against — see `DiffSource`. */
  source: DiffSource;
}

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
  // The single file currently shown in the Center Workspace Diff Overlay
  // (issue #379), or null when the overlay is closed. Independent of
  // `probeTab` — the overlay floats over the terminal grid and survives Probe
  // tab switches, so the user can keep the Probe open on any tab while
  // reviewing. Cleared by `closeDiff`, by Esc / "Back to Terminals", and by
  // the overlay's auto-close when the focused node or selected mesh changes.
  activeDiffFile: DiffContext | null;
  toggleProbe: () => void;
  setProbeTab: (tab: ProbeTab) => void;
  // Open the probe on a specific tab, opening the panel if it's collapsed.
  // Used by issue #376's call sites (sidebar "File Explorer" menu, agent
  // node git-summary chip) to land the user on the right tab in one call.
  // The "click active tab to collapse" UX is left to ProbePanel's own
  // click handler — this is a pure "make the tab visible" action.
  openProbeTab: (tab: ProbeTab) => void;
  openDiff: (ctx: DiffContext) => void;
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
    // Pure tab switch. The Center Workspace Diff Overlay (issue #379) is
    // independent of the active tab — it floats over the terminal grid — so
    // switching tabs no longer clears `activeDiffFile`. The overlay closes
    // only via `closeDiff` (Esc / "Back to Terminals") or its own auto-close
    // when the focused node / selected mesh changes.
    set({ probeTab: tab });
  },

  openDiff: (ctx: DiffContext) => {
    // Open the Center Workspace Diff Overlay on `ctx.filePath`. The Probe
    // stays on whatever tab it was on (so the user can keep clicking files in
    // Project Files / Agent Changes to switch the diff), but we make sure the
    // panel is visible — the overlay and the interactive file list are meant
    // to be used together (issue #379).
    set({ activeDiffFile: ctx, probeOpen: true });
  },

  closeDiff: () => {
    set({ activeDiffFile: null });
  },

  // Idempotent "make this tab visible" — atomic `setProbeTab(tab) +
  // probeOpen = true`. Call sites stay one-liners; the activity-bar owns
  // the "click active tab to collapse" UX. The probe tabs (#376, #377,
  // #378) all open via this action. Does not touch `activeDiffFile`: the
  // diff overlay (#379) is independent of the active tab.
  openProbeTab: (tab) => {
    get().setProbeTab(tab);
    set({ probeOpen: true });
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