/**
 * ProbeToolRail — the working-set tab strip at the top of the Probe panel
 * (ADR-0032).
 *
 * ADR-0030 moved Probe navigation to the title bar and palette and left the
 * open panel with no in-panel switcher, so alternating between two or three
 * destinations meant a round trip through the palette every time. This rail
 * restores fast switching *inside* the open panel without reviving the
 * always-visible activity rail ADR-0030 rejected: it renders only while the
 * panel is open (closed-render discipline is untouched), and it shows only
 * the destinations actually visited this session (`probeWorkingSet`, capped
 * at `PROBE_WORKING_SET_CAP`) instead of all eleven.
 *
 * **Display order is spatially stable.** The strip renders the working set
 * in insertion order; activation never reorders the DOM. WAI-ARIA arrow-key
 * navigation therefore walks real positions — every destination in the set
 * is reachable with ArrowRight/ArrowLeft, Home/End jump to the row ends.
 * (An earlier draft rendered MRU order and reordered on activation, which
 * trapped every tab past index 1 behind an ArrowRight ping-pong and made
 * ArrowLeft dead code; recency now drives eviction only.)
 *
 * The trailing ⊞ button follows the WAI-ARIA menu-button contract: click,
 * ArrowDown, or ArrowUp open the menu (focusing the first/last tile
 * respectively), and it closes on Escape, focusout, or mousedown outside.
 * The menu reuses the palette's tool-discovery groups (ADR-0031) — the same
 * tiles, labels, and scope notes, so "tool grid" means one thing across the
 * omnibar and the inspector. The glyph is deliberately `layout-grid`, not
 * "+": the action browses existing tools, it doesn't create one.
 *
 * Narrow widths: below 320px panel width the tab labels collapse to icons
 * (names remain in `title`/`aria-label`), and the menu drops to a single
 * column — a 2-column grid truncates labels to noise at the dock's 240px
 * minimum (`probe-ui-checklist.md` §2: 240px is the case to design for).
 */

import { useEffect, useRef, useState } from 'react';
import type { KeyboardEvent as ReactKeyboardEvent, FocusEvent as ReactFocusEvent } from 'react';
import { useUIStore } from '../../stores/uiStore';
import type { ProbeTab } from '../../lib/probeContext';
import { PROBE_TAB_DEFINITIONS } from '../../lib/probeContext';
import {
  TOOL_DISCOVERY_GROUPS,
  TOOL_DISCOVERY_TILES,
  toolDiscoveryArrowTarget,
} from '../CommandOmnibar/toolDiscovery';
import { LayoutGridIcon, PROBE_TAB_ICONS } from './probeIcons';

/** Panel width at which rail tab labels collapse to icons. The dock
 *  resizes 240–720 (`PROBE_PANEL_BOUNDS`); below 320 four labeled tabs
 *  would truncate to noise, while icon-only tabs fit comfortably.
 *  ProbePanel computes `narrow` from the live body width. */
export const LABEL_COLLAPSE_WIDTH = 320;

const tabId = (tab: ProbeTab) => `probe-rail-tab-${tab}`;
const menuTileId = (tab: ProbeTab) => `probe-rail-menu-${tab}`;

type ArrowDir = 'up' | 'down' | 'left' | 'right';

const MENU_KEYS: Record<string, ArrowDir | 'start' | 'end'> = {
  ArrowDown: 'down',
  ArrowUp: 'up',
  ArrowLeft: 'left',
  ArrowRight: 'right',
  Home: 'start',
  End: 'end',
};

export function ProbeToolRail({ narrow }: { narrow: boolean }) {
  const probeTab = useUIStore((s) => s.probeTab);
  const workingSet = useUIStore((s) => s.probeWorkingSet);
  const openProbeTab = useUIStore((s) => s.openProbeTab);
  const tabs = workingSet.tabs;
  const [menuOpen, setMenuOpen] = useState(false);
  const [menuFocusTab, setMenuFocusTab] = useState<ProbeTab>(
    TOOL_DISCOVERY_TILES[0].tab,
  );
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);

  // Menu dismissal. Escape is handled on the menu itself; focusout closing
  // covers Tab away from the menu (the WAI-ARIA menu-button contract — an
  // orphaned floating menu must not outlive its trigger's focus scope), and
  // mousedown-outside covers clicks on non-focusable surfaces, which move
  // no focus at all.
  useEffect(() => {
    if (!menuOpen) return;
    const onDocMouseDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (menuRef.current?.contains(target)) return;
      if (triggerRef.current?.contains(target)) return;
      setMenuOpen(false);
    };
    document.addEventListener('mousedown', onDocMouseDown);
    return () => document.removeEventListener('mousedown', onDocMouseDown);
  }, [menuOpen]);

  // On open, move DOM focus to the tile `openMenu` chose (active tile on
  // click/ArrowDown, last tile on ArrowUp) — resolved through a ref so the
  // effect needs no reactive dependencies beyond the open flag itself.
  const initialMenuFocusRef = useRef<ProbeTab>(TOOL_DISCOVERY_TILES[0].tab);
  useEffect(() => {
    if (!menuOpen) return;
    menuRef.current
      ?.querySelector<HTMLElement>(`[id="probe-rail-menu-${initialMenuFocusRef.current}"]`)
      ?.focus();
  }, [menuOpen]);

  const onRailBlur = (e: ReactFocusEvent<HTMLDivElement>) => {
    if (!menuOpen) return;
    // Focus left the rail entirely (Tab, focus() call elsewhere): close the
    // floating menu so it can't outlive its trigger. `relatedTarget` is null
    // when focus moves to non-node targets — treat that as outside too.
    if (e.currentTarget.contains(e.relatedTarget as Node | null)) return;
    setMenuOpen(false);
  };

  const focusMenuTile = (tab: ProbeTab) => {
    menuRef.current
      ?.querySelector<HTMLElement>(`[id="${menuTileId(tab)}"]`)
      ?.focus();
  };

  const openMenu = (initial: 'first' | 'last') => {
    const initialTab = initial === 'last'
      ? TOOL_DISCOVERY_TILES[TOOL_DISCOVERY_TILES.length - 1].tab
      : TOOL_DISCOVERY_TILES.some((t) => t.tab === probeTab)
        ? probeTab
        : TOOL_DISCOVERY_TILES[0].tab;
    initialMenuFocusRef.current = initialTab;
    setMenuFocusTab(initialTab);
    setMenuOpen(true);
  };

  // WAI-ARIA menu button: ArrowDown opens (first item), ArrowUp opens
  // (last item). While open, they hand focus straight to the end tiles.
  const onTriggerKeyDown = (e: ReactKeyboardEvent<HTMLButtonElement>) => {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    e.preventDefault();
    const end = e.key === 'ArrowUp' ? 'last' : 'first';
    if (menuOpen) {
      focusMenuTile(end === 'last'
        ? TOOL_DISCOVERY_TILES[TOOL_DISCOVERY_TILES.length - 1].tab
        : TOOL_DISCOVERY_TILES[0].tab);
      return;
    }
    openMenu(end);
  };

  // Tabs pattern with automatic activation over the strip's stable display
  // order: arrows move focus AND switch the destination in one step, so all
  // working-set entries are reachable in both directions.
  const onTablistKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (tabs.length === 0) return;
    const current = Math.max(0, tabs.indexOf(probeTab));
    let next = -1;
    if (e.key === 'ArrowRight') next = Math.min(tabs.length - 1, current + 1);
    else if (e.key === 'ArrowLeft') next = Math.max(0, current - 1);
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = tabs.length - 1;
    if (next === -1) return;
    e.preventDefault();
    const nextTab = tabs[next];
    if (nextTab !== probeTab) openProbeTab(nextTab);
    // Activation reorders nothing — the focused element is stable, so a
    // scoped lookup (never a global document query) is enough.
    e.currentTarget
      .querySelector<HTMLElement>(`[id="${tabId(nextTab)}"]`)
      ?.focus();
  };

  const onMenuKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      setMenuOpen(false);
      triggerRef.current?.focus();
      return;
    }
    const action = MENU_KEYS[e.key];
    if (!action) return;
    e.preventDefault();
    const nextTab =
      action === 'start'
        ? TOOL_DISCOVERY_TILES[0].tab
        : action === 'end'
          ? TOOL_DISCOVERY_TILES[TOOL_DISCOVERY_TILES.length - 1].tab
          : TOOL_DISCOVERY_TILES[
              toolDiscoveryArrowTarget(
                Math.max(0, TOOL_DISCOVERY_TILES.findIndex((t) => t.tab === menuFocusTab)),
                action,
              )
            ].tab;
    // Menus use manual activation: arrows move focus only, Enter/click
    // switches — same contract as the omnibar's tool grid.
    setMenuFocusTab(nextTab);
    focusMenuTile(nextTab);
  };

  const selectFromMenu = (tab: ProbeTab) => {
    openProbeTab(tab);
    setMenuOpen(false);
    triggerRef.current?.focus();
  };

  return (
    <div
      className="relative shrink-0 border-b border-border-subtle"
      data-testid="probe-tool-rail"
      onBlur={onRailBlur}
    >
      <div className="flex items-center gap-1 px-1.5 py-1">
        <div
          role="tablist"
          aria-label="Probe destinations"
          className="flex items-center gap-1 min-w-0 flex-1"
          onKeyDown={onTablistKeyDown}
        >
          {tabs.map((tab) => {
            const def = PROBE_TAB_DEFINITIONS[tab];
            const Icon = PROBE_TAB_ICONS[tab];
            const active = tab === probeTab;
            return (
              <button
                key={tab}
                type="button"
                id={tabId(tab)}
                role="tab"
                aria-selected={active}
                aria-controls="probe-tab-panel"
                aria-label={narrow ? def.label : undefined}
                tabIndex={active ? 0 : -1}
                title={def.label}
                data-testid={`probe-rail-tab-${tab}`}
                onClick={() => openProbeTab(tab)}
                className={`relative flex items-center gap-1.5 h-7 rounded-md text-xs font-medium transition-colors min-w-0 ${
                  narrow ? 'flex-none w-7 justify-center' : 'px-2'
                } ${
                  active
                    ? 'bg-bg-card text-text-primary after:absolute after:inset-x-1.5 after:bottom-0 after:h-0.5 after:rounded-full after:bg-accent-cyan'
                    : 'text-text-secondary hover:text-text-primary hover:bg-bg-card-hover'
                }`}
              >
                <Icon className="w-3.5 h-3.5 shrink-0" />
                {!narrow && (
                  <span className="truncate min-w-0">{def.label}</span>
                )}
              </button>
            );
          })}
        </div>
        <button
          ref={triggerRef}
          type="button"
          aria-haspopup="menu"
          aria-expanded={menuOpen}
          aria-controls="probe-tool-menu"
          aria-label="All tools"
          title="All tools"
          data-testid="probe-rail-all-tools"
          onClick={() => (menuOpen ? setMenuOpen(false) : openMenu('first'))}
          onKeyDown={onTriggerKeyDown}
          className={`p-1.5 rounded-md transition-colors shrink-0 ${
            menuOpen
              ? 'text-text-primary bg-bg-card-hover'
              : 'text-text-muted hover:text-text-primary hover:bg-bg-card-hover'
          }`}
        >
          <LayoutGridIcon className="w-4 h-4" />
        </button>
      </div>

      {menuOpen && (
        <div
          ref={menuRef}
          id="probe-tool-menu"
          role="menu"
          aria-label="All tools"
          data-testid="probe-tool-menu"
          onKeyDown={onMenuKeyDown}
          className="absolute left-2 right-2 top-full mt-1 z-20 max-h-72 overflow-y-auto overflow-x-hidden rounded-lg border border-border-default bg-bg-overlay shadow-md p-2 animate-scale-in"
        >
          {TOOL_DISCOVERY_GROUPS.map((group, groupIndex) => (
            <div
              key={group.id}
              role="group"
              aria-label={group.title}
              className={groupIndex === TOOL_DISCOVERY_GROUPS.length - 1 ? '' : 'mb-1.5'}
            >
              <div className="flex items-baseline gap-2 mt-1 mb-1">
                <span className="text-2xs font-semibold uppercase tracking-[0.14em] text-text-muted">
                  {group.title}
                </span>
                <span className="ml-auto text-right text-2xs text-text-muted">
                  {group.scopeNote}
                </span>
              </div>
              {/* Single column at narrow widths: a 2-column grid truncates
                  tile names to ~4 characters at the dock's 240px minimum. */}
              <div className={`grid gap-1 ${narrow ? 'grid-cols-1' : 'grid-cols-2'}`}>
                {group.tiles.map((tile) => {
                  const Icon = PROBE_TAB_ICONS[tile.tab];
                  const active = tile.tab === probeTab;
                  return (
                    <button
                      key={tile.tab}
                      type="button"
                      id={menuTileId(tile.tab)}
                      role="menuitemradio"
                      aria-checked={active}
                      tabIndex={tile.tab === menuFocusTab ? 0 : -1}
                      data-menu-tab={tile.tab}
                      data-testid={`probe-tool-menu-${tile.tab}`}
                      onClick={() => selectFromMenu(tile.tab)}
                      className={`flex items-start gap-2 p-1.5 rounded-md text-left border transition-colors ${
                        active
                          ? 'border-accent-cyan/25 bg-bg-highlight'
                          : 'border-transparent hover:bg-bg-card-hover'
                      }`}
                    >
                      <Icon
                        className={`w-4 h-4 mt-0.5 shrink-0 ${
                          active ? 'text-accent-cyan' : 'text-text-secondary'
                        }`}
                      />
                      <span className="min-w-0">
                        <span className="block text-xs font-medium text-text-primary truncate min-w-0">
                          {tile.title}
                        </span>
                        <span className="block text-2xs text-text-muted truncate min-w-0">
                          {tile.description}
                        </span>
                      </span>
                    </button>
                  );
                })}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
