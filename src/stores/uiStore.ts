import { create } from 'zustand';

// Tabs the Probe Panel can show. Kept as a string-literal union (not a
// generated wire enum) because it's a pure UI concern — no backend serialises
// it. `usage` was added in issue #601 as the dedicated glanceable surface
// for Usage Meters (subscription quota + cash balance), reached from a
// meter icon in the sidebar header.
export type ProbeTab = 'files' | 'review' | 'usage' | 'properties' | 'issues' | 'pulls' | 'sessions' | 'worktrees' | 'scratchpad';

// Which baseline a single-file diff is taken against:
//   'head' — uncommitted working-tree changes vs HEAD (Project Files tab,
//            `diff_file_against_head`).
//   'base' — every change since the agent branched, vs the merge-base
//            (Agent Changes tab, ADR 0005, `diff_node_file_against_base`).
//   'pr'   — a file in a GitHub pull request, fetched from the GitHub
//            `/pulls/{n}/files` API (issue #421). `prNumber` must be set;
//            `filePath === ''` switches the overlay to a list view of all
//            changed files in the PR (click a file to drill in).
export type DiffSource = 'head' | 'base' | 'pr';

// Everything the Center Workspace Diff Overlay (issue #379) needs to fetch,
// label, and auto-close a single-file diff. Captured when the user clicks a
// changed file in the Probe; consumed by `CenterDiffOverlay`.
export interface DiffContext {
  /** Path of the file being diffed, relative to `rootPath`. Empty string
   *  means "list view" — currently only meaningful for `source: 'pr'`,
   *  where it switches the overlay between the PR's file list and a
   *  single-file diff. */
  filePath: string;
  /** Repo/worktree root the diff resolves against — also the path watched for
   *  live refresh while the overlay is open. For PR diffs, the mesh root
   *  is good enough: there's no live file-watcher hook for a remote PR. */
  rootPath: string;
  /** Owning agent node — also the focused-lens node captured at open time. Used
   *  for the toolbar's "parent node" label and the auto-close comparison
   *  (criterion: close when the user focuses a different node). Null for a
   *  mesh-scoped diff opened from Project Files with no node focused, and
   *  always null for a PR diff (the PR's source branch may not even exist
   *  locally — the auto-close is mesh-scoped only). */
  nodeId: number | null;
  /** Mesh the diff belongs to (the lens mesh captured at open time). Drives the
   *  auto-close when the user selects a different project in the sidebar. */
  meshId: number;
  /** Baseline to diff against — see `DiffSource`. */
  source: DiffSource;
  /** PR number when `source === 'pr'`. The overlay reads the file list /
   *  patch from `GET /repos/{owner}/{repo}/pulls/{n}/files` keyed off this.
   *  Undefined for `'head'` / `'base'` sources. */
  prNumber?: number;
}

interface UIState {
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
  // The "click active tab to collapse" UX is left to ProbePanel's own
  // click handler — this is a pure "make the tab visible" action.
  openProbeTab: (tab: ProbeTab) => void;
  openDiff: (ctx: DiffContext) => void;
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
