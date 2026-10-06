import { useCallback, useLayoutEffect, useMemo, useRef, useState } from 'react';

import { useAgentNodeStore, useAllAgentNodes, type AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore, type Mesh } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { deriveScope, resolveSingleNode, type DerivedScope } from '../../lib/viewModes';
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
 * filters narrowed it — comes from `deriveScope` (#2071). It never re-derives
 * scope from `selectedMeshId` or the focused node, which is exactly the drift
 * #2071 existed to delete: the switcher beside it picks the View Mode, this
 * reads the scope that mode produces, and the two cannot disagree.
 *
 * This is also the caller #2071's `ScopeInput.meshes` was written for. The
 * Mesh list is optional there because `AgentNodeView` renders no name; here
 * the name IS the label, so the list is passed. Subscription cost: the
 * indicator re-renders on a Mesh rename, which is one button's worth of work.
 * Adding the same subscription to `AgentNodeView` instead would re-render the
 * whole terminal grid on every rename — the reason the input is optional.
 *
 * ## The picker
 *
 * Choosing a Mesh calls `uiStore.enterMeshScope` and nothing else — the same
 * store operation the sidebar Mesh row, the sidebar's Mesh-lens Probe
 * destinations and the omnibar's Mesh-scoped routes call (#2070 review,
 * extended by #2081 when the sidebar's four destination handlers were still
 * calling bare `selectMesh` and so diverged from the palette). That one
 * operation moves the canvas and the Probe destinations (which follow the
 * selection since #2073 deleted the Context Pins), so no action here can put
 * the two out of step, and a re-pick of the Mesh already in scope means the
 * same thing in every entrypoint. Escape and an outside click are dismissal,
 * not a decision: they close the panel, restore focus to the trigger, and
 * leave the scope exactly as it was.
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
 * The label span is bounded (`max-w-[10rem]`, `truncate`) and its collapse tier
 * sits ABOVE every other control's: it is hidden below 1700px, where the
 * switcher segments and the utility pills already hide at 1400px (PR #1623).
 * That asymmetry is measured, not stylistic.
 *
 * Filtered is the worst case, and structurally so: it is the only mode whose
 * header grid is `grid-cols-[auto_minmax(0,1fr)_auto]`, so both flanking
 * tracks are content-sized `auto` and the centre absorbs whatever is left,
 * down to its own 16px padding floor. There the labelled indicator adds
 * 134.88px (32px glyph-only → 166.88px labelled) to a left track the switcher's
 * own labels have just grown from 518px to 862/893px. Measured against the
 * real window at the 13px root, that turns the Filtered search bar into an
 * overlap with the right cluster:
 *
 *   viewport   left track   centre track   overflow   collides
 *   1200px     518px        339px          0          no
 *   1300px     518px        439px          0          no
 *   1400px     862px        16px           114px      YES
 *   1500px     893px        70px           73px       YES
 *   1600px     893px        170px          0          no
 *   1700px     893px        270px          0          no
 *   1920px     893px        490px          0          no
 *
 * The collision window is therefore exactly 1400–1599px — the band where the
 * switcher's labels have just appeared AND this label's would too. Below 1400
 * neither has paid (518px left track, centre 339–439px); at 1600 and up the
 * centre is wide enough to absorb the cost. Without the indicator the centre
 * had ~167px at 1400px and fitted its 130px content, so the indicator — not the
 * left cell, whose `scrollWidth` equals its `clientWidth` — is what broke it.
 *
 * 1700px rather than 1600px because 1600px is the first width measured CLEAR,
 * not a comfortable one: the centre lands at 170px against a 130–143px
 * min-content (the "Search or open" field, 143px of it once the
 * `SEARCH_SHORTCUT_LABEL` kbd chip is visible at ≥1400px), so the whole margin
 * is ~27px — and part of that min-content is a runtime string this control does
 * not own. At 1700px the centre is 270px, a measured 127px of slack. Between
 * 1400px and 1700px the indicator stays glyph-only, which costs the centre
 * nothing. 1600px would also be defensible if the chip label were fixed-width;
 * it is not.
 *
 * The switcher's own 1400px tier does not move: that ladder was measured
 * against the same Filtered search bar and still holds. Only this control pays.
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
function CheckGlyph({ className }: IconProps) {
  return (
    <Svg className={className}>
      <path d="M20 6 9 17l-5-5" />
    </Svg>
  );
}

function nodeCount(count: number): string {
  return `${count} ${count === 1 ? 'node' : 'nodes'}`;
}

interface ScopeLabel {
  /** The visible label — also the accessible name, verbatim. */
  text: string;
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
function scopeLabel(scope: DerivedScope, soloNode: AgentNode | null, meshes: Mesh[]): ScopeLabel {
  // Single is checked before the Mesh-scoped branch on purpose: the canvas
  // shows ONE node in Single mode, so naming the grid's Mesh would name a
  // scope the user is not looking at.
  if (scope.viewMode === 'single') {
    if (soloNode === null) {
      return {
        text: 'Single · no agent node',
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
        tooltip: `All Nodes across every Mesh — ${count}`,
        Icon: CrossMeshGlyph,
        meshScoped: false,
      };
    case 'pinned':
      return {
        text: `Pinned across meshes · ${count}`,
        tooltip: `Pinned across every Mesh — ${count}`,
        Icon: CrossMeshGlyph,
        meshScoped: false,
      };
    case 'filtered':
      return {
        // Both counts, so "the filters hid everything" reads differently from
        // "nothing is in scope" (#1536) and from a broken render.
        text: `Filtered across meshes · ${scope.visibleNodeCount} of ${nodeCount(scope.scopedNodeCount)}`,
        tooltip: `Filtered across every Mesh — ${scope.visibleNodeCount} of ${nodeCount(scope.scopedNodeCount)} match`,
        Icon: CrossMeshGlyph,
        meshScoped: false,
      };
  }
}

export function ScopeIndicator() {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);

  const viewMode = useUIStore(s => s.viewMode);
  const lastNonSingleMode = useUIStore(s => s.lastNonSingleMode);
  // The open request (#2076) — a monotonically increasing counter the Mesh
  // Grid segment bumps. Read through the same store subscription as
  // everything else here, so there is no ref or event channel to keep in
  // step with this component.
  const openPickerRequest = useUIStore(s => s.openScopePickerRequest);
  const gridSearchQuery = useUIStore(s => s.gridSearchQuery);
  const gridProviderFilter = useUIStore(s => s.gridProviderFilter);
  const gridStatusFilter = useUIStore(s => s.gridStatusFilter);
  const selectedMeshId = useMeshStore(s => s.selectedMeshId);
  const meshes = useMeshStore(s => s.meshes);
  // Entering Mesh scope is one store operation (see the picker below); this
  // component does not decide what re-picking a Mesh in scope means.
  const enterMeshScope = useUIStore(s => s.enterMeshScope);
  const activeNodeId = useAgentNodeStore(s => s.activeNodeId);
  // `useShallow` on the element list, so a status flip re-renders this one
  // button while unrelated node writes (terminal output, circuit indicators)
  // leave the title bar alone. `GridFilterPopover` already holds this
  // subscription in the Filtered view, so the cost is new only elsewhere.
  const agentNodes = useAllAgentNodes();

  const controls = useMemo(
    () => ({ gridSearchQuery, gridProviderFilter, gridStatusFilter }),
    [gridSearchQuery, gridProviderFilter, gridStatusFilter],
  );
  const scope = useMemo(
    () => deriveScope({
      viewMode,
      lastNonSingleMode,
      agentNodes,
      selectedMeshId,
      activeNodeId,
      meshes,
      controls,
    }),
    [viewMode, lastNonSingleMode, agentNodes, selectedMeshId, activeNodeId, meshes, controls],
  );
  // The node Single solos, from the same helper the grid reads — so this
  // control can never name a different node than the one on screen.
  const soloNode = useMemo(
    () => viewMode === 'single'
      ? resolveSingleNode(agentNodes, activeNodeId, lastNonSingleMode, selectedMeshId, controls)
      : null,
    [viewMode, agentNodes, activeNodeId, lastNonSingleMode, selectedMeshId, controls],
  );

  const { text, tooltip, Icon, meshScoped } = scopeLabel(scope, soloNode, meshes);

  // Dismissal is not a decision, so every route that closes the panel hands
  // the user back where they were instead of leaving focus on <body>: the
  // panel is unmounted, and an unmounted focused element drops focus to
  // <body>, which loses a keyboard user's place entirely (#2081 review).
  // Escape and an outside click share this, so the two dismissals behave
  // identically — matching the Escape contract `TitleBarOverflow` and
  // `ZoomControl` already establish for this cluster.
  const closeAndReturnFocus = useCallback(() => {
    const trigger = triggerRef.current;
    setOpen(false);
    requestAnimationFrame(() => trigger?.focus());
  }, []);

  useClickOutside(open ? PICKER_ID : null, closeAndReturnFocus);
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

  // Open the picker on every distinct request (#2076). `useLayoutEffect` so
  // the panel is mounted before paint — same timing as the search-focus
  // request in `GridControls` — and so the focus move above lands in the same
  // commit as the mount.
  useLayoutEffect(() => {
    if (openPickerRequest === 0) return;
    setOpen(true);
  }, [openPickerRequest]);

  return (
    <div className="relative flex shrink-0 items-center" data-dropdown-for={open ? PICKER_ID : undefined}>
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
        className={`inline-flex h-9 shrink-0 items-center gap-1.5 rounded-md px-2 py-1.5 text-sm font-sans font-medium transition-colors ${
          meshScoped || open
            ? 'bg-bg-card text-accent-cyan'
            : 'text-text-secondary hover:bg-bg-card hover:text-text-primary'
        }`}
      >
        <Icon className="h-4 w-4 shrink-0" />
        {/* Bounded first, collapsed second — and collapsed at a LATER tier
            than the switcher's own (see the width-ladder note above): the
            label costs 134.88px, which the Filtered centre cell cannot absorb
            between 1400px and 1599px. `max-[1699px]:hidden` must stay a class
            literal so Tailwind v4's source scanner compiles it. */}
        <span className="min-w-0 max-w-[10rem] truncate max-[1699px]:hidden">{text}</span>
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
          {meshes.length === 0 ? (
            <p className="px-2 py-2 text-xs text-text-muted">No meshes yet</p>
          ) : meshes.map((mesh) => {
            const current = mesh.id === selectedMeshId;
            return (
              <button
                key={mesh.id}
                type="button"
                onClick={() => {
                  // One store operation, shared with the sidebar and the
                  // omnibar: a selection change moves the canvas through the
                  // uiStore mesh→mode subscription, and re-picking the Mesh
                  // already in scope returns the canvas to its grid (#2072).
                  // The Probe destinations follow the selection itself
                  // (#2073).
                  enterMeshScope(mesh.id);
                  closeAndReturnFocus();
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
        </div>
      )}
    </div>
  );
}