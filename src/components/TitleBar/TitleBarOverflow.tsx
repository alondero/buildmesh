import { useCallback, useRef, useState } from 'react';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useEscapeKey } from '../../hooks/useEscapeKey';
import { useUIStore } from '../../stores/uiStore';
import { HeaderPillButton } from './HeaderPillButton';
import { PROBE_TAB_ICONS, UsageIcon } from '../Probe/probeIcons';
import { MobileIcon, SettingsIcon, ZoomIcon } from './utilityIcons';
import { TerminalTextSize } from './ZoomControl';
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

// Usage and Notifications already have dedicated title-bar buttons (Usage
// joins the utility rows below when its pill is folded).
const ENTRIES = TOOL_DISCOVERY_TILES.filter(entry => entry.tab !== 'usage' && entry.tab !== 'notifications');
const USAGE_TILE = TOOL_DISCOVERY_TILES.find(entry => entry.tab === 'usage');

const ROW_CLASS = 'flex w-full items-start gap-2 rounded-md px-2 py-1.5 text-left transition-colors hover:bg-bg-overlay';

/** One panel row: glyph, title, one-line description — the palette tile
    shape the tool entries already use. */
function RowText({ title, description, active = false }: { title: string; description: string; active?: boolean }) {
  return (
    <span className="min-w-0">
      <span className={`block text-xs font-medium ${active ? 'text-accent-cyan' : 'text-text-primary'}`}>
        {title}
      </span>
      <span className="block text-2xs text-text-muted break-words">{description}</span>
    </span>
  );
}

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

/**
 * `includeUtilities` — below `UTILITIES_IN_BAR_MIN_WIDTH` the title bar folds
 * its Usage, Zoom, Settings and Mobile pills into this panel (#2421): progressive
 * disclosure that keeps each one's name and description, rather than the
 * icon-only row the bar degraded to before.
 */
export function TitleBarOverflow({ includeUtilities = false }: { includeUtilities?: boolean }) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const openProbeTab = useUIStore((s) => s.openProbeTab);
  const probeOpen = useUIStore((s) => s.probeOpen);
  const probeTab = useUIStore((s) => s.probeTab);
  const usageOpen = probeOpen && probeTab === 'usage';

  const closeAndReturnFocus = useCallback(() => {
    const trigger = triggerRef.current;
    setOpen(false);
    requestAnimationFrame(() => trigger?.focus());
  }, []);

  useClickOutside(open ? OVERFLOW_ID : null, () => setOpen(false));
  useEscapeKey(closeAndReturnFocus, open);

  // Active styling tracks the disclosure; the tooltip names the open tool.
  // Usage has its own bar button unless it is folded into this panel below
  // 1700px, so it is named here only in the folded case.
  const activeEntry = (includeUtilities && probeTab === 'usage' ? USAGE_TILE : undefined) ?? ENTRIES.find((entry) => entry.tab === probeTab);

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
          {includeUtilities && (
            <>
              <button
                type="button"
                onClick={() => {
                  openProbeTab('usage');
                  closeAndReturnFocus();
                }}
                aria-current={usageOpen ? 'true' : undefined}
                data-testid="titlebar-overflow-usage"
                className={ROW_CLASS}
              >
                <UsageIcon className="mt-0.5 h-4 w-4 shrink-0 text-text-muted" />
                <RowText title={USAGE_TILE?.title ?? 'Usage'} description={USAGE_TILE?.description ?? ''} active={usageOpen} />
              </button>
              <div className="flex items-start gap-2 px-2 py-1.5" data-testid="titlebar-overflow-zoom">
                <ZoomIcon className="mt-0.5 h-4 w-4 shrink-0 text-text-muted" />
                <div className="min-w-0 flex-1">
                  {/* Out of the Zoom popover's context, the row names what it sizes. */}
                  <TerminalTextSize title="Terminal text size" />
                </div>
              </div>
              {/* These rows open a modal that returns focus to the trigger on
                  close; see the comment on the Settings row. */}
              <button
                type="button"
                onClick={() => {
                  // The shared Modal records focus at mount and restores it on
                  // close. Focus the trigger first, because the row is about to
                  // unmount and would otherwise leave the body focused (#2421 review).
                  triggerRef.current?.focus();
                  useUIStore.getState().openAppSettings();
                  setOpen(false);
                }}
                data-testid="titlebar-overflow-settings"
                className={ROW_CLASS}
              >
                <SettingsIcon className="mt-0.5 h-4 w-4 shrink-0 text-text-muted" />
                <RowText title="Settings" description="Buildmesh-wide defaults" />
              </button>
              <button
                type="button"
                onClick={() => {
                  triggerRef.current?.focus();
                  useUIStore.getState().openRemoteAccess();
                  setOpen(false);
                }}
                data-testid="titlebar-overflow-remote-access"
                className={ROW_CLASS}
              >
                <MobileIcon className="mt-0.5 h-4 w-4 shrink-0 text-text-muted" />
                <RowText title="Mobile remote access" description="Connect your phone to this workspace" />
              </button>
              <div className="mx-2 my-1 h-px bg-border-subtle" />
            </>
          )}
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
                aria-current={isActive ? 'true' : undefined}
                className={ROW_CLASS}
              >
                <Icon className="mt-0.5 h-4 w-4 shrink-0 text-text-muted" />
                <RowText title={entry.title} description={entry.description} active={isActive} />
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
