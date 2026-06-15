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
 * Tab bodies land incrementally as follow-up issues under #372 complete:
 *   - #376: `ProjectFilesTab` (📁) and `AgentChangesTab` (🔍)
 *   - remaining tabs (`properties`, `worktrees`, `issues`, `sessions`)
 *     still render the placeholder pending their own issues.
 * Until then each tab renders a scaffold placeholder, and the friendly
 * empty states for "no project / no agent node" are wired here so the dock is
 * useful from day one.
 */

import { useUIStore, type ProbeTab } from '../../stores/uiStore';
import { useProbeContext } from '../../hooks/useProbeContext';
import { ProjectFilesTab } from './ProjectFilesTab';
import { AgentChangesTab } from './AgentChangesTab';
import { MeshPropertiesTab } from './MeshPropertiesTab';
import { GitIssuesTab } from './GitIssuesTab';
import { SessionHistoryTab } from './SessionHistoryTab';

interface ProbeTabDef {
  tab: ProbeTab;
  icon: string;
  /** Full descriptive name — used for the header title, tooltip, and the
   *  button's accessible name (aria-label). */
  label: string;
  /** Single-word caption shown under the icon in the narrow activity bar, so
   *  it stays legible at a readable size without wrapping. */
  short: string;
}

// Display order follows the PRD's activity-bar order (issue #374), which is
// deliberately different from the `ProbeTab` union's declaration order.
const PROBE_TABS: ProbeTabDef[] = [
  { tab: 'files', icon: '📁', label: 'Project Files', short: 'Files' },
  { tab: 'review', icon: '🔍', label: 'Agent Changes', short: 'Changes' },
  { tab: 'worktrees', icon: '🌳', label: 'Worktree Manager', short: 'Worktrees' },
  { tab: 'properties', icon: '⚙️', label: 'Mesh Properties', short: 'Properties' },
  { tab: 'issues', icon: '🐙', label: 'Git Issues', short: 'Issues' },
  { tab: 'sessions', icon: '🕒', label: 'Session History', short: 'History' },
];

const PROBE_BODY_WIDTH = 360;

export function ProbePanel() {
  const probeOpen = useUIStore((s) => s.probeOpen);
  const probeTab = useUIStore((s) => s.probeTab);
  const setProbeTab = useUIStore((s) => s.setProbeTab);
  const toggleProbe = useUIStore((s) => s.toggleProbe);

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
        <section
          role="region"
          aria-label="Probe panel"
          className="flex flex-col h-full overflow-hidden border-l border-border-subtle"
          style={{ width: PROBE_BODY_WIDTH }}
        >
          {/* Header — active tab label + collapse button */}
          <div
            className="flex items-center justify-between px-3 py-2 border-b border-border-subtle"
            style={{ minHeight: 40 }}
          >
            <span className="text-xs text-text-secondary font-medium truncate flex items-center gap-1.5 min-w-0">
              <span aria-hidden="true">{activeDef.icon}</span>
              <span className="truncate">{activeDef.label}</span>
            </span>
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
      )}

      {/* Activity bar — always visible so the dock can be reopened after close */}
      <nav
        aria-label="Probe tabs"
        className="flex flex-col w-16 shrink-0 py-1 border-l border-border-subtle bg-bg-surface"
      >
        {PROBE_TABS.map(({ tab, icon, label, short }) => {
          const isActive = probeOpen && probeTab === tab;
          return (
            <button
              key={tab}
              type="button"
              onClick={() => handleTabClick(tab)}
              aria-label={label}
              aria-pressed={isActive}
              title={label}
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
  // no node focused) still apply uniformly. The 🐙 and 🕒 tabs are
  // mesh-scoped but don't require an active node, so they fall through
  // to the "no project selected" guard only.
  if (tab === 'files') return <ProjectFilesTab />;
  if (tab === 'review') return <AgentChangesTab />;
  if (tab === 'properties') return <MeshPropertiesTab />;
  if (tab === 'issues') return <GitIssuesTab />;
  if (tab === 'sessions') return <SessionHistoryTab />;

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
