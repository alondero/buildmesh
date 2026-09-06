import { describe, expect, it } from 'vitest';
import {
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
    ['properties', 'down', 'pulls', 'preserves the column across a group heading'],
    ['pulls', 'down', 'circuits', 'preserves the column across another group heading'],
    ['scratchpad', 'down', 'usage', 'clamps into the one-tile final row'],
    ['usage', 'down', 'files', 'wraps from the last row to the first'],
    ['files', 'up', 'usage', 'wraps from the first row to the last'],
  ];

  it('matches the rendered per-group two-column rows', () => {
    expect(TOOL_DISCOVERY_ROW_LAYOUT).toEqual([2, 2, 2, 2, 2, 1]);
  });

  it.each(cases)('%s %s -> %s (%s)', (from, direction, to) => {
    const targetIndex = toolDiscoveryArrowTarget(indexOf(from), direction);
    expect(TOOL_DISCOVERY_TILES[targetIndex].tab).toBe(to);
  });

  it('recovers an invalid index at the first tile', () => {
    expect(toolDiscoveryArrowTarget(99, 'down')).toBe(0);
  });
});
