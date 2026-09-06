/**
 * ProbePanel — the on-demand right-side inspector (issue #374, PRD #372;
 * navigation model revised by issue #1375).
 *
 * Issue #1375 replaced the old always-visible activity rail with a
 * title-bar-first navigation model: destinations are opened through the
 * command palette ("Search or open…", Ctrl/Cmd+K), the title-bar Usage
 * action, and contextual entries (sidebar menus, node headers). This
 * component therefore renders **only while `probeOpen` is true** — when the
 * inspector is closed, no rail, replacement sidebar, or always-visible Probe
 * button remains, and the workspace regains the full width.
 *
 * **Closed-render discipline (PR #1489 review).** The exported `ProbePanel`
 * is a thin gatekeeper that subscribes to exactly one boolean (`probeOpen`)
 * and returns `null` when the inspector is hidden. The full body, the
 * `useProbeResize` hook (which reads `localStorage` and arms the drag
 * handles), and `useProbeContext` (which subscribes to `nodesById`,
 * `meshesById`, `selectedMeshId`, `activeNodeId`, `viewMode`, `probeTab`,
 * and `probeContextPins`) all live behind that gate in `ProbePanelContent`,
 * so a hidden panel performs zero work — no store subscriptions fire, no
 * resize state mutates, no context resolves. Every re-render of the closed
 * panel would otherwise walk into `useProbeContext` and `bodyWidthRef`
 * before bailing at the `if (!probeOpen) return null;` check.
 * The `openProbeDestination` test helper must keep calling `render(<ProbePanel />)`
 * — mounting the outer component is fine because it returns `null` when closed,
 * and the helper flips `probeOpen` on via the store before rendering.
 *
 * The body width is **horizontally resizable** (issue #724) via a separator
 * handle on the body's left edge. Default 360px, clamped to [240, 720];
 * the chosen width persists across launches via localStorage. The bounds
 * are wider than the sidebar's [192, 480] so the inspector can accommodate
 * dense file lists while the center workspace stays useful on common laptop
 * resolutions.
 *
 * The header pins the active destination's icon in a tinted chip next to its
 * label, with the explicit lens/subject and following-or-pinned mode visible
 * so each destination body stays free of redundant context chrome. Body
 * content fades in on destination switch (keyed remount), and the whole body
 * slides in from the right when the inspector opens — both animations run
 * through the design-token keyframes in `App.css` and respect
 * `prefers-reduced-motion` via the global media query.
 */

import { Suspense, lazy, useRef } from 'react';
import { useUIStore, type ProbeTab } from '../../stores/uiStore';
import { useProbeContext } from '../../hooks/useProbeContext';
import {
  PROBE_TAB_DEFINITIONS,
  PROBE_TAB_ORDER,
  type ProbeContext,
  type ProbeTabDefinition,
} from '../../lib/probeContext';
import { useProbeResize, PROBE_PANEL_BOUNDS } from './useProbeResize';
import { ProbeToolRail, LABEL_COLLAPSE_WIDTH } from './ProbeToolRail';
import { EmptyState } from '../shared/Spinner';
import {
  CompassIcon,
  PROBE_TAB_ICONS,
  SearchIcon,
  type ProbeIcon,
} from './probeIcons';

// Issue #1568 - lazy-load each tab so the initial bundle doesn't pay for
// every inspector surface the user may never open. The tabs are 4-71 KB
// each (WorktreeManagerTab alone is 71 KB); until the inspector is opened
// (probeOpen===true) the user's app boots without any of them. Even after
// opening, only the active tab's chunk is fetched — switching tabs brings
// the next one down on demand. The lazy components are intentionally
// defined at module scope so they're not recreated on every render.
const ProjectFilesTab = lazy(() => import('./ProjectFilesTab').then((m) => ({ default: m.ProjectFilesTab })));
const AgentChangesTab = lazy(() => import('./AgentChangesTab').then((m) => ({ default: m.AgentChangesTab })));
const MeshPropertiesTab = lazy(() => import('./MeshPropertiesTab').then((m) => ({ default: m.MeshPropertiesTab })));
const WorktreeManagerTab = lazy(() => import('./WorktreeManagerTab').then((m) => ({ default: m.WorktreeManagerTab })));
const AutopilotProbeTab = lazy(() => import('./AutopilotProbeTab').then((m) => ({ default: m.AutopilotProbeTab })));
const CircuitsProbeTab = lazy(() => import('./CircuitsProbeTab').then((m) => ({ default: m.CircuitsProbeTab })));
const GitIssuesTab = lazy(() => import('./GitIssuesTab').then((m) => ({ default: m.GitIssuesTab })));
const GitPullRequestsTab = lazy(() => import('./GitPullRequestsTab').then((m) => ({ default: m.GitPullRequestsTab })));
const ArchivedNodesTab = lazy(() => import('./ArchivedNodesTab').then((m) => ({ default: m.ArchivedNodesTab })));
const ScratchpadTab = lazy(() => import('./ScratchpadTab').then((m) => ({ default: m.ScratchpadTab })));
const UsageTab = lazy(() => import('./UsageTab').then((m) => ({ default: m.UsageTab })));

type ProbeTabDef = ProbeTabDefinition & { tab: ProbeTab; icon: ProbeIcon };

export const PROBE_TABS: readonly ProbeTabDef[] = PROBE_TAB_ORDER.map((tab) => ({
  tab,
  icon: PROBE_TAB_ICONS[tab],
  ...PROBE_TAB_DEFINITIONS[tab],
}));

function ContextPinIcon({ className = 'w-3.5 h-3.5' }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="m12 17 5-5" />
      <path d="M9 3h6l1 5-4 4v5l-2 2v-7L6 8l1-5Z" />
      <path d="M5 21h14" />
    </svg>
  );
}

/**
 * Issue #1568 — `Suspense` fallback shown while a lazy Probe tab chunk is
 * in flight. The body takes the full flex region (`flex-1`) so the
 * surrounding fade-in doesn't jump when the chunk lands, and the spinner
 * inherits the muted palette so it reads as "loading" rather than
 * "broken". Tests that open a tab can stub the fetch with a synchronous
 * `<EmptyState>` instead of waiting on the chunk.
 */
function ProbeTabLoadingShell() {
  return (
    <div className="flex-1 flex items-center justify-center text-text-muted">
      <div className="flex flex-col items-center gap-2">
        <span className="inline-block h-5 w-5 animate-spin rounded-full border-2 border-text-muted border-t-transparent" />
        <span className="text-xs text-text-muted">Loading…</span>
      </div>
    </div>
  );
}

/**
 * Closed-render gatekeeper (PR #1489 review): subscribe to exactly one
 * boolean and return null when the inspector is hidden. All other
 * subscriptions, the resize hook, and the context resolver live in
 * `ProbePanelContent` below so a hidden panel does zero work.
 */
export function ProbePanel() {
  const probeOpen = useUIStore((s) => s.probeOpen);
  if (!probeOpen) return null;
  return <ProbePanelContent />;
}

/**
 * The full inspector body — only mounted while `probeOpen` is true.
 * Everything that fires every store update lives inside this component so
 * the closed-state `ProbePanel` render cost is one boolean comparison.
 */
function ProbePanelContent() {
  const probeTab = useUIStore((s) => s.probeTab);
  const toggleProbe = useUIStore((s) => s.toggleProbe);
  const pinProbeContext = useUIStore((s) => s.pinProbeContext);
  const clearProbeContextPin = useUIStore((s) => s.clearProbeContextPin);
  // Issue #724 — the panel is now horizontally resizable (default 360px,
  // clamped 240–720). The shared `useResizable` hook's valueRef pattern
  // (issue #301) prevents the second-drag stale-closure jump; the wrapper
  // hook adds localStorage persistence so the chosen width survives a
  // relaunch. The handle lives on a `relative` outer wrapper (not inside
  // the overflow-hidden section) so the `-left-1` extension actually
  // reaches the gap with the center workspace instead of being clipped.
  const { width: bodyWidth, isResizing, handleMouseDown, setWidth: setBodyWidth } = useProbeResize();
  // Mirror bodyWidth into a ref so the keyboard handler reads the latest
  // value on every keydown — the same stale-closure anti-pattern
  // useResizable's valueRef guards against for the drag path. Without
  // this, auto-repeat arrow keys (held) call setBodyWidth with the same
  // stale bodyWidth N times before React commits, collapsing the rapid
  // expansion into a single +8 step per render.
  const bodyWidthRef = useRef(bodyWidth);
  bodyWidthRef.current = bodyWidth;
  // The inspector header surfaces the destination's explicit lens and subject.
  // Unlike the old mesh-only subheading, this keeps Host usage from inheriting
  // a misleading mesh name and makes selection-following/pinned behavior
  // visible before a stateful action is taken.
  const context = useProbeContext();

  // Issue #1375 — the inspector is fully on-demand: closed means GONE. There
  // is no rail or always-visible button to reopen it; the palette, the
  // title-bar Usage action, and contextual entries own reopening.
  const activeDef = PROBE_TABS.find((t) => t.tab === probeTab) ?? PROBE_TABS[0];
  const ActiveIcon = activeDef.icon;
  const contextModeLabel = context.mode === 'pinned'
    ? 'Pinned context'
    : context.lens === 'host'
      ? 'Host-wide'
      : context.followsSelection
        ? 'Following selection'
        : 'Fixed context';
  const handlePinToggle = () => {
    if (context.mode === 'pinned') {
      clearProbeContextPin();
    } else if (context.pinCandidate !== null) {
      pinProbeContext(context.pinCandidate);
    }
  };

  return (
    // Outer wrapper is `relative` but does NOT carry `overflow-hidden` —
    // the handle's `-left-1` extension must reach into the gap with the
    // center workspace to deliver the documented 10px hit zone. The
    // inner section owns the overflow-hidden + the border so the dock's
    // scroll content doesn't escape. (Same outer/inner split as the
    // sidebar's resize handle.) The slide-in animation plays once per
    // open; since the whole inspector unmounts on close, nothing else
    // lingers in the layout.
    <div
      className="relative h-full shrink-0 animate-slide-in-right"
      style={{ width: bodyWidth }}
    >
      {/* Resize handle — issue #724. Sits on the LEFT edge of the body
          (which is the inner edge of a right-side inspector) and is
          keyboard-accessible via the WAI-ARIA APG separator pattern (Arrow
          keys step, Shift = 4× step, PageUp/Down = 10× step, Home/End jump
          to min/max). The 10px hit zone (`w-2.5`) extends 4px into the
          gap with the center workspace via `-left-1`; the 2px visible
          line (`after:w-0.5`) is centred at the panel's actual border
          so the affordance reads as part of the dock. Reads the latest
          width through `bodyWidthRef` (not the closed-over state) so
          auto-repeat keydowns see the post-commit value on the next
          event — mirrors useResizable's valueRef pattern. */}
      <div
        onMouseDown={handleMouseDown}
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize probe panel"
        aria-valuenow={bodyWidth}
        aria-valuemin={PROBE_PANEL_BOUNDS.MIN_WIDTH}
        aria-valuemax={PROBE_PANEL_BOUNDS.MAX_WIDTH}
        tabIndex={0}
        onKeyDown={(e) => {
          // Drag-LEFT grows the panel (handle tracks cursor; the panel's
          // right edge is the window edge — see useProbeResize for the
          // geometry). Read the latest width through the ref so
          // auto-repeat doesn't collapse rapid presses into one commit.
          // setWidth clamps internally to PROBE_PANEL_BOUNDS.
          const current = bodyWidthRef.current;
          const min = PROBE_PANEL_BOUNDS.MIN_WIDTH;
          const max = PROBE_PANEL_BOUNDS.MAX_WIDTH;
          const small = e.shiftKey ? 32 : 8;
          const large = e.shiftKey ? max - min : 80;
          let next: number | null = null;
          switch (e.key) {
            case 'ArrowLeft':  next = Math.min(max, current + small); break;
            case 'ArrowRight': next = Math.max(min, current - small); break;
            case 'PageUp':     next = Math.min(max, current + large); break;
            case 'PageDown':   next = Math.max(min, current - large); break;
            case 'Home':       next = max; break;
            case 'End':        next = min; break;
          }
          if (next !== null) {
            setBodyWidth(next);
            e.preventDefault();
          }
        }}
        className={`absolute top-0 -left-1 w-2.5 h-full cursor-col-resize z-10 outline-none focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent-cyan after:absolute after:inset-y-0 after:left-1 after:w-0.5 after:transition-colors ${
          isResizing ? 'after:bg-accent-cyan/60' : 'after:bg-transparent hover:after:bg-accent-cyan/40'
        }`}
      />

      <section
        role="region"
        aria-label="Probe panel"
        className="flex flex-col h-full w-full overflow-hidden border-l border-border-subtle"
      >
        {/* Header — active destination icon chip + label (title) + explicit
            lens/subject + following/pinned mode + close button. The subject
            line replaces the directory-path strip the Issues / PRs tabs
            used to render individually, so every destination makes its
            ownership visible in the same place. */}
        <div
          className="flex items-center justify-between gap-2 pl-3 pr-2 py-2 border-b border-border-subtle min-h-[56px]"
        >
          <div className="flex items-center gap-2.5 min-w-0 flex-1">
            <span
              aria-hidden="true"
              className="flex items-center justify-center w-7 h-7 rounded-md bg-accent-cyan/10 text-accent-cyan shrink-0"
            >
              <ActiveIcon className="w-4 h-4" />
            </span>
            <div className="flex flex-col min-w-0 flex-1">
              <span className="text-sm text-text-primary font-medium truncate">
                {activeDef.label}
              </span>
              <div
                data-testid="probe-context-subject"
                className="flex items-center gap-1 min-w-0 text-xs text-text-secondary"
                title={context.subjectLabel}
              >
                <span className="truncate min-w-0">{context.subjectLabel}</span>
                {context.detailLabel && (
                  <span className="truncate min-w-0 text-text-muted">
                    · {context.detailLabel}
                  </span>
                )}
              </div>
              <span
                data-testid="probe-context-mode"
                className="text-2xs text-text-muted/80 truncate"
              >
                {contextModeLabel}
              </span>
            </div>
          </div>
          {context.canPin && (
            <button
              type="button"
              onClick={handlePinToggle}
              data-testid="probe-context-pin"
              aria-pressed={context.mode === 'pinned'}
              className={`p-1.5 rounded-md transition-colors shrink-0 ${
                context.mode === 'pinned'
                  ? 'text-accent-cyan bg-accent-cyan/10'
                  : 'text-text-muted hover:text-text-primary hover:bg-bg-card'
              }`}
              title={context.mode === 'pinned' ? 'Unpin context' : 'Pin context'}
              aria-label={context.mode === 'pinned' ? 'Unpin context' : 'Pin context'}
            >
              <ContextPinIcon />
            </button>
          )}
          <button
            type="button"
            onClick={toggleProbe}
            className="p-1.5 rounded-md text-text-muted hover:text-text-primary hover:bg-bg-card transition-colors shrink-0"
            title="Close panel"
            aria-label="Close panel"
          >
            {/* Lucide `panel-right-close` — reads as "collapse the dock"
                rather than "dismiss a dialog". With the rail gone the close
                fully hides the inspector; the palette, the title-bar Usage
                action, and contextual entries reopen it (issue #1375). */}
            <svg
              width="14"
              height="14"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <rect width="18" height="18" x="3" y="3" rx="2" />
              <path d="M15 3v18" />
              <path d="m10 9-3 3 3 3" />
            </svg>
          </button>
        </div>

        {/* Tool rail — working-set tabs for destinations opened this session
            plus the grouped "All tools" menu (ADR-0032). Sits outside the
            body scroller per probe-ui-checklist.md §1 (toolbars are
            shrink-0 siblings of the scroller). Renders only while the panel
            is open — ADR-0030's closed-render discipline is unchanged. */}
        <ProbeToolRail narrow={bodyWidth < LABEL_COLLAPSE_WIDTH} />

        {/* Body — the inner wrapper is keyed by destination so switching
            destinations remounts the content and replays the fade-in once
            per destination. The wrapper preserves the h-full/flex chain the
            destination roots rely on for their internal `flex-1 overflow`
            regions. Issue #1568 — every tab is now a `React.lazy` chunk,
            so we wrap the keyed body in `<Suspense>` to host the brief
            loading state the chunk fetch produces on first open. The
            tabpanel wiring points at the tool rail's tabs (ADR-0032). */}
        <div className="flex-1 overflow-y-auto">
          <div
            key={probeTab}
            role="tabpanel"
            id="probe-tab-panel"
            aria-labelledby={`probe-rail-tab-${probeTab}`}
            className="animate-fade-in h-full flex flex-col"
          >
            <Suspense fallback={<ProbeTabLoadingShell />}>
              <ProbeTabBody tab={probeTab} />
            </Suspense>
          </div>
        </div>
      </section>
    </div>
  );
}

/**
 * Switch-renders the active tab's content, falling back to a friendly empty
 * state when the derived context can't support the tab.
 */
function ProbeTabBody({ tab }: { tab: ProbeTab }) {
  const context = useProbeContext();
  const definition = PROBE_TAB_DEFINITIONS[tab];

  // Host-lens tabs render without a mesh selection. Today that's only
  // the Usage tab (issue #601) — it shows every detected harness and
  // keyed provider regardless of which mesh (if any) is focused. Adding
  // a new host-lens tab means a new branch here.
  if (definition.lens === 'host') {
    if (tab === 'usage') return <UsageTab />;
    return <ProbeTabPlaceholder tab={tab} />;
  }

  if (!context.hasRequiredContext) return <ProbeContextEmptyState context={context} />;

  // "Agent Changes" lists a specific node's edits — with no focused node
  // there's nothing to inspect, even though a mesh is selected.
  // The Agent lens reports a missing node through the same contract as a
  // missing Mesh; its empty state names the lens and whether a pin is stale.

  // Real content for the tabs whose issues have landed. Routed after the
  // explicit context guard so each destination gets the same missing-subject
  // behavior. Mesh-lens Issues, PRs, and Archive do not require an active
  // node; the Agent Changes destination does. Usage is Host-lens and is
  // short-circuited above so it renders even with no Mesh selected.
  if (tab === 'files') return <ProjectFilesTab />;
  if (tab === 'review') return <AgentChangesTab />;
  if (tab === 'properties') return <MeshPropertiesTab />;
  if (tab === 'autopilot') return <AutopilotProbeTab />;
  if (tab === 'circuits') return <CircuitsProbeTab />;
  if (tab === 'worktrees') return <WorktreeManagerTab />;
  if (tab === 'issues') return <GitIssuesTab />;
  if (tab === 'pulls') return <GitPullRequestsTab />;
  if (tab === 'sessions') return <ArchivedNodesTab />;
  if (tab === 'scratchpad') return <ScratchpadTab />;

  return <ProbeTabPlaceholder tab={tab} />;
}

function ProbeContextEmptyState({ context }: { context: ProbeContext }) {
  if (context.lens === 'agent') {
    return (
      <EmptyState
        icon={<SearchIcon className="w-5 h-5" />}
        label={context.mode === 'pinned' ? 'Pinned agent unavailable' : 'No active agent node'}
        hint={context.mode === 'pinned'
          ? 'Agent lens is pinned to a node that is no longer available. Unpin the context to follow the current selection.'
          : 'Agent lens: focus an agent terminal to review the changes it has made.'}
        fill
        testId="probe-context-empty"
      />
    );
  }

  return (
    <EmptyState
      icon={<CompassIcon className="w-5 h-5" />}
      label={context.mode === 'pinned'
        ? context.subject.available ? 'Pinned file context unavailable' : 'Pinned mesh unavailable'
        : 'No project selected'}
      hint={context.mode === 'pinned'
        ? context.subject.available
          ? 'The pinned working tree is no longer available. Unpin the context to follow the current selection.'
          : 'Mesh lens is pinned to a mesh that is no longer available. Unpin the context to follow the current selection.'
        : 'Mesh lens: select a mesh in the sidebar, or focus an agent node, to inspect its files, changes, and settings here.'}
      fill
      testId="probe-context-empty"
    />
  );
}

/**
 * Scaffold shown for a tab whose real content is still being built in a
 * follow-up issue. Deliberately understated so it reads as "in progress",
 * not "broken".
 */
function ProbeTabPlaceholder({ tab }: { tab: ProbeTab }) {
  const def = PROBE_TABS.find((t) => t.tab === tab) ?? PROBE_TABS[0];
  const Icon = def.icon;
  return (
    <EmptyState
      icon={<Icon className="w-5 h-5" />}
      label={def.label}
      hint="This tab's content is coming soon."
      fill
    />
  );
}
