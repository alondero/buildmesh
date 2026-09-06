import { create } from 'zustand';
import { useMeshStore } from './meshStore';
import { STATUS_CONFIG } from '../lib/status';
import type { SessionStatus } from '../types/generated/SessionStatus';
import type { ProbeContextPin } from '../lib/probeContext';
import { pushProbeWorkingSet } from '../lib/probeWorkingSet';

// The five canvas View Modes (wayfinder #982 — tickets #983 state model,
// #986 rendering). 'single' solos the active node (it subsumes the old
// `maximizedNodeId`), 'mesh' scopes to the sidebar-selected mesh, 'pinned'
// is a cross-mesh filter over `AgentNode.is_pinned`, 'all' shows every
// node, and 'filtered' (the Search Nodes view, #1609) shows the cross-mesh
// node set narrowed by the Grid Controls (search text + provider + status
// filters). Kept as a string-literal union (not a generated wire enum)
// because it's a pure UI concern — no backend serialises it.
export type ViewMode = 'single' | 'mesh' | 'pinned' | 'all' | 'filtered';

// The Universal Command Omnibar's preselected palette mode (issue #1409 —
// map #1371 Decision #2): 'files' is the default search (⌘/Ctrl+K),
// 'commands' is the command-runner (⌘/Ctrl+P) — the editors' "quick open
// file / run command" convention. The palette UI reads `omnibarMode` once
// at mount to seed its search box; the user can switch modes inside the
// palette without touching the store.
export type OmnibarMode = 'files' | 'commands';

// The grid modes 'single' can be entered from — `exitSingleMode()` returns
// here. 'single' itself is never a valid return target.
export type NonSingleViewMode = Exclude<ViewMode, 'single'>;

const VIEW_MODE_STORAGE_KEY = 'buildmesh.view-mode';
const VIEW_MODES: readonly ViewMode[] = ['single', 'mesh', 'pinned', 'all', 'filtered'];

// Boot value for `viewMode`: the persisted mode if it's present and valid,
// else derived from the current mesh selection exactly as the pre-view-modes
// canvas filter behaved (mesh selected → 'mesh', else 'all'). The
// localStorage read is lazy-init at store creation and wrapped in try/catch
// (unavailable in test envs / private mode), following the `src/lib/theme.ts`
// convention. Only the mode is persisted — the single-mode target is not
// (it falls back to the active node).
function loadViewMode(selectedMeshId: number | null): ViewMode {
  try {
    const stored = localStorage.getItem(VIEW_MODE_STORAGE_KEY);
    if (stored !== null && (VIEW_MODES as readonly string[]).includes(stored)) {
      return stored as ViewMode;
    }
  } catch {
    // Fall through to the selection-derived default.
  }
  return selectedMeshId === null ? 'all' : 'mesh';
}

// Write-on-change persistence, try/catch-wrapped like the read above — a
// storage failure must never block the mode switch itself.
function persistViewMode(mode: ViewMode): void {
  try {
    localStorage.setItem(VIEW_MODE_STORAGE_KEY, mode);
  } catch {
    // localStorage unavailable — in-memory state still flips.
  }
}

// ---------------------------------------------------------------------------
// Grid Controls (wayfinder #988 — this ticket #995 is the state model, #996
// the AgentNodeView filter/sort logic, #997 the View Header UI, #998 the
// keyboard shortcuts). Only the state and its persistence live here: nothing
// in this module reads the controls to decide what the grid renders.
// ---------------------------------------------------------------------------

// What the grid is ordered by. 'custom' is the user's manual drag order (the
// `AgentNode.position` column) and stays the default, so the pre-#988 grid is
// exactly what an un-persisted boot shows; #988 also disables drag-and-drop
// reordering while `sortBy !== 'custom'`. 'created' covers the map's "last
// active/created" axis. A 'last-active' sort is deliberately deferred rather
// than missing: the activity timestamp the app already keeps
// (`agent_nodes.status_changed_at`, schema v14, surfaced by the coordinator
// digest as `last_activity`) is not carried on the `AgentNode` wire struct, so
// the grid store has no client-side value to sort by. Exposing it is a backend
// change, out of scope for a state-model ticket. A string-literal union rather
// than a generated wire enum, like `ViewMode` above: sorting is a pure UI
// concern that no backend serialises.
export type GridSortBy = 'custom' | 'name' | 'status' | 'created';

export type GridSortDirection = 'asc' | 'desc';

/** The five grid filter/sort controls #995 adds to `uiStore` — the "control
 *  set" that is saved as one `PersistedGridControls` payload. */
export interface GridControls {
  /** Free-text node filter. Empty string means "no search". What it matches
   *  (name only vs. name + branch + task) is #996's decision. */
  gridSearchQuery: string;
  /** Harness/profile id to filter to, or null for "all providers". An opaque
   *  string, not a closed enum — `AgentNode.provider` is a user-extensible
   *  harness id (ADR-0014 / issue #535), so unknown ids must round-trip. */
  gridProviderFilter: string | null;
  /** Lifecycle status to filter to, or null for "all statuses". */
  gridStatusFilter: SessionStatus | null;
  gridSortBy: GridSortBy;
  gridSortDirection: GridSortDirection;
}

const GRID_CONTROLS_STORAGE_KEY = 'buildmesh.grid-controls';

// Neutral controls: no search, no filters, manual order ascending — i.e. the
// grid as it behaved before #988. Also what `resetGridControls` restores and
// what every unreadable/invalid persisted field falls back to.
const DEFAULT_GRID_CONTROLS: GridControls = {
  gridSearchQuery: '',
  gridProviderFilter: null,
  gridStatusFilter: null,
  gridSortBy: 'custom',
  gridSortDirection: 'asc',
};

const GRID_SORT_BY: readonly GridSortBy[] = ['custom', 'name', 'status', 'created'];
const GRID_SORT_DIRECTIONS: readonly GridSortDirection[] = ['asc', 'desc'];

// The status vocabulary a persisted filter is validated against. Reuses the
// display config rather than re-listing the nine statuses a third time —
// `STATUS_CONFIG` is already "one status vocabulary for every spawn/status
// surface". The `Record<SessionStatus, unknown>` annotation is the compile-time
// guard: adding a status to the Rust enum without a `STATUS_CONFIG` entry
// fails `tsc` here rather than silently making that status unfilterable.
const STATUS_VOCABULARY: Record<SessionStatus, unknown> = STATUS_CONFIG;

function isSessionStatus(value: unknown): value is SessionStatus {
  return typeof value === 'string'
    && Object.prototype.hasOwnProperty.call(STATUS_VOCABULARY, value);
}

// Stored payload shape. The keys drop the `grid` prefix the store fields carry
// — the storage key already scopes them, and `sortBy` / `sortDirection` are
// the names #988 uses. Anything else in the object is ignored on read.
interface PersistedGridControls {
  searchQuery: string;
  provider: string | null;
  status: SessionStatus | null;
  sortBy: GridSortBy;
  sortDirection: GridSortDirection;
}

// Boot value for the grid controls, read lazily at store creation and
// try/catch-wrapped like `loadViewMode` above (localStorage is unavailable in
// some test envs / private mode). Validation is per-field: a payload that has
// drifted or been hand-edited keeps whichever controls still parse instead of
// discarding the whole set, so a dropped sort option can't also wipe the
// user's filters.
function loadGridControls(): GridControls {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(GRID_CONTROLS_STORAGE_KEY);
  } catch {
    return { ...DEFAULT_GRID_CONTROLS };
  }
  if (raw === null) return { ...DEFAULT_GRID_CONTROLS };

  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return { ...DEFAULT_GRID_CONTROLS };
  }
  if (typeof parsed !== 'object' || parsed === null) return { ...DEFAULT_GRID_CONTROLS };

  const stored = parsed as Partial<PersistedGridControls>;
  return {
    gridSearchQuery:
      typeof stored.searchQuery === 'string'
        ? stored.searchQuery
        : DEFAULT_GRID_CONTROLS.gridSearchQuery,
    // Any string is a legal provider id (harness profiles are user-defined),
    // so only the type is checked — an id whose profile has since been deleted
    // simply matches nothing.
    gridProviderFilter:
      typeof stored.provider === 'string'
        ? stored.provider
        : DEFAULT_GRID_CONTROLS.gridProviderFilter,
    gridStatusFilter:
      isSessionStatus(stored.status)
        ? stored.status
        : DEFAULT_GRID_CONTROLS.gridStatusFilter,
    gridSortBy:
      (GRID_SORT_BY as readonly unknown[]).includes(stored.sortBy)
        ? stored.sortBy as GridSortBy
        : DEFAULT_GRID_CONTROLS.gridSortBy,
    gridSortDirection:
      (GRID_SORT_DIRECTIONS as readonly unknown[]).includes(stored.sortDirection)
        ? stored.sortDirection as GridSortDirection
        : DEFAULT_GRID_CONTROLS.gridSortDirection,
  };
}

// Write-on-change persistence of the whole control set under one key, as #988
// specifies. try/catch-wrapped like `persistViewMode` — a storage failure must
// never block the control change itself.
function persistGridControls(controls: GridControls): void {
  const payload: PersistedGridControls = {
    searchQuery: controls.gridSearchQuery,
    provider: controls.gridProviderFilter,
    status: controls.gridStatusFilter,
    sortBy: controls.gridSortBy,
    sortDirection: controls.gridSortDirection,
  };
  try {
    localStorage.setItem(GRID_CONTROLS_STORAGE_KEY, JSON.stringify(payload));
  } catch {
    // localStorage unavailable — in-memory state still changes.
  }
}

// Issue #1264 — `setGridSearchQuery` fires once per keystroke; the
// other four setters are clicked/selected, not typed. Persisting the
// search-query write synchronously per keystroke was wasteful (a
// localStorage write + JSON.stringify on every character). Debounce the
// search-query persistence to ~300 ms so a fast typist's burst lands as
// one write, mirroring `useSidebarResize.ts:55-62` (which already
// persists-on-settle for the same reason — sidebar drag fires the
// setter on every mousemove frame). The in-memory state still updates
// immediately, so the grid stays responsive; only the localStorage
// write is coalesced. The other four setters keep their synchronous
// persistence because their inputs are discrete clicks, not a stream.
const GRID_SEARCH_PERSIST_DEBOUNCE_MS = 300;
let pendingSearchPersistTimer: ReturnType<typeof setTimeout> | null = null;
function scheduleDebouncedSearchPersist(controls: GridControls): void {
  if (pendingSearchPersistTimer !== null) {
    clearTimeout(pendingSearchPersistTimer);
  }
  pendingSearchPersistTimer = setTimeout(() => {
    pendingSearchPersistTimer = null;
    persistGridControls(controls);
  }, GRID_SEARCH_PERSIST_DEBOUNCE_MS);
}
/** Test-only: flush the pending debounced search-query persist
 * synchronously and cancel any armed timer. Mirrors
 * `resetPathInvalidatedCacheForTests`. */
export function flushGridSearchPersistForTests(): void {
  if (pendingSearchPersistTimer === null) return;
  clearTimeout(pendingSearchPersistTimer);
  pendingSearchPersistTimer = null;
}

// Tabs the Probe Panel can show. Kept as a string-literal union (not a
// generated wire enum) because it's a pure UI concern — no backend serialises
// it. `usage` was added in issue #601 as the dedicated glanceable surface
// for Usage Meters (subscription quota + cash balance), reached from a
// meter icon in the sidebar header. `autopilot` was added in wayfinder
// #990 ticket #994 as the dedicated configure + monitor surface for the
// Issue-Driven and Looping Autopilot modes. `circuits` was added for the
// Autopilot Circuits walking skeleton (spec #1205 / issue #1206).
export type ProbeTab = 'files' | 'review' | 'usage' | 'properties' | 'autopilot' | 'circuits' | 'issues' | 'pulls' | 'sessions' | 'worktrees' | 'scratchpad';

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

interface UIState extends GridControls {
  // ---- Probe Panel (issue #373) ----
  // The Probe Panel is a unified right-hand surface for a focused context
  // (mesh + optional agent node): files, review, properties, issues, sessions,
  // and worktrees all live behind the same dock and switch by tab. Visibility
  // and active tab are stored here so every consumer (sidebar, global view
  // card, keyboard shortcut) reads/writes the same source of truth.
  probeOpen: boolean;
  probeTab: ProbeTab;
  // Working set for the Probe tool rail (ADR-0032): destinations the user
  // has opened this session, most-recently-used first, capped at
  // PROBE_WORKING_SET_CAP. Updated by `setProbeTab` (so `openProbeTab`
  // records visits transitively); session-only — never persisted.
  probeMru: readonly ProbeTab[];
  // Destination-local context captures (issue #1456). Host-lens tabs do
  // not use these. Mesh/Agent tabs follow selection until the user pins the
  // current stable id; the resolver keeps a missing pin visible instead of
  // silently falling back to a newly selected subject. Keeping one slot per
  // destination means pinning a second tab cannot erase the first tab's
  // protection when the user returns to it.
  probeContextPins: Partial<Record<ProbeTab, ProbeContextPin>>;
  // The single file currently shown in the Center Workspace Diff Overlay
  // (issue #379), or null when the overlay is closed. Independent of
  // `probeTab` — the overlay floats over the terminal grid and survives Probe
  // tab switches, so the user can keep the Probe open on any tab while
  // reviewing. Cleared by `closeDiff`, by Esc / "Back to Terminals", and by
  // the overlay's auto-close when the focused node or selected mesh changes.
  activeDiffFile: DiffContext | null;
  toggleProbe: () => void;
  setProbeTab: (tab: ProbeTab) => void;
  pinProbeContext: (pin: ProbeContextPin) => void;
  clearProbeContextPin: () => void;
  // Open the probe on a specific tab, opening the panel if it's collapsed.
  // The "click active tab to collapse" UX is left to ProbePanel's own
  // click handler — this is a pure "make the tab visible" action.
  openProbeTab: (tab: ProbeTab) => void;
  openDiff: (ctx: DiffContext) => void;
  closeDiff: () => void;

  // Full-screen circuit canvas editor (issue #1209), or null when closed.
  // Same overlay discipline as `activeDiffFile`: the editor covers the
  // center workspace while it's open — the TerminalManager singleton keeps
  // every PTY alive underneath, so closing returns to the exact grid.
  activeCircuitEditorId: number | null;
  openCircuitEditor: (circuitId: number) => void;
  closeCircuitEditor: () => void;

  // ---- Universal Command Omnibar (map #1371 Decision #2 / issue #1409) ----
  // Same overlay discipline as the circuit editor and diff overlay above:
  // the palette floats over the terminal grid while open. `omnibarMode`
  // seeds the palette's search box; the user can switch modes inside the
  // palette without touching the store.
  omnibarOpen: boolean;
  omnibarMode: OmnibarMode;
  openOmnibar: (mode?: OmnibarMode) => void;
  closeOmnibar: () => void;
  toggleOmnibar: (mode?: OmnibarMode) => void;

  // ---- Cross-surface modal open state (issue #1411 review) ----
  // The three modals a palette command can summon, hoisted here so every
  // owner (App's `?` key, TitleBar's header buttons, the Omnibar's command
  // router) reads and writes the same source of truth instead of gluing
  // themselves together with window CustomEvents. Same discipline as
  // `probeOpen` above: state + open/close actions, rendering decided by the
  // owning component.
  cheatsheetOpen: boolean;
  openCheatsheet: () => void;
  closeCheatsheet: () => void;
  toggleCheatsheet: () => void;
  appSettingsOpen: boolean;
  openAppSettings: () => void;
  closeAppSettings: () => void;
  remoteAccessOpen: boolean;
  openRemoteAccess: () => void;
  closeRemoteAccess: () => void;

  // Agent node currently under an OS file-drag, or null. Drives the terminal
  // "drop file to paste path" overlay; set by the window-level drop listener.
  dragTargetNodeId: number | null;
  setDragTargetNodeId: (nodeId: number | null) => void;

  // The active canvas View Mode (wayfinder #982 / ticket #983). Persisted
  // in localStorage (`buildmesh.view-mode`); sidebar mesh selection syncs
  // it via the subscription at the bottom of this module. What each mode
  // renders is decided in `src/lib/viewModes.ts` and `AgentNodeView` (#986).
  viewMode: ViewMode;
  // The grid mode 'single' was most recently entered from — the Escape /
  // restore path (`exitSingleMode`) returns here. Remembered by
  // `setViewMode` whenever a non-single mode is set.
  lastNonSingleMode: NonSingleViewMode;
  // Switch the canvas View Mode. Idempotent: a same-mode call is a no-op
  // (no subscriber notification, no storage write) so the meshStore sync
  // subscription can fire freely.
  setViewMode: (mode: ViewMode) => void;
  // Leave 'single' for the grid mode it was entered from. No-op when the
  // current mode isn't 'single'.
  exitSingleMode: () => void;

  // ---- Grid Controls (wayfinder #988 / ticket #995) ----
  // The five control fields themselves come from `GridControls`. Every setter
  // below is idempotent — a same-value call neither notifies subscribers nor
  // writes storage — matching `setViewMode`. That matters most for the search
  // box, which fires a setter per keystroke.
  setGridSearchQuery: (query: string) => void;
  setGridProviderFilter: (provider: string | null) => void;
  setGridStatusFilter: (status: SessionStatus | null) => void;
  setGridSortBy: (sortBy: GridSortBy) => void;
  setGridSortDirection: (direction: GridSortDirection) => void;
  // Clear every filter and return to the default manual ascending order —
  // the "clear all" behind #988's active-filter badges. Persists the cleared
  // set, so the reset survives a restart.
  resetGridControls: () => void;

  // ---- Grid search focus request (issue #998) ----
  // Monotonically increasing counter bumped by `requestFocusGridSearch()`. The
  // App.tsx `focus-grid-search` Tauri global-shortcut handler calls this
  // action; the `GridControls` component subscribes to the counter in a
  // layout effect and, on every increment, calls `.focus()` + `.select()` on
  // the rendered search input. This replaces the previous module-level ref
  // singleton (see PR review on the singleton anti-pattern) — a request
  // counter is the React-idiomatic "imperative command from outside the
  // component tree" channel: the producer (App.tsx) writes a number, the
  // consumer (GridControls) reacts via its existing store subscription, and
  // there is no module-level mutable state to leak across tests or races.
  //
  // The counter pattern (vs. a boolean `focusGridSearchRequested: boolean`)
  // handles re-presses: two ⌘+F presses bump the counter twice, and the
  // consumer effect fires twice (each bump is a distinct change), so focus
  // is set on every press — not just the first one.
  focusGridSearchRequest: number;
  requestFocusGridSearch: () => void;
}

export const useUIStore = create<UIState>((set, get) => {
  const initialViewMode = loadViewMode(useMeshStore.getState().selectedMeshId);
  const initialGridControls = loadGridControls();
  return {
    probeOpen: false,
    probeTab: 'files',
    probeMru: [],
    probeContextPins: {},
    activeDiffFile: null,

    toggleProbe: () => {
      set({ probeOpen: !get().probeOpen });
    },

    setProbeTab: (tab: ProbeTab) => {
      // Pure tab switch. The Center Workspace Diff Overlay (issue #379) is
      // independent of the active tab — it floats over the terminal grid — so
      // switching tabs no longer clears `activeDiffFile`. The overlay closes
      // only via `closeDiff` (Esc / "Back to Terminals") or its own auto-close
      // when the focused node or selected mesh changes.
      // Every activation also records a working-set visit for the tool rail
      // (ADR-0032); `openProbeTab` inherits this via its `setProbeTab` call,
      // so palette, title-bar, and contextual entries all feed the MRU.
      set((state) => ({
        probeTab: tab,
        probeMru: pushProbeWorkingSet(state.probeMru, tab),
      }));
    },

    pinProbeContext: (pin) => {
      set((state) => ({
        probeContextPins: {
          ...state.probeContextPins,
          [pin.tab]: pin,
        },
      }));
    },

    clearProbeContextPin: () => {
      const tab = get().probeTab;
      if (get().probeContextPins[tab] === undefined) return;
      const probeContextPins = { ...get().probeContextPins };
      delete probeContextPins[tab];
      set({ probeContextPins });
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

    activeCircuitEditorId: null,
    openCircuitEditor: (circuitId) => {
      set({ activeCircuitEditorId: circuitId });
    },
    closeCircuitEditor: () => {
      set({ activeCircuitEditorId: null });
    },

    omnibarOpen: false,
    omnibarMode: 'files',
    openOmnibar: (mode = 'files') => {
      // Opening an already-open palette just re-seeds the mode — the user
      // pressed the other chord while the palette was up, so switch it to
      // that mode rather than stacking a second palette.
      set({ omnibarOpen: true, omnibarMode: mode });
    },
    closeOmnibar: () => {
      if (!get().omnibarOpen) return;
      set({ omnibarOpen: false });
    },
    // Pressing the palette's activation chord while it is already open in
    // the same mode closes it (universal modal-toggle expectation).
    toggleOmnibar: (mode = 'files') => {
      const { omnibarOpen, omnibarMode } = get();
      if (omnibarOpen && omnibarMode === mode) {
        set({ omnibarOpen: false });
      } else {
        set({ omnibarOpen: true, omnibarMode: mode });
      }
    },

    cheatsheetOpen: false,
    openCheatsheet: () => {
      set({ cheatsheetOpen: true });
    },
    closeCheatsheet: () => {
      set({ cheatsheetOpen: false });
    },
    // The `?` key toggles (issue #731); the Omnibar command only opens.
    toggleCheatsheet: () => {
      set({ cheatsheetOpen: !get().cheatsheetOpen });
    },

    appSettingsOpen: false,
    openAppSettings: () => {
      set({ appSettingsOpen: true });
    },
    closeAppSettings: () => {
      set({ appSettingsOpen: false });
    },

    remoteAccessOpen: false,
    openRemoteAccess: () => {
      set({ remoteAccessOpen: true });
    },
    closeRemoteAccess: () => {
      set({ remoteAccessOpen: false });
    },

    // Idempotent "make this tab visible" — atomic `setProbeTab(tab) +
    // probeOpen = true`. Call sites stay one-liners; the inspector's close
    // button owns hiding (issue #1375 removed the always-visible rail, so
    // there is no collapse affordance beyond it). The probe tabs (#376, #377,
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

    viewMode: initialViewMode,
    // A boot straight into 'single' has no prior grid mode to remember —
    // 'all' is the neutral return target (matches the no-mesh-selected boot).
    lastNonSingleMode: initialViewMode === 'single' ? 'all' : initialViewMode,

    setViewMode: (mode) => {
      const { viewMode } = get();
      if (viewMode === mode) return;
      set({
        viewMode: mode,
        lastNonSingleMode: mode === 'single' ? get().lastNonSingleMode : mode,
      });
      persistViewMode(mode);
    },

    exitSingleMode: () => {
      get().setViewMode(get().lastNonSingleMode);
    },

    ...initialGridControls,

    setGridSearchQuery: (query) => {
      if (get().gridSearchQuery === query) return;
      set({ gridSearchQuery: query });
      // Debounced — see `scheduleDebouncedSearchPersist` above. State
      // updates immediately so the grid re-renders per keystroke; only
      // the localStorage write is coalesced into a single 300 ms-burst
      // call.
      scheduleDebouncedSearchPersist(get());
    },

    setGridProviderFilter: (provider) => {
      if (get().gridProviderFilter === provider) return;
      set({ gridProviderFilter: provider });
      persistGridControls(get());
    },

    setGridStatusFilter: (status) => {
      if (get().gridStatusFilter === status) return;
      set({ gridStatusFilter: status });
      persistGridControls(get());
    },

    setGridSortBy: (sortBy) => {
      if (get().gridSortBy === sortBy) return;
      set({ gridSortBy: sortBy });
      persistGridControls(get());
    },

    setGridSortDirection: (direction) => {
      if (get().gridSortDirection === direction) return;
      set({ gridSortDirection: direction });
      persistGridControls(get());
    },

    resetGridControls: () => {
      set({ ...DEFAULT_GRID_CONTROLS });
      persistGridControls(DEFAULT_GRID_CONTROLS);
    },

    focusGridSearchRequest: 0,
    // No idempotency guard here — unlike the setters above, this action
    // *must* notify subscribers on every press so the consumer's
    // `useLayoutEffect` runs each time. The counter is the only piece of
    // state the effect observes, so a same-value call would skip the
    // re-focus on the second ⌘+F of a re-press sequence. Two presses =
    // counter goes 0 → 1 → 2 = two effect runs = two .focus() calls.
    requestFocusGridSearch: () => {
      set({ focusGridSearchRequest: get().focusGridSearchRequest + 1 });
    },
  };
});

// Sidebar sync — "one filter, two controls" (wayfinder #982 / ticket #983,
// re-click-deselect → 'all' per ticket #986). Selecting a mesh in the
// sidebar switches the canvas to Mesh Grid for that mesh; clearing the
// selection switches to All Nodes. Pinned mode never writes selectedMeshId,
// but a sidebar mesh click always means "show me this mesh", so the sync
// applies in whatever mode the canvas is in. zustand notifies subscribers
// on every `set` — even same-value ones — so the prevState comparison
// filters no-op selectMesh calls before they can touch the view mode.
useMeshStore.subscribe((state, prevState) => {
  if (state.selectedMeshId === prevState.selectedMeshId) return;
  useUIStore.getState().setViewMode(state.selectedMeshId === null ? 'all' : 'mesh');
});
