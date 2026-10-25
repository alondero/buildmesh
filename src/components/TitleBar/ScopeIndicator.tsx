import { useCallback, useLayoutEffect, useRef, useState } from 'react';

import { type AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore, type Mesh } from '../../stores/meshStore';
import { enterMesh } from '../../lib/navigation';
import { useUIStore } from '../../stores/uiStore';
import { type DerivedScope } from '../../lib/viewModes';
import { useScope } from '../../hooks/useScope';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useEscapeKey } from '../../hooks/useEscapeKey';
import { dropdownId } from '../../lib/dropdownId';

/**
 * ScopeIndicator — the title-bar control that names the active scope and
 * doubles as the Mesh picker (#2074).
 *
 * ## One derived scope, never a second derivation
 *
 * Every fact this control renders — is the scope Mesh-anchored, which Mesh,
 * how many Agent Nodes are visible, how many the scope held before the
 * filters narrowed it — comes from `useScope`, which delegates to
 * `deriveScope` (#2071). It never re-derives scope from the stores itself.
 * The switcher beside it picks the View Mode, this reads the scope that mode
 * produces, and the two cannot disagree.
 *
 * Mesh names are requested here (`meshNames: true`) and not on the canvas.
 * `AgentNodeView` renders no name, and subscribing it to the Mesh list would
 * re-render the terminal grid on every rename.
 *
 * ## The picker
 *
 * Choosing a Mesh calls `navigation.enterMesh` and nothing else — the same
 * navigation intent the sidebar Mesh row, the sidebar's Mesh-lens Probe
 * destinations and the omnibar's Mesh-scoped routes call (#2070 review,
 * extended by #2081 when the sidebar's four destination handlers were still
 * calling bare `selectMesh` and so diverged from the palette). That one
 * operation moves the canvas and the Probe destinations (which follow the
 * selection since #2073 deleted the Context Pins), so no action here can put
 * the two out of step, and a re-pick of the Mesh already in scope means the
 * same thing in every entrypoint. Escape and an outside click are dismissal,
 * not a decision: they close the panel and leave the scope exactly as it was.
 * Escape also restores focus to the trigger; an outside click leaves focus
 * where the user clicked.
 *
 * It is a disclosure dialog rather than an ARIA `menu` on purpose: the panel
 * holds plain buttons, so Tab order, Enter and Escape work without a roving
 * tabindex — the same contract `TitleBarOverflow` and `ZoomControl` already
 * establish for this cluster.
 *
 * ## Opening on request (#2076)
 *
 * `open` is local state, so a producer outside this component cannot set it.
 * Entering Mesh Grid with no Mesh selected is exactly that case: the switcher
 * segment sets the mode and asks for the picker instead of choosing a Mesh
 * for the user (#2071 deleted the guess), and the request arrives as a bump of
 * `useUIStore.openScopePickerRequest` — the same request-counter channel
 * `focusGridSearchRequest` already uses to drive `GridControls`. The effect
 * runs on every distinct bump, so a user who dismisses the panel and presses
 * the segment again gets it back. `openScopePickerRequest === 0` covers the
 * initial mount: a cold start must not pop a panel nobody asked for.
 *
 * Opening is not choosing. The request opens the panel and nothing else —
 * `selectedMeshId` is untouched, so the canvas keeps the honest "no Mesh
 * selected" state (#2071) behind it. Focus moves into the panel so the
 * `role="dialog"` is announced (a dialog opened without it says nothing to a
 * screen reader); Escape returns focus to this trigger.
 *
 * ## Width ladder
 *
 * Since #2421 this control only mounts from `LABELLED_SWITCHER_MIN_WIDTH`
 * (1440px); narrower windows get the `ViewSelector`, which shows `detail` in
 * text. Its label span is bounded (`max-w-[16rem]`, `truncate`) and hidden
 * below 1700px — the same width `UTILITIES_IN_BAR_MIN_WIDTH` returns the
 * utility pills at. The measurements below, taken when the segments and
 * pills shared one 1440px label tier, are why that number is 1700.
 *
 * Filtered is the worst case, and structurally so: it is the only mode whose
 * header grid is `grid-cols-[auto_minmax(0,1fr)_auto]`, so both flanking
 * tracks are content-sized `auto` and the centre absorbs whatever is left,
 * down to its own 16px padding floor. There the labelled indicator adds
 * 134.88px (32px glyph-only → 166.88px labelled) to a left track the switcher's
 * own labels have just grown from 518px to 726px. Measured against the real
 * window at the 13px root, that turns the Filtered search bar into an overlap
 * with the right cluster. With the switcher labels AND this label both
 * rendered:
 *
 *   viewport   left track   centre track   overflow   collides
 *   1400px     862px        16px           114px      YES   (old shared tier)
 *   1500px     893px        70px           73px       YES
 *   1600px     893px        170px          0          no
 *   1700px     893px        270px          0          no
 *   1920px     893px        490px          0          no
 *
 * The 1400px row is the measurement that motivated the old tier and is no
 * longer reachable: the switcher's labels do not appear below 1440px. The
 * labelled collision window is therefore exactly 1440–1599px — the band where
 * the switcher's labels have just appeared AND this label's would too. At the
 * 1440px boundary, derived from that width's measured glyph-only state (left
 * 758px, centre 145px, min-content 145px), paying this label's 134.88px drops
 * the centre to ~10px against a 143px min-content — still a collision. Below
 * 1440 neither label has paid (518px left track); at 1600 and up the centre is
 * wide enough to absorb the cost. Without the indicator the centre at 1440
 * fits its 145px content exactly, so the indicator — not the left cell, whose
 * `scrollWidth` equals its `clientWidth` — is what breaks the tier.
 *
 * 1700px rather than 1600px because 1600px is the first width measured CLEAR,
 * not a comfortable one: the centre lands at 170px against a 130–143px
 * min-content (the "Search or open" field, 143px of it once the
 * `SEARCH_SHORTCUT_LABEL` kbd chip is visible at the labelled tier), so the
 * whole margin is ~27px — and part of that min-content is a runtime string this
 * control does not own. At 1700px the centre is 270px, a measured 127px of
 * slack. Between 1440px and 1700px the indicator stays glyph-only, which costs
 * the centre nothing. 1600px would also be defensible if the chip label were
 * fixed-width; it is not. Raising the switcher's labelled tier to 1440px only
 * WIDENED the band this control hides through, so its own tier needs no move —
 * it is the right number today and stays it.
 *
 * `aria-label` carries the same string as the visible label, so the
 * accessible name survives the collapse (and WCAG 2.5.3 Label in Name holds
 * for free), and `title` spells the long form — so the glyph-only band costs
 * no information, only a glance.
 */

const PICKER_ID = dropdownId('titlebar', 'scope-picker');

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

/** Lucide `columns` — the Mesh-scoped glyph, identical to the Mesh Grid
 *  segment so the two controls read as one toolbar. */
function MeshGlyph({ className }: IconProps) {
  return (
    <Svg className={className}>
      <rect width="18" height="18" x="3" y="3" rx="2" />
      <path d="M12 3v18" />
    </Svg>
  );
}

/** Lucide `layers` — the cross-Mesh glyph. One glyph for All / Pinned /
 *  Filtered: the shape is "many Meshes", and the label beside it names which
 *  View Mode produced the count. */
function CrossMeshGlyph({ className }: IconProps) {
  return (
    <Svg className={className}>
      <path d="M12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83z" />
      <path d="m6.08 9.5-3.5 1.6a1 1 0 0 0 0 1.81l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9a1 1 0 0 0 0-1.83l-3.5-1.59" />
      <path d="m6.08 14.5-3.5 1.6a1 1 0 0 0 0 1.81l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9a1 1 0 0 0 0-1.83l-3.5-1.59" />
    </Svg>
  );
}

/** Lucide `maximize-2` — the Single glyph, matching that segment. */
function SingleGlyph({ className }: IconProps) {
  return (
    <Svg className={className}>
      <polyline points="15 3 21 3 21 9" />
      <polyline points="9 21 3 21 3 15" />
      <line x1="21" x2="14" y1="3" y2="10" />
      <line x1="3" x2="10" y1="21" y2="14" />
    </Svg>
  );
}

/** Lucide `check` — marks the picker's current Mesh. A glyph, not just the
 *  accent colour (colour is never the only signal). */
export function CheckGlyph({ className }: IconProps) {
  return (
    <Svg className={className}>
      <path d="M20 6 9 17l-5-5" />
    </Svg>
  );
}

function nodeCount(count: number): string {
  return `${count} ${count === 1 ? 'node' : 'nodes'}`;
}

export interface ScopeLabel {
  /** The full label, and the accessible name, verbatim. */
  text: string;
  /** The text the bar draws. Equal to `text` except for the cross-Mesh Filtered
   *  and Pinned labels, which drop their mode word: the highlighted segment
   *  already names the mode, and the full form squeezed the command field at
   *  1700-1760px (#2421 round 4). */
  visible: string;
  /** The scope half of the compact `ViewSelector` label (#2421), which names
   *  the View Mode itself, so this part omits it: a Mesh, a node, or
   *  "all meshes" plus the count — cross-Mesh scope stays explicit. */
  detail: string;
  /** The tooltip, which spells the long form while the label is collapsed. */
  tooltip: string;
  /** The shape glyph. Paired with `text` so colour is never the only signal. */
  Icon: (props: IconProps) => React.JSX.Element;
  /** Whether the control names a Mesh — the state that gets the accent. */
  meshScoped: boolean;
}

/**
 * The one place the rendered label is decided. Every value it needs comes
 * from the derived scope; the two lookups it does itself are node/Mesh
 * *identity* (the soloed node's own name, its parent Mesh's name), not a
 * second scope derivation — `deriveScope` resolves names out of the same
 * loaded Mesh list for the same reason.
 */
function fullScopeLabel(scope: DerivedScope, soloNode: AgentNode | null, meshes: Mesh[]): Omit<ScopeLabel, 'visible'> {
  // Single is checked before the Mesh-scoped branch on purpose: the canvas
  // shows ONE node in Single mode, so naming the grid's Mesh would name a
  // scope the user is not looking at.
  if (scope.viewMode === 'single') {
    if (soloNode === null) {
      return {
        text: 'Single · no agent node',
        detail: 'no agent node',
        tooltip: 'Single: no agent node to solo',
        Icon: SingleGlyph,
        meshScoped: false,
      };
    }
    // `focusedNodeMesh` only covers the focused node, and Single can solo a
    // fallback node when nothing is focused — so the parent Mesh is looked up
    // from the soloed node itself, which is honest in every case.
    const meshName = meshes.find(m => m.id === soloNode.mesh_id)?.name ?? null;
    return {
      text: meshName === null ? `Single · ${soloNode.name}` : `Single · ${soloNode.name} · ${meshName}`,
      detail: meshName === null ? soloNode.name : `${soloNode.name} · ${meshName}`,
      tooltip: meshName === null
        ? `Single: ${soloNode.name}`
        : `Single: ${soloNode.name} in ${meshName}`,
      Icon: SingleGlyph,
      meshScoped: false,
    };
  }

  if (scope.isMeshScoped) {
    // `mesh` is non-null whenever `isMeshScoped` is true. A null name is a
    // Mesh this surface has never seen (deleted mid-session, or still
    // loading) — show the id rather than invent a label.
    const mesh = scope.mesh!;
    const name = mesh.name ?? `Mesh #${mesh.id}`;
    return {
      text: name,
      detail: name,
      tooltip: `Mesh scope: ${name} — ${nodeCount(scope.visibleNodeCount)} in this Mesh. Choose another Mesh here.`,
      Icon: MeshGlyph,
      meshScoped: true,
    };
  }

  if (scope.gridMode === 'mesh') {
    // Mesh Grid with no Mesh selected. Since #2071 that is a real state with
    // its own empty state, not a gap to paper over — and #2076 owns what
    // entering it from the switcher should do.
    return {
      text: 'No mesh selected',
      detail: 'no mesh selected',
      tooltip: 'Mesh Grid with no Mesh selected — choose one here',
      Icon: MeshGlyph,
      meshScoped: false,
    };
  }

  const count = nodeCount(scope.visibleNodeCount);
  switch (scope.gridMode) {
    case 'all':
      return {
        text: `All meshes · ${count}`,
        detail: `all meshes · ${count}`,
        tooltip: `All Nodes across every Mesh — ${count}`,
        Icon: CrossMeshGlyph,
        meshScoped: false,
      };
    case 'pinned':
      return {
        text: `Pinned across meshes · ${count}`,
        detail: `all meshes · ${count}`,
        tooltip: `Pinned across every Mesh — ${count}`,
        Icon: CrossMeshGlyph,
        meshScoped: false,
      };
    case 'filtered':
      return {
        // Both counts, so "the filters hid everything" reads differently from
        // "nothing is in scope" (#1536) and from a broken render.
        text: `Filtered across meshes · ${scope.visibleNodeCount} of ${nodeCount(scope.scopedNodeCount)}`,
        detail: `all meshes · ${scope.visibleNodeCount} of ${scope.scopedNodeCount}`,
        tooltip: `Filtered across every Mesh — ${scope.visibleNodeCount} of ${nodeCount(scope.scopedNodeCount)} match`,
        Icon: CrossMeshGlyph,
        meshScoped: false,
      };
  }
}

/** The label the bar draws. Cross-Mesh Filtered and Pinned use the short form
 *  (see `ScopeLabel.visible`); every other mode draws its full text. */
function scopeLabel(scope: DerivedScope, soloNode: AgentNode | null, meshes: Mesh[]): ScopeLabel {
  const full = fullScopeLabel(scope, soloNode, meshes);
  const crossMesh = scope.viewMode !== 'single' && !scope.isMeshScoped && (scope.gridMode === 'filtered' || scope.gridMode === 'pinned');
  return { ...full, visible: crossMesh ? full.detail : full.text };
}

/**
 * The scope label for the current View Mode, plus the Mesh list and selection
 * the picker renders. One hook so `ScopeIndicator` and the compact
 * `ViewSelector` (#2421) read the same `useScope` result — neither can
 * name a scope the other would not.
 */
export function useScopeLabel(): ScopeLabel & { meshes: Mesh[]; selectedMeshId: number | null } {
  const scope = useScope({ meshNames: true });
  const meshes = useMeshStore(s => s.meshes);
  return { ...scopeLabel(scope, scope.currentNode, meshes), meshes, selectedMeshId: scope.selectedMeshId };
}

/** Lucide `chevron-down` — marks a trigger as a dropdown, so the scope
 *  control cannot read as one more View Mode segment (#2421). */
export function ChevronGlyph({ className }: IconProps) {
  return (
    <Svg className={className}>
      <path d="m6 9 6 6 6-6" />
    </Svg>
  );
}

/**
 * The Mesh rows both pickers render. Choosing a row is `enterMesh` and
 * nothing else (see "The picker" above); the caller closes its own panel.
 */
export function MeshPickerRows({ meshes, selectedMeshId, onPicked }: {
  meshes: Mesh[];
  selectedMeshId: number | null;
  onPicked: () => void;
}) {
  if (meshes.length === 0) {
    return <p className="px-2 py-2 text-xs text-text-muted">No meshes yet</p>;
  }
  return (
    <>
      {meshes.map((mesh) => {
        const current = mesh.id === selectedMeshId;
        return (
          <button
            key={mesh.id}
            type="button"
            onClick={() => {
              enterMesh(mesh.id);
              onPicked();
            }}
            data-testid={`scope-picker-mesh-${mesh.id}`}
            aria-current={current ? 'true' : undefined}
            className={`flex w-full min-w-0 items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm transition-colors hover:bg-bg-overlay ${
              current ? 'text-accent-cyan' : 'text-text-primary'
            }`}
          >
            <CheckGlyph className={`h-4 w-4 shrink-0 ${current ? '' : 'invisible'}`} />
            <span className="min-w-0 truncate font-medium">{mesh.name}</span>
          </button>
        );
      })}
    </>
  );
}

/**
 * Open a Mesh picker on every Mesh Grid request (#2076) made while it is
 * mounted. The request is a monotonically increasing counter
 * (`openScopePickerRequest`) the Mesh Grid segment bumps, read through the
 * store like everything else here. Only a change AFTER mount opens the panel:
 * the title bar mounts a different picker per width tier (#2421), so one can
 * mount long after the last request was answered, and a cold start must not
 * pop a panel nobody asked for either. `useLayoutEffect` so the panel mounts
 * before paint — the search-focus request in `GridControls` has the same
 * timing — and the picker's own focus move lands in that commit.
 */
export function useOpenOnPickerRequest(open: (value: true) => void): void {
  const request = useUIStore(s => s.openScopePickerRequest);
  const answered = useRef(request);
  useLayoutEffect(() => {
    if (request === answered.current) return;
    answered.current = request;
    open(true);
  }, [request, open]);
}

export function ScopeIndicator() {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const { text, visible, tooltip, Icon, meshScoped, meshes, selectedMeshId } = useScopeLabel();

  // Dismissal is not a decision, so every route that closes the panel hands
  // the user back where they were instead of leaving focus on <body>: the
  // panel is unmounted, and an unmounted focused element drops focus to
  // <body>, which loses a keyboard user's place entirely (#2081 review).
  // Escape returns focus here, matching the Escape contract `TitleBarOverflow`
  // and `ZoomControl` establish for this cluster. An outside click returns
  // focus here only when the click left it on <body>; see the
  // `useClickOutside` call below.
  const closeAndReturnFocus = useCallback(() => {
    const trigger = triggerRef.current;
    setOpen(false);
    requestAnimationFrame(() => trigger?.focus());
  }, []);

  // An outside click closes the panel. Focus stays on whatever the user
  // clicked; only if that left focus on <body> (an inert click) does it come
  // back to the trigger (#2421 review).
  const closeFromOutsideClick = useCallback(() => {
    const trigger = triggerRef.current;
    setOpen(false);
    requestAnimationFrame(() => {
      const active = document.activeElement;
      if (active === null || active === document.body) trigger?.focus();
    });
  }, []);
  useClickOutside(open ? PICKER_ID : null, closeFromOutsideClick);
  useEscapeKey(closeAndReturnFocus, open);

  // Hand focus to the panel as it opens, from whichever route opened it. A
  // `role="dialog"` the user is not moved into is silent for a screen reader —
  // pressing Mesh Grid and hearing nothing is indistinguishable from a
  // broken control. The panel itself takes focus (`tabIndex={-1}`, never a
  // tab stop) rather than its first Mesh, so Enter cannot pick a Mesh the
  // user only looked at; Tab reaches the rows, and Escape returns focus to
  // the trigger through `closeAndReturnFocus`.
  const panelRef = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    if (!open) return;
    panelRef.current?.focus();
  }, [open]);

  useOpenOnPickerRequest(setOpen);

  // `min-w-0`, not `shrink-0`: in the centred grid the left cell is capped at
  // half the leftover width, so a long label must truncate rather than run
  // under the "Search or open" field (#2421 measured "All meshes · 3 nodes"
  // overlapping it by 49px at 1920px).
  return (
    <div className="relative flex min-w-0 items-center" data-dropdown-for={open ? PICKER_ID : undefined}>
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setOpen(value => !value)}
        data-testid="scope-indicator"
        // The accessible name IS the visible label, so it survives the
        // collapse and satisfies Label in Name at the same time.
        aria-label={text}
        aria-expanded={open}
        aria-haspopup="dialog"
        aria-controls={open ? PICKER_ID : undefined}
        title={tooltip}
        // `HeaderPillButton`'s vocabulary, unchanged: borderless, card-hover,
        // active accent — the indicator reads as part of the same toolbar as
        // the switcher segments rather than a separate bordered group.
        className={`inline-flex h-9 min-w-0 items-center gap-1.5 rounded-md px-2 py-1.5 text-sm font-sans font-medium transition-colors ${
          meshScoped || open
            ? 'bg-bg-card text-accent-cyan'
            : 'text-text-secondary hover:bg-bg-card hover:text-text-primary'
        }`}
      >
        <Icon className="h-4 w-4 shrink-0" />
        {/* Bounded first, collapsed second — and collapsed at a LATER tier
            than the switcher's own (see the width-ladder note above): the
            label costs 134.88px, which the Filtered centre cell cannot absorb
            anywhere in the 1440–1599px labelled band. `max-[1700px]:hidden`
            must stay a class literal so Tailwind v4's source scanner compiles
            it. */}
        <span className="min-w-0 max-w-[16rem] truncate max-[1700px]:hidden">{visible}</span>
        {/* The chevron is what tells this glyph apart from the Mesh Grid and
            Single segments it shares a shape with (#2421). */}
        <span aria-hidden className="flex shrink-0" data-testid="scope-indicator-chevron">
          <ChevronGlyph className="h-3.5 w-3.5 opacity-70" />
        </span>
      </button>
      {open && (
        <div
          ref={panelRef}
          id={PICKER_ID}
          role="dialog"
          // Focusable, never a tab stop: the open effect focuses the panel so
          // the dialog is announced, and Tab from there reaches the rows.
          tabIndex={-1}
          aria-label="Select a Mesh"
          data-testid="scope-picker"
          className="absolute left-0 top-full z-50 mt-1 max-h-[calc(100dvh-4rem)] w-72 overflow-y-auto rounded-md border border-border-default bg-bg-card p-1 shadow-md animate-scale-in origin-top-left"
        >
          <MeshPickerRows meshes={meshes} selectedMeshId={selectedMeshId} onPicked={closeAndReturnFocus} />
        </div>
      )}
    </div>
  );
}