import { describe, expect, it } from 'vitest';
import {
  TOOL_DISCOVERY_GROUPS,
  TOOL_DISCOVERY_ROW_LAYOUT,
  TOOL_DISCOVERY_TILES,
  toolDiscoveryArrowTarget,
  type ToolDiscoveryArrowDirection,
} from '../../src/components/CommandOmnibar/toolDiscovery';
import type { ProbeTab } from '../../src/stores/uiStore';

describe('tool discovery grid traversal', () => {
  const indexOf = (tab: ProbeTab) =>
    TOOL_DISCOVERY_TILES.findIndex((tile) => tile.tab === tab);
  const cases: Array<[
    ProbeTab,
    ToolDiscoveryArrowDirection,
    ProbeTab,
    string,
  ]> = [
    ['files', 'right', 'review', 'moves right within a row'],
    ['review', 'left', 'files', 'moves left within a row'],
    ['files', 'left', 'files', 'stops at the left edge of a row'],
    ['review', 'right', 'review', 'stops at the right edge of a row'],
    ['properties', 'down', 'issues', 'preserves the column across a group heading'],
    ['worktrees', 'down', 'pulls', 'preserves the column from the other Project tile'],
    ['pulls', 'down', 'circuits', 'preserves the column across another group heading'],
    ['scratchpad', 'down', 'usage', 'clamps into the one-tile final row'],
    ['usage', 'down', 'files', 'wraps from the last row to the first'],
    ['files', 'up', 'usage', 'wraps from the first row to the last'],
  ];

  it('matches the rendered per-group two-column rows', () => {
    // Issue #1460 split "Code" into Code (files, review) + Project
    // (properties, worktrees). Both groups are two tiles, so the per-group
    // row layout is unchanged even though the grouping moved.
    expect(TOOL_DISCOVERY_ROW_LAYOUT).toEqual([2, 2, 2, 2, 2, 1]);
  });

  it('groups the two project destinations together, not with code browsing', () => {
    // The palette's start screen is where the pre-split IA was most visible:
    // Project Settings sat beside Files and Agent Changes. Configuration and
    // maintenance now share a "Project" heading (issue #1460).
    const project = TOOL_DISCOVERY_GROUPS.find((g) => g.id === 'project');
    expect(project?.tiles.map((t) => t.tab)).toEqual(['properties', 'worktrees']);
    const code = TOOL_DISCOVERY_GROUPS.find((g) => g.id === 'code');
    expect(code?.tiles.map((t) => t.tab)).toEqual(['files', 'review']);
  });

  it('labels the two destinations with their user-facing names', () => {
    // The Repository destination was renamed from "Worktree Manager"
    // (issue #1460); the tile title comes from probeContext, so the palette,
    // the title bar, and the inspector header cannot drift apart.
    const tiles = TOOL_DISCOVERY_TILES;
    expect(tiles.find((t) => t.tab === 'properties')?.title).toBe('Project Settings');
    expect(tiles.find((t) => t.tab === 'worktrees')?.title).toBe('Repository');
  });

  it.each(cases)('%s %s -> %s (%s)', (from, direction, to) => {
    const targetIndex = toolDiscoveryArrowTarget(indexOf(from), direction);
    expect(TOOL_DISCOVERY_TILES[targetIndex].tab).toBe(to);
  });

  it('recovers an invalid index at the first tile', () => {
    expect(toolDiscoveryArrowTarget(99, 'down')).toBe(0);
  });
});
