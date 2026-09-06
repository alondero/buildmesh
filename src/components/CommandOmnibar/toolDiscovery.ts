/**
 * Tool discovery start-screen config (Option A, ADR-0031).
 *
 * Presentation-layer companion to the command palette: which probe
 * destinations appear on first open, in which task groups, with what copy.
 * This module owns no search behavior — `../../lib/omnibar/indexers.ts`
 * owns the fuzzy-search catalog (`PROBE_DESTINATION_COMMANDS`); this
 * module only reads its labels so tile copy and search-row copy share one
 * source and cannot drift.
 *
 * Static by design: groups and tiles are module constants resolved once at
 * load, so the palette render pass allocates nothing here.
 */
import type { ProbeTab } from '../../stores/uiStore';
import { PROBE_TAB_DEFINITIONS } from '../../lib/probeContext';
import { PROBE_DESTINATION_COMMANDS } from '../../lib/omnibar/indexers';

export interface ToolDiscoveryTile {
  tab: ProbeTab;
  title: string;
  description: string;
}

export interface ToolDiscoveryGroup {
  /** Stable id, also used for the `command-omnibar-tool-group-<id>` test id. */
  id: string;
  /** User-facing group heading. */
  title: string;
  /** Scope line beside the heading — answers "where does this act?" per group. */
  scopeNote: string;
  tiles: readonly ToolDiscoveryTile[];
}

/** DOM/test id for a tile (shared by the palette and its tests). */
export function toolTileId(tab: ProbeTab): string {
  return `command-omnibar-tool-${tab}`;
}

const CODE_TABS = ['files', 'review', 'worktrees', 'properties'] as const;
const GITHUB_TABS = ['issues', 'pulls'] as const;
const AUTOMATE_TABS = ['autopilot', 'circuits'] as const;
const REMEMBER_TABS = ['sessions', 'scratchpad'] as const;
const APP_TABS = ['usage'] as const;

type GroupedTab =
  | (typeof CODE_TABS)[number]
  | (typeof GITHUB_TABS)[number]
  | (typeof AUTOMATE_TABS)[number]
  | (typeof REMEMBER_TABS)[number]
  | (typeof APP_TABS)[number];

/** Single type-level exhaustiveness check: adding a `ProbeTab` fails the
 *  build until it is placed in a group, and a mistyped tab fails too. */
type _AssertDiscoveryExhaustive =
  Exclude<ProbeTab, GroupedTab> extends never
    ? Exclude<GroupedTab, ProbeTab> extends never
      ? unknown
      : ['unknown tab in discovery groups', Exclude<GroupedTab, ProbeTab>]
    : ['probe tab missing from discovery groups', Exclude<ProbeTab, GroupedTab>];

function tile(tab: ProbeTab): ToolDiscoveryTile {
  const cmd = PROBE_DESTINATION_COMMANDS[`probe-${tab}`];
  return {
    tab,
    title: PROBE_TAB_DEFINITIONS[tab].label,
    description: cmd.subtitle ?? '',
  };
}

export const TOOL_DISCOVERY_GROUPS: readonly ToolDiscoveryGroup[] & _AssertDiscoveryExhaustive = [
  {
    id: 'code',
    title: 'Code',
    scopeNote: 'Selected project · Agent Changes follows the focused agent',
    tiles: CODE_TABS.map(tile),
  },
  {
    id: 'github',
    title: 'GitHub',
    scopeNote: 'Selected project',
    tiles: GITHUB_TABS.map(tile),
  },
  {
    id: 'automate',
    title: 'Automate',
    scopeNote: 'Selected project',
    tiles: AUTOMATE_TABS.map(tile),
  },
  {
    id: 'remember',
    title: 'Remember',
    scopeNote: 'Selected project',
    tiles: REMEMBER_TABS.map(tile),
  },
  {
    id: 'app',
    title: 'App-wide',
    scopeNote: 'App-wide — ignores project selection',
    tiles: APP_TABS.map(tile),
  },
];

/** Flat tile order for DOM ids and the virtual active index. */
export const TOOL_DISCOVERY_TILES: readonly ToolDiscoveryTile[] =
  TOOL_DISCOVERY_GROUPS.flatMap((group) => group.tiles);

export const TOOL_DISCOVERY_GRID_COLUMNS = 2;

/**
 * The DOM renders one grid per group. Arrow navigation treats those rows as
 * one virtual grid, so vertical movement crosses group headings while keeping
 * the same column. These rows are the actual per-group grid shape.
 */
export const TOOL_DISCOVERY_ROW_LAYOUT: readonly number[] =
  TOOL_DISCOVERY_GROUPS.flatMap((group) => {
    const rows: number[] = [];
    for (
      let start = 0;
      start < group.tiles.length;
      start += TOOL_DISCOVERY_GRID_COLUMNS
    ) {
      rows.push(
        Math.min(TOOL_DISCOVERY_GRID_COLUMNS, group.tiles.length - start),
      );
    }
    return rows;
  });

export type ToolDiscoveryArrowDirection = 'up' | 'down' | 'left' | 'right';

/**
 * Return the tile reached by an arrow in the palette's virtual grid. The DOM
 * has one grid per group, but vertical movement crosses the group headings,
 * wraps between the first and last rows, and clamps to the target row's
 * available width.
 */
export function toolDiscoveryArrowTarget(
  currentIndex: number,
  direction: ToolDiscoveryArrowDirection,
): number {
  if (currentIndex < 0 || currentIndex >= TOOL_DISCOVERY_TILES.length) {
    return 0;
  }

  let rowStart = 0;
  let row = -1;
  let column = -1;
  for (
    let rowIndex = 0;
    rowIndex < TOOL_DISCOVERY_ROW_LAYOUT.length;
    rowIndex += 1
  ) {
    const rowLength = TOOL_DISCOVERY_ROW_LAYOUT[rowIndex];
    if (currentIndex < rowStart + rowLength) {
      row = rowIndex;
      column = currentIndex - rowStart;
      break;
    }
    rowStart += rowLength;
  }

  if (row === -1) return currentIndex;

  if (direction === 'left') {
    return column > 0 ? currentIndex - 1 : currentIndex;
  }

  if (direction === 'right') {
    return column < TOOL_DISCOVERY_ROW_LAYOUT[row] - 1
      ? currentIndex + 1
      : currentIndex;
  }

  const rowDelta = direction === 'down' ? 1 : -1;
  const targetRowIndex =
    (row + rowDelta + TOOL_DISCOVERY_ROW_LAYOUT.length) %
    TOOL_DISCOVERY_ROW_LAYOUT.length;

  let targetRowStart = 0;
  for (let rowIndex = 0; rowIndex < targetRowIndex; rowIndex += 1) {
    targetRowStart += TOOL_DISCOVERY_ROW_LAYOUT[rowIndex];
  }

  const targetColumn = Math.min(
    column,
    TOOL_DISCOVERY_ROW_LAYOUT[targetRowIndex] - 1,
  );
  return targetRowStart + targetColumn;
}
