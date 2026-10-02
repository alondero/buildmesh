import { useCallback, useRef, useState } from 'react';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useEscapeKey } from '../../hooks/useEscapeKey';
import { useUIStore } from '../../stores/uiStore';
import { HeaderPillButton } from './HeaderPillButton';
import { PROBE_TAB_ICONS } from '../Probe/probeIcons';
import { TOOL_DISCOVERY_TILES } from '../CommandOmnibar/toolDiscovery';
import { dropdownId } from '../../lib/dropdownId';

/**
 * Title-bar overflow for inspector tools without a dedicated title-bar button.
 * Shares the palette's tool order, labels, descriptions, and inspector icons.
 *
 * It is a disclosure dialog rather than an ARIA `menu` on purpose: the panel
 * holds plain buttons, so Tab order, Enter, and Escape all work without a
 * roving-tabindex implementation — the same contract `ZoomControl` already
 * establishes for the popover in this cluster (Escape returns focus to the
 * trigger; an outside click closes without stealing focus).
 */

const OVERFLOW_ID = dropdownId('titlebar', 'tools-overflow');

// Usage already has a dedicated title-bar button.
const ENTRIES = TOOL_DISCOVERY_TILES.filter((entry) => entry.tab !== 'usage');

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

  // Active styling tracks the disclosure; the tooltip names the open tool.
  const activeEntry = ENTRIES.find((entry) => entry.tab === probeTab);

  return (
    <div className="relative" data-dropdown-for={open ? OVERFLOW_ID : undefined}>
      <HeaderPillButton
        buttonRef={triggerRef}
        testId="titlebar-overflow"
        ariaLabel="More tools"
        ariaExpanded={open}
        ariaHasPopup="dialog"
        ariaControls={open ? OVERFLOW_ID : undefined}
        onClick={() => setOpen((value) => !value)}
        title={
          probeOpen && activeEntry
            ? `${activeEntry.title} is open in the inspector`
            : 'Open more tools'
        }
        label="More"
        active={open}
        icon={<MoreIcon className="h-4 w-4 shrink-0" />}
      />
      {open && (
        <div
          id={OVERFLOW_ID}
          role="dialog"
          aria-label="Tools"
          data-testid="titlebar-overflow-panel"
          className="absolute right-0 top-full z-50 mt-1 max-h-[calc(100dvh-4rem)] w-72 overflow-y-auto rounded-md border border-border-default bg-bg-card p-1 shadow-md animate-scale-in origin-top-right"
        >
          {ENTRIES.map((entry) => {
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
                    {entry.title}
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
