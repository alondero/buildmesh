import { useCallback, useRef, useState } from 'react';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useEscapeKey } from '../../hooks/useEscapeKey';
import { useUIStore } from '../../stores/uiStore';
import { HeaderPillButton } from './HeaderPillButton';
import { ProbeTab, PROBE_TAB_DEFINITIONS } from '../../lib/probeContext';
import { PROBE_TAB_ICONS } from '../Probe/probeIcons';
import { dropdownId } from '../../lib/dropdownId';

/**
 * Title-bar overflow for the two project destinations (issue #1460).
 *
 * The maintainer note on #1460 requires Project Settings and Repository to be
 * reachable as "clearly labelled global/project actions from the command
 * palette and title-bar overflow", and to consume no permanent Probe rail
 * space. The palette half already existed (`probe-properties` /
 * `probe-worktrees`); this is the title-bar half. It is a *disclosure*, not
 * a rail: nothing is rendered until the user opens it, and the inspector
 * still closes back to nothing at all.
 *
 * It is a disclosure dialog rather than an ARIA `menu` on purpose: the panel
 * holds plain buttons, so Tab order, Enter, and Escape all work without a
 * roving-tabindex implementation — the same contract `ZoomControl` already
 * establishes for the popover in this cluster (Escape returns focus to the
 * trigger; an outside click closes without stealing focus).
 *
 * Entries read their labels and icons from `probeContext` / `probeIcons`
 * rather than restating them, so the title bar, the palette, and the
 * inspector header cannot drift into three names for one destination
 * (ADR-0030 "one name per destination").
 */

const OVERFLOW_ID = dropdownId('titlebar', 'project-overflow');

/** Ordered here, not derived: the overflow's order is a task order
 *  (configure, then maintain), and `PROBE_TAB_ORDER` is presentation order
 *  for the inspector's chip list. */
const ENTRIES: { tab: ProbeTab; description: string }[] = [
  { tab: 'properties', description: 'Identity, agents, commands, worktrees' },
  { tab: 'worktrees', description: 'Health, recovery, cleanup' },
];

/** Lucide `ellipsis` — the conventional overflow affordance. */
function MoreIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="currentColor"
      aria-hidden
    >
      <circle cx="12" cy="12" r="1.75" />
      <circle cx="5" cy="12" r="1.75" />
      <circle cx="19" cy="12" r="1.75" />
    </svg>
  );
}

export function TitleBarOverflow() {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const openProbeTab = useUIStore((s) => s.openProbeTab);
  const probeOpen = useUIStore((s) => s.probeOpen);
  const probeTab = useUIStore((s) => s.probeTab);

  const closeAndReturnFocus = useCallback(() => {
    const trigger = triggerRef.current;
    setOpen(false);
    requestAnimationFrame(() => trigger?.focus());
  }, []);

  useClickOutside(open ? OVERFLOW_ID : null, () => setOpen(false));
  useEscapeKey(closeAndReturnFocus, open);

  // Both entries open an inspector destination, so the trigger's tooltip
  // names whichever one is already in the inspector — the same "entry
  // point, not readout" contract `UsageButton` follows. The `active` styling
  // itself tracks the disclosure, not the destination.
  const activeLabel = ENTRIES.find((e) => e.tab === probeTab)?.tab;

  return (
    <div className="relative" data-dropdown-for={open ? OVERFLOW_ID : undefined}>
      <HeaderPillButton
        buttonRef={triggerRef}
        testId="titlebar-overflow"
        ariaLabel="More project actions"
        ariaExpanded={open}
        ariaHasPopup="dialog"
        ariaControls={open ? OVERFLOW_ID : undefined}
        onClick={() => setOpen((value) => !value)}
        title={
          probeOpen && activeLabel
            ? `${PROBE_TAB_DEFINITIONS[activeLabel].label} is open in the inspector`
            : 'Project settings and repository maintenance'
        }
        label="More"
        active={open}
        icon={<MoreIcon className="h-4 w-4 shrink-0" />}
      />
      {open && (
        <div
          id={OVERFLOW_ID}
          role="dialog"
          aria-label="Project actions"
          data-testid="titlebar-overflow-panel"
          className="absolute right-0 top-full z-50 mt-1 w-60 rounded-md border border-border-default bg-bg-card p-1 shadow-md animate-scale-in origin-top-right"
        >
          {ENTRIES.map((entry) => {
            const definition = PROBE_TAB_DEFINITIONS[entry.tab];
            const Icon = PROBE_TAB_ICONS[entry.tab];
            const isActive = probeOpen && probeTab === entry.tab;
            return (
              <button
                key={entry.tab}
                type="button"
                onClick={() => {
                  openProbeTab(entry.tab);
                  closeAndReturnFocus();
                }}
                data-testid={`titlebar-overflow-${entry.tab}`}
                className="flex w-full items-start gap-2 rounded-md px-2 py-1.5 text-left transition-colors hover:bg-bg-overlay"
              >
                <Icon className="mt-0.5 h-4 w-4 shrink-0 text-text-muted" />
                <span className="min-w-0">
                  <span
                    className={`block text-xs font-medium ${
                      isActive ? 'text-accent-cyan' : 'text-text-primary'
                    }`}
                  >
                    {definition.label}
                  </span>
                  <span className="block text-2xs text-text-muted break-words">
                    {entry.description}
                  </span>
                </span>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
