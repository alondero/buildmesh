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
 * the destinations actually visited this session (`probeMru`, MRU order,
 * capped at `PROBE_WORKING_SET_CAP`) instead of all eleven.
 *
 * The trailing ⊞ button opens the full destination list as a grouped
 * menu that reuses the palette's tool-discovery groups (ADR-0031) — the
 * same tiles, labels, and scope notes, so "tool grid" means one thing
 * across the omnibar and the inspector. The glyph is deliberately
 * `layout-grid`, not "+": the action browses existing tools, it doesn't
 * create one.
 *
 * Keyboard: the strip follows the WAI-ARIA tabs pattern (roving tabindex,
 * Arrow/Home/End, activation follows focus); the menu follows the menu
 * pattern (arrow keys move focus through ADR-0031's virtual 2-column grid
 * via `toolDiscoveryArrowTarget`, Enter/click activates, Esc closes and
 * restores focus to the trigger). At panel widths below 320px the tab
 * labels collapse to icons — the full names remain in `title` and
 * `aria-label`, and the narrow case is the one `probe-ui-checklist.md`
 * says to design for.
 */

import { useEffect, useRef, useState } from 'react';
import type { KeyboardEvent as ReactKeyboardEvent } from 'react';
import { useUIStore } from '../../stores/uiStore';
import type { ProbeTab } from '../../stores/uiStore';
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
  const probeMru = useUIStore((s) => s.probeMru);
  const openProbeTab = useUIStore((s) => s.openProbeTab);
  const [menuOpen, setMenuOpen] = useState(false);
  const [menuFocusTab, setMenuFocusTab] = useState<ProbeTab>(
    TOOL_DISCOVERY_TILES[0].tab,
  );
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);

  // Menu dismissal — Esc is handled on the menu itself; a mousedown that
  // lands outside both the menu and its trigger closes it. (Mousedown, not
  // click: matching native menu behavior so the press that starts inside
  // the menu but ends outside doesn't double-fire a tile activation.)
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

  // On open, park roving focus on the active destination's tile (or the
  // first tile when the active tab isn't in the grid — cannot happen while
  // the tab union is stable, but the fallback keeps focus valid).
  useEffect(() => {
    if (!menuOpen) return;
    const initial = TOOL_DISCOVERY_TILES.some((t) => t.tab === probeTab)
      ? probeTab
      : TOOL_DISCOVERY_TILES[0].tab;
    setMenuFocusTab(initial);
    // Deliberately keyed to `menuOpen` only: this is "where focus starts",
    // not "follow the active tab" — arrow keys own focus from here.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [menuOpen]);

  // Roving tabindex needs real DOM focus to follow `menuFocusTab`.
  useEffect(() => {
    if (!menuOpen) return;
    menuRef.current
      ?.querySelector<HTMLElement>(`[data-menu-tab="${menuFocusTab}"]`)
      ?.focus();
  }, [menuOpen, menuFocusTab]);

  const onTablistKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (probeMru.length === 0) return;
    const current = Math.max(0, probeMru.indexOf(probeTab));
    let next = -1;
    if (e.key === 'ArrowRight') next = Math.min(probeMru.length - 1, current + 1);
    else if (e.key === 'ArrowLeft') next = Math.max(0, current - 1);
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = probeMru.length - 1;
    if (next === -1) return;
    e.preventDefault();
    // Tabs pattern with automatic activation: arrows move focus and switch
    // the destination in one step, matching the omnibar's keyboard rhythm.
    const nextTab = probeMru[next];
    if (nextTab !== probeTab) openProbeTab(nextTab);
    document.getElementById(tabId(nextTab))?.focus();
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
    >
      <div className="flex items-center gap-1 px-1.5 py-1">
        <div
          role="tablist"
          aria-label="Probe destinations"
          className="flex items-center gap-1 min-w-0 flex-1"
          onKeyDown={onTablistKeyDown}
        >
          {probeMru.map((tab) => {
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
          aria-label="All tools"
          title="All tools"
          data-testid="probe-rail-all-tools"
          onClick={() => setMenuOpen((v) => !v)}
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
              <div className="grid grid-cols-2 gap-1">
                {group.tiles.map((tile) => {
                  const Icon = PROBE_TAB_ICONS[tile.tab];
                  const active = tile.tab === probeTab;
                  return (
                    <button
                      key={tile.tab}
                      type="button"
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
