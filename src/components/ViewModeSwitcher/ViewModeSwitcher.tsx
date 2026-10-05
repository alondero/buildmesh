import { useMeshStore } from '../../stores/meshStore';
import { useUIStore, type ViewMode } from '../../stores/uiStore';

/**
 * ViewModeSwitcher — the five-segment View Mode control (wayfinder #982 /
 * ticket #983; Filtered added by #1609). Lives in the bespoke TitleBar (see
 * `components/TitleBar`, moved out of the old canvas header strip when the
 * window went frameless) and drives `uiStore.viewMode`. Hand-rolled
 * `<button>` + Tailwind per repo convention (no component library); icons
 * follow the `probeIcons.tsx` idiom — 24×24 viewBox, stroke="currentColor",
 * 1.75 width, round caps — so each glyph inherits the segment's text colour.
 *
 * Segment semantics:
 *   - Single:    solo the active node (subsumes the old maximize toggle).
 *   - Mesh Grid: scope to the sidebar-selected mesh. With no selection the
 *                segment asks for one instead of picking it (#2076): it sets
 *                the mode — so the canvas still lands in the Mesh Grid's
 *                "no Mesh selected" empty state, which #2071 made a real
 *                state after deleting the fallback that used to select the
 *                active node's mesh (or the first loaded one) for the user —
 *                and requests the title bar's Mesh picker so choosing a scope
 *                is an explicit act.
 *   - Pinned:    cross-mesh filter over is_pinned; never touches
 *                selectedMeshId.
 *   - All Nodes: clear the mesh selection — the only route out of Mesh
 *                scope since #2072 removed the sidebar's re-click-deselect
 *                gesture. The sync flips the mode to 'all'.
 *   - Filtered:  cross-mesh view narrowed by the Grid Controls (the Search
 *                Nodes bar renders next to the switcher only in this mode,
 *                #1609). Clicking the segment focuses that search — the
 *                user's next keystroke starts filtering without a second
 *                click.
 */

interface IconProps {
  className?: string;
}

function Svg({ className, children }: IconProps & { children: React.ReactNode }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.75"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      {children}
    </svg>
  );
}

/** Lucide `maximize-2` — Single. */
function SingleIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <polyline points="15 3 21 3 21 9" />
      <polyline points="9 21 3 21 3 15" />
      <line x1="21" x2="14" y1="3" y2="10" />
      <line x1="3" x2="10" y1="21" y2="14" />
    </Svg>
  );
}

/** Lucide `columns` — Mesh Grid. */
function MeshGridIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <rect width="18" height="18" x="3" y="3" rx="2" />
      <path d="M12 3v18" />
    </Svg>
  );
}

/** Lucide `pin` — Pinned. */
function PinnedIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <path d="M12 17v5" />
      <path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z" />
    </Svg>
  );
}

/** Lucide `filter` (funnel) — Filtered (#1609). The filled-to-the-line
    funnel reads as "narrowing" the way the grid glyphs read as layouts. */
function FilteredIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <polygon points="22 3 2 3 10 12.46 10 19 14 21 14 12.46 22 3" />
    </Svg>
  );
}

/** Lucide `layout-dashboard` — All Nodes. */
function AllNodesIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <rect width="7" height="9" x="3" y="3" rx="1" />
      <rect width="7" height="5" x="14" y="3" rx="1" />
      <rect width="7" height="9" x="14" y="12" rx="1" />
      <rect width="7" height="5" x="3" y="16" rx="1" />
    </Svg>
  );
}

interface Segment {
  mode: ViewMode;
  label: string;
  Icon: (props: IconProps) => React.JSX.Element;
}

const SEGMENTS: Segment[] = [
  { mode: 'single', label: 'Single', Icon: SingleIcon },
  { mode: 'mesh', label: 'Mesh Grid', Icon: MeshGridIcon },
  { mode: 'pinned', label: 'Pinned', Icon: PinnedIcon },
  { mode: 'all', label: 'All Nodes', Icon: AllNodesIcon },
  { mode: 'filtered', label: 'Filtered', Icon: FilteredIcon },
];

export function ViewModeSwitcher() {
  const viewMode = useUIStore(state => state.viewMode);
  const setViewMode = useUIStore(state => state.setViewMode);

  const handleSelect = (mode: ViewMode) => {
    // #2071/#2076 — Mesh Grid picks no Mesh. The mode is set and the Mesh
    // comes from the sidebar alone; #2071 deleted the fallback chain
    // (the active node's mesh, else the first loaded) that showed the user
    // a scope they never chose. With no selection this segment ASKS: it
    // requests the title bar's Mesh picker, which lists every Mesh and
    // chooses nothing. The mode still flips first — the notice never
    // suppresses the path it describes, and the canvas keeps its honest
    // "no Mesh selected" state behind the panel.
    if (mode === 'mesh') {
      setViewMode(mode);
      if (useMeshStore.getState().selectedMeshId === null) {
        useUIStore.getState().requestOpenScopePicker();
      }
      return;
    }
    if (mode === 'filtered') {
      // #1609 — switch first, then request focus. The request counter
      // pattern (App.tsx `focus-grid-search`) means the consumer's layout
      // effect runs on the bump, but the input only exists once this
      // render mounts `GridControls`; the bump is observed after commit,
      // so the ordering here is what makes the first click focus the
      // search. Re-clicking while already in Filtered re-arms the request
      // — the user's intent when clicking a segment they're already on is
      // "get me to the search box".
      //
      // #2076 — both halves of the gesture (the mode flip, the focus
      // request) and the cross-Mesh-results notice live in one store
      // action, because "the notice fires once per escape" is a property of
      // the gesture, not of each caller. Calling it is also why the notice
      // cannot fire on a re-click from Filtered: no escape happens then.
      useUIStore.getState().enterFilteredFromSearch();
      return;
    }
    setViewMode(mode);
  };

  return (
    <div role="group" aria-label="View mode" className="flex items-center gap-1">
      {SEGMENTS.map(({ mode, label, Icon }) => {
        const active = viewMode === mode;
        return (
          <button
            key={mode}
            type="button"
            aria-pressed={active}
            // aria-label keeps the accessible name when the visible label
            // hides on narrow windows (see the span below).
            aria-label={label}
            onClick={() => handleSelect(mode)}
            className={`inline-flex items-center gap-1.5 px-2 py-1.5 rounded-md text-sm font-sans font-medium whitespace-nowrap transition-colors ${
              active
                ? 'bg-bg-card text-accent-cyan'
                : 'text-text-secondary hover:text-text-primary hover:bg-bg-card'
            }`}
          >
            <Icon className="w-4 h-4 shrink-0" />
            {/* Icon-only below 1400px window width — at exactly 1300px
                the labels become visible but the centre's `w-80` (260px
                at the 13px root) plus the side clusters' min-content
                (~565px each when labels are visible) overflows the
                available side tracks and clips the last segment
                ("Filtered"). The 1400px floor keeps labels hidden in
                the 1300–1399px range where the layout can't support
                them (PR #1623 review). The aria-label keeps the
                accessible name stable. */}
            <span className="max-[1399px]:hidden">{label}</span>
          </button>
        );
      })}
    </div>
  );
}
