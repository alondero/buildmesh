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
 * `meshesById`, `selectedMeshId`, `activeNodeId`, `viewMode`, and
 * `probeTab`) all live behind that gate in `ProbePanelContent`,
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
 * The header shows the active destination's icon in a tinted chip next to its
 * label on the first line and its scope on the second. Selection-following
 * behavior is disclosed in the scope tooltip.
 * Issue #2075 promoted that lens/subject line to a legible weight: since
 * #2073 removed Probe Context Pins, disclosure is the only thing making a
 * selection-following destination's subject drift visible, so the subject is
 * no longer the quietest run in the header.
 * Body
 * content fades in on destination switch (keyed remount), and the whole body
 * slides in from the right when the inspector opens — both animations run
 * through the design-token keyframes in `App.css` and respect
 * `prefers-reduced-motion` via the global media query.
 */

import { Suspense, useRef } from 'react';
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
import { RelatedTools } from './RelatedTools';
import { probeGroupId } from '../../lib/probeGroups';
import { EmptyState } from '../shared/Spinner';
import {
  CompassIcon,
  SearchIcon,
  type ProbeIcon,
} from './probeIcons';

// The registry owns chunk loading and destination dispatch.
type ProbeTabDef = ProbeTabDefinition & { tab: ProbeTab; icon: ProbeIcon };

export const PROBE_TABS: readonly ProbeTabDef[] = PROBE_TAB_ORDER.map((tab) => ({
  tab,
  ...PROBE_TAB_DEFINITIONS[tab],
}));

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
  // a misleading mesh name and makes selection-following behavior visible
  // before a stateful action is taken.
  const context = useProbeContext();

  // Issue #1375 — the inspector is fully on-demand: closed means GONE. There
  // is no rail or always-visible button to reopen it; the palette, the
  // title-bar Usage action, and contextual entries own reopening.
  const activeDef = PROBE_TABS.find((t) => t.tab === probeTab) ?? PROBE_TABS[0];
  const ActiveIcon = activeDef.icon;
  const scopeLabel = context.mode === 'fixed' ? 'Host-wide' : context.subjectLabel;
  const scopeTitle = [scopeLabel, context.detailLabel, context.mode === 'following' ? 'Following selection' : null]
    .filter(Boolean).join(' · ');

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
        {/* Two lines: destination, then current scope. */}
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
              {/* Destination name — the heading the subject line sits under.
                  Issue #2075 promoted the subject to a `text-sm font-medium`
                  line, so the title takes `font-semibold`: without the extra
                  step the two header lines would render identically and the
                  header would stop saying which is the destination. */}
              <span
                data-testid="probe-context-title"
                title={activeDef.label}
                className="text-sm text-text-primary font-semibold truncate"
              >
                {activeDef.label}
              </span>
              {/* Issue #2075 — the disclosure. Since #2073 deleted Probe
                  Context Pins, this line IS the guarantee that the
                  destination's subject is visible while the user reads a diff
                  or a file tree: with two Agent Nodes in one Mesh, Agent
                  Changes swaps subject and Project Files swaps working tree
                  as focus moves. It therefore renders at the same size as the
                  destination name, in `text-text-primary` (the highest-contrast
                  text token) — previously a 12px `text-text-secondary` run,
                  the quietest thing in the header. Glyph + text still carry the
                  meaning, so colour is never the only signal. Size and colour
                  live on the spans rather than the row, so the row cannot
                  re-introduce the muted run by inheritance. */}
              <div
                data-testid="probe-context-subject"
                className="flex items-center gap-1 min-w-0"
                title={scopeTitle}
              >
                <span
                  data-testid="probe-context-subject-name"
                  className="truncate min-w-0 text-sm font-medium text-text-primary"
                >
                  {scopeLabel}
                </span>
                {/* Mixed-ownership qualifier: Project Files' working tree vs the
                    repository root, an Agent lens' parent Mesh. One step below
                    the subject it qualifies, and off `text-text-muted` — it is
                    the load-bearing half of the disclosure for the two
                    destinations whose subject moves under the user (#732). */}
                {context.detailLabel && (
                  <span
                    data-testid="probe-context-subject-detail"
                    className="truncate min-w-0 text-xs text-text-secondary"
                  >
                    · {context.detailLabel}
                  </span>
                )}
              </div>
            </div>
          </div>
          <button
            type="button"
            onClick={toggleProbe}
            className="p-1.5 rounded-md text-text-muted hover:text-text-primary hover:bg-bg-card transition-colors shrink-0"
            title="Close panel"
            aria-label="Close panel"
          >
            {/* Lucide `panel-right-close` — chevron points right, toward the
                edge the right-side dock collapses into. (The previous path
                `m10 9-3 3 3 3` was `panel-right-open`'s left-pointing
                chevron — the reopen affordance, not the close one.) With
                the rail gone the close fully hides the inspector; the
                palette, the title-bar Usage action, and contextual entries
                reopen it (issue #1375). */}
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
              <path d="m8 9 3 3-3 3" />
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
        <RelatedTools />
        <div className="flex-1 min-h-0 min-w-0 overflow-hidden">
          <div
            key={probeTab}
            role="tabpanel"
            id="probe-tab-panel"
            aria-labelledby={`probe-rail-tab-${probeGroupId(probeTab)}`}
            className="animate-fade-in h-full min-h-0 min-w-0 flex flex-col"
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

/** Resolve the adapter after applying the shared context guard. */
function ProbeTabBody({ tab }: { tab: ProbeTab }) {
  const context = useProbeContext();
  const { Component } = PROBE_TAB_DEFINITIONS[tab];
  if (!context.hasRequiredContext) return <ProbeContextEmptyState context={context} />;
  return <Component />;
}

function ProbeContextEmptyState({ context }: { context: ProbeContext }) {
  // Issue #2073 — losing the subject through ordinary selection loss lands
  // here with the destination's recovery instruction. There is no pin to
  // unpin: the destination simply has no subject yet.
  if (context.lens === 'agent') {
    return (
      <EmptyState
        icon={<SearchIcon className="w-5 h-5" />}
        label="No active agent node"
        hint="Agent lens: focus an agent terminal to review the changes it has made."
        fill
        testId="probe-context-empty"
      />
    );
  }

  return (
    <EmptyState
      icon={<CompassIcon className="w-5 h-5" />}
      label="No project selected"
      hint="Mesh lens: select a mesh in the sidebar, or focus an agent node, to inspect its files, changes, and settings here."
      fill
      testId="probe-context-empty"
    />
  );
}
