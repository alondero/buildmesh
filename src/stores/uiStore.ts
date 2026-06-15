import { create } from 'zustand';

// Tabs the Probe Panel can show. Kept as a string-literal union (not a
// generated wire enum) because it's a pure UI concern — no backend serialises
// it.
export type ProbeTab = 'files' | 'review' | 'properties' | 'issues' | 'sessions' | 'worktrees';

interface UIState {
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
  // The "click active tab to collapse" UX is left to ProbePanel's own
  // click handler — this is a pure "make the tab visible" action.
  openProbeTab: (tab: ProbeTab) => void;
  openDiff: (file: string) => void;
  closeDiff: () => void;

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

  openDiff: (file: string) => {
    // Open the diff for `file`: also flip the probe to the review tab and
    // make sure the panel is visible, so a file picked from the file tree
    // surfaces its diff without the user hunting for the tab.
    set({ activeDiffFile: file, probeTab: 'review', probeOpen: true });
  },

  closeDiff: () => {
    set({ activeDiffFile: null });
  },

  // Idempotent "make this tab visible" — atomic `setProbeTab(tab) +
  // probeOpen = true`. Call sites stay one-liners; the activity-bar owns
  // the "click active tab to collapse" UX. All 6 probe tabs open via this
  // action.
  //
  // Routes through `setProbeTab` so the activeDiffFile cleanup (clear
  // when leaving `review`) is inherited — a stale diff from Review
  // would otherwise linger behind a freshly-opened Properties tab.
  openProbeTab: (tab) => {
    get().setProbeTab(tab);
    set({ probeOpen: true });
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
