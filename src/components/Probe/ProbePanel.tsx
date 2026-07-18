/**
 * ProbePanel — the unified right-hand dock (issue #374, PRD #372).
 *
 * Layout is two columns: a wide collapsible **body** on the left and a thin,
 * always-visible **activity bar** on the right edge. The activity bar holds one
 * icon per tab; clicking an icon opens the body to that tab, clicking the
 * already-active icon again collapses it (mirroring VS Code's side-panel
 * behaviour). Because the bar stays visible while collapsed, the panel can
 * always be reopened after the header's close button hides the body.
 *
 * The body width is **horizontally resizable** (issue #724) via a separator
 * handle on the body's left edge. Default 360px, clamped to [240, 720];
 * the chosen width persists across launches via localStorage. The bounds
 * are wider than the sidebar's [192, 480] because the dock's primary job
 * is reviewing diffs (`AgentReviewPanel` uses ~104px of fixed-width gutter
 * per line — see `Diff.tsx` `DiffLineRow`), and narrower than the center
 * workspace needs to stay useful on common laptop resolutions.
 *
 * Tab bodies land incrementally as follow-up issues under #372 complete:
 *   - #376: `ProjectFilesTab` (📁) and `AgentChangesTab` (🔍)
 *   - #377: `WorktreeManagerTab` (🌳) — health recovery + branch / worktree
 *     prune + remote-tracking prune
 *   - remaining tabs (`issues`, `sessions`) still render the placeholder
 *     pending their own issues.
 * Until those land each unbuilt tab renders a scaffold placeholder, and the
 * friendly empty states for "no project / no agent node" are wired here so
 * the dock is useful from day one.
 */

import { useRef } from 'react';
import { useUIStore, type ProbeTab } from '../../stores/uiStore';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useProbeResize, PROBE_PANEL_BOUNDS } from './useProbeResize';
import { ProjectFilesTab } from './ProjectFilesTab';
import { AgentChangesTab } from './AgentChangesTab';
import { MeshPropertiesTab } from './MeshPropertiesTab';
import { WorktreeManagerTab } from './WorktreeManagerTab';
import { GitIssuesTab } from './GitIssuesTab';
import { GitPullRequestsTab } from './GitPullRequestsTab';
import { ArchivedNodesTab } from './ArchivedNodesTab';
import { ScratchpadTab } from './ScratchpadTab';
import { UsageTab } from './UsageTab';

interface ProbeTabDef {
  tab: ProbeTab;
  icon: string;
  /** Full descriptive name — used for the header title, fallback tooltip,
   *  and the button's accessible name (aria-label). */
  label: string;
  /** Optional longer tooltip for the activity-bar button. Falls back to
   *  `label` when omitted. */
  tooltip?: string;
  /** Single-word caption shown under the icon in the narrow activity bar, so
   *  it stays legible at a readable size without wrapping. */
  short: string;
}

// Display order follows the PRD's activity-bar order (issue #374), which is
// deliberately different from the `ProbeTab` union's declaration order.
// Issue #601 — `usage` is the dedicated glanceable surface for Usage Meters
// (subscription quota + cash balance). Placed early so the always-visible
// activity bar makes it a one-click glance: the bar stays visible even when
// the panel body is collapsed, so the user can always reopen the Usage tab.
const PROBE_TABS: ProbeTabDef[] = [
  { tab: 'files', icon: '📁', label: 'Project Files', short: 'Files' },
  { tab: 'review', icon: '🔍', label: 'Agent Changes', short: 'Changes' },
  { tab: 'usage', icon: '⏱', label: 'Usage', tooltip: 'Provider usage meters', short: 'Usage' },
  { tab: 'worktrees', icon: '🌳', label: 'Worktree Manager', short: 'Worktrees' },
  { tab: 'properties', icon: '⚙️', label: 'Mesh Properties', short: 'Properties' },
  { tab: 'issues', icon: '🐙', label: 'Git Issues', short: 'Issues' },
  { tab: 'pulls', icon: '🔀', label: 'Pull Requests', short: 'PRs' },
  { tab: 'sessions', icon: '🕒', label: 'Archive', tooltip: 'Archived Nodes', short: 'Archive' },
  { tab: 'scratchpad', icon: '📝', label: 'Scratch Pad', short: 'Notes' },
];

export function ProbePanel() {
  const probeOpen = useUIStore((s) => s.probeOpen);
  const probeTab = useUIStore((s) => s.probeTab);
  const setProbeTab = useUIStore((s) => s.setProbeTab);
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
  // The dock header surfaces the active mesh name as a subheading on every
  // tab, so each tab body stays free of redundant path/name chrome. The
  // hook degrades to `null` in the empty state; we render nothing in that
  // branch rather than blank-pad the row.
  const { activeMeshName } = useProbeContext();

  // The "click active tab to collapse, click any tab to open" rule composes the
  // store's two orthogonal primitives. Kept in the component so the store stays
  // a dumb state container that other call sites can reuse without inheriting
  // this widget's interaction semantics.
  const handleTabClick = (tab: ProbeTab) => {
    if (probeOpen && probeTab === tab) {
      toggleProbe();
      return;
    }
    setProbeTab(tab);
    if (!probeOpen) toggleProbe();
  };

  const activeDef = PROBE_TABS.find((t) => t.tab === probeTab) ?? PROBE_TABS[0];

  return (
    <div className="flex h-full shrink-0 bg-bg-surface">
      {probeOpen && (
        // Outer wrapper is `relative` but does NOT carry `overflow-hidden` —
        // the handle's `-left-1` extension must reach into the gap with the
        // center workspace to deliver the documented 10px hit zone. The
        // inner section owns the overflow-hidden + the border so the dock's
        // scroll content doesn't escape. (Same outer/inner split as the
        // sidebar's resize handle.)
        <div
          className="relative shrink-0"
          style={{ width: bodyWidth }}
        >
          {/* Resize handle — issue #724. Sits on the LEFT edge of the body
              (which is the inner edge of a right-side dock) and is keyboard-
              accessible via the WAI-ARIA APG separator pattern (Arrow keys
              step, Shift = 4× step, PageUp/Down = 10× step, Home/End jump
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
              // Drag-LEFT grows the panel (handle tracks cursor, panel's
              // right edge is fixed by the activity bar — see useProbeResize
              // for the geometry). Read the latest width through the ref
              // so auto-repeat doesn't collapse rapid presses into one
              // commit. setWidth clamps internally to PROBE_PANEL_BOUNDS.
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
          {/* Header — active tab label (title) + active mesh name
              (subheading) + collapse button. The subheading replaces the
              directory-path strip the Issues / PRs tabs used to render
              individually, so the same context appears uniformly across
              all 7 probe tabs. Bumped from 40px → 56px to fit the second
              line comfortably. */}
          <div
            className="flex items-center justify-between gap-2 px-3 py-2 border-b border-border-subtle min-h-[56px]"
          >
            <div className="flex flex-col min-w-0 flex-1">
              <span className="text-sm text-text-primary font-medium truncate flex items-center gap-1.5 min-w-0">
                <span aria-hidden="true">{activeDef.icon}</span>
                <span className="truncate">{activeDef.label}</span>
              </span>
              {activeMeshName && (
                <span
                  className="text-xs text-text-secondary truncate min-w-0"
                  title={activeMeshName}
                >
                  {activeMeshName}
                </span>
              )}
            </div>
            <button
              type="button"
              onClick={toggleProbe}
              className="text-text-muted hover:text-text-secondary transition-colors shrink-0 ml-2"
              title="Close panel"
              aria-label="Close panel"
            >
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
                <path d="M18 6 6 18M6 6l12 12" />
              </svg>
            </button>
          </div>

          {/* Body */}
          <div className="flex-1 overflow-y-auto">
            <ProbeTabBody tab={probeTab} />
          </div>
          </section>
        </div>
      )}

      {/* Activity bar — always visible so the dock can be reopened after close */}
      <nav
        aria-label="Probe tabs"
        className="flex flex-col w-16 shrink-0 py-1 border-l border-border-subtle bg-bg-surface"
      >
        {PROBE_TABS.map(({ tab, icon, label, short, tooltip }) => {
          const isActive = probeOpen && probeTab === tab;
          return (
            <button
              key={tab}
              type="button"
              onClick={() => handleTabClick(tab)}
              aria-label={tooltip ?? label}
              aria-pressed={isActive}
              title={tooltip ?? label}
              className={`flex flex-col items-center gap-1 px-1 py-2.5 transition-colors border-l-2 ${
                isActive
                  ? 'text-accent-cyan border-accent-cyan bg-bg-highlight'
                  : 'text-text-secondary border-transparent hover:text-text-primary hover:bg-bg-card/40'
              }`}
            >
              <span className="text-xl leading-none" aria-hidden="true">
                {icon}
              </span>
              <span className="text-xs font-medium leading-tight text-center">{short}</span>
            </button>
          );
        })}
      </nav>
    </div>
  );
}

/**
 * Switch-renders the active tab's content, falling back to a friendly empty
 * state when the derived context can't support the tab.
 */
function ProbeTabBody({ tab }: { tab: ProbeTab }) {
  const { activeMeshId, activeNodeId } = useProbeContext();

  // Host-scoped tabs render without a mesh selection. Today that's only
  // the Usage tab (issue #601) — it shows every detected harness and
  // keyed provider regardless of which mesh (if any) is focused. Adding
  // a new host-scoped tab means a new branch here.
  if (tab === 'usage') return <UsageTab />;

  if (activeMeshId === null) {
    return (
      <ProbeEmptyState
        icon="🧭"
        title="No project selected"
        message="Select a mesh in the sidebar, or focus an agent node, to inspect its files, changes, and settings here."
      />
    );
  }

  // "Agent Changes" reviews a specific node's edits — with no focused node
  // there's nothing to diff, even though a mesh is selected.
  if (tab === 'review' && activeNodeId === null) {
    return (
      <ProbeEmptyState
        icon="🔍"
        title="No active agent node"
        message="Focus an agent terminal to review the changes it has made."
      />
    );
  }

  // Real content for the tabs whose issues have landed. Routed before
  // the placeholder so the empty-state guards above (no mesh selected,
  // no node focused) still apply uniformly. The 🐙, 🔀 and 🕒 tabs are
  // mesh-scoped but don't require an active node, so they fall through
  // to the "no project selected" guard only. The ⏱ Usage tab is
  // host-scoped and is short-circuited above so it renders even with
  // no mesh selected.
  if (tab === 'files') return <ProjectFilesTab />;
  if (tab === 'review') return <AgentChangesTab />;
  if (tab === 'properties') return <MeshPropertiesTab />;
  if (tab === 'worktrees') return <WorktreeManagerTab />;
  if (tab === 'issues') return <GitIssuesTab />;
  if (tab === 'pulls') return <GitPullRequestsTab />;
  if (tab === 'sessions') return <ArchivedNodesTab />;
  if (tab === 'scratchpad') return <ScratchpadTab />;

  return <ProbeTabPlaceholder tab={tab} />;
}

/**
 * Scaffold shown for a tab whose real content is still being built in a
 * follow-up issue. Deliberately understated so it reads as "in progress",
 * not "broken".
 */
function ProbeTabPlaceholder({ tab }: { tab: ProbeTab }) {
  const def = PROBE_TABS.find((t) => t.tab === tab) ?? PROBE_TABS[0];
  return (
    <ProbeEmptyState
      icon={def.icon}
      title={def.label}
      message="This tab's content is coming soon."
    />
  );
}

function ProbeEmptyState({
  icon,
  title,
  message,
}: {
  icon: string;
  title: string;
  message: string;
}) {
  return (
    <div className="h-full flex items-center justify-center p-6 text-center">
      <div className="max-w-[260px]">
        <div className="text-2xl mb-2" aria-hidden="true">
          {icon}
        </div>
        <p className="text-sm text-text-primary font-medium mb-1">{title}</p>
        <p className="text-xs text-text-secondary leading-relaxed">{message}</p>
      </div>
    </div>
  );
}
