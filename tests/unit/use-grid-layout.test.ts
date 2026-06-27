import { describe, it, expect } from 'vitest';
import { getGridRows, equalSizes } from '../../src/hooks/useGridLayout';
import { arrowTargetIndex } from '../../src/lib/gridTraversal';

describe('getGridRows', () => {
  const cases: Array<[number, number[]]> = [
    [1, [1]],
    [2, [2]],
    [3, [3]],
    [4, [2, 2]],
    [5, [3, 2]],
    [6, [3, 3]],
    [7, [3, 2, 2]],
    [8, [3, 3, 2]],
    [9, [3, 3, 3]],
    [10, [3, 3, 2, 2]],
  ];

  it.each(cases)('balances %i nodes into %j', (count, expected) => {
    expect(getGridRows(count)).toEqual(expected);
  });

  it('returns an empty layout for zero or negative counts', () => {
    expect(getGridRows(0)).toEqual([]);
    expect(getGridRows(-3)).toEqual([]);
  });

  it('never exceeds 3 columns and always sums to the node count', () => {
    for (let count = 1; count <= 30; count++) {
      const rows = getGridRows(count);
      expect(Math.max(...rows)).toBeLessThanOrEqual(3);
      expect(rows.reduce((a, b) => a + b, 0)).toBe(count);
    }
  });
});

describe('equalSizes', () => {
  it('splits 100% evenly', () => {
    expect(equalSizes(4)).toEqual([25, 25, 25, 25]);
    expect(equalSizes(1)).toEqual([100]);
  });
});

describe('arrowTargetIndex', () => {
  // 1 node — ResizablePanes, single row of 1. Only one node; arrows are all no-ops.
  describe('single node', () => {
    const rows = [1];
    it('any arrow stays put', () => {
      for (const dir of ['left', 'right', 'up', 'down'] as const) {
        expect(arrowTargetIndex(0, dir, rows)).toBe(0);
      }
    });
  });

  // 2 nodes — ResizablePanes, single row of 2. Up/Down no-op; Left/Right swap.
  describe('two nodes in one row', () => {
    const rows = [2];
    it('Right from index 0 goes to index 1', () => {
      expect(arrowTargetIndex(0, 'right', rows)).toBe(1);
    });
    it('Right from index 1 wraps to index 0', () => {
      expect(arrowTargetIndex(1, 'right', rows)).toBe(0);
    });
    it('Left from index 1 goes to index 0', () => {
      expect(arrowTargetIndex(1, 'left', rows)).toBe(0);
    });
    it('Left from index 0 wraps within the row to index 1', () => {
      expect(arrowTargetIndex(0, 'left', rows)).toBe(1);
    });
    it('Up/Down are no-ops in a single row', () => {
      expect(arrowTargetIndex(0, 'up', rows)).toBe(0);
      expect(arrowTargetIndex(1, 'down', rows)).toBe(1);
    });
  });

  // 3 nodes — GridSplitter, rows [3]. One wide row; Left/Right wrap, Up/Down no-op.
  describe('three nodes in one row', () => {
    const rows = [3];
    it('Right moves one step forward within the row', () => {
      expect(arrowTargetIndex(0, 'right', rows)).toBe(1);
      expect(arrowTargetIndex(1, 'right', rows)).toBe(2);
    });
    it('Right from the last column wraps to the first', () => {
      expect(arrowTargetIndex(2, 'right', rows)).toBe(0);
    });
    it('Left from the first column wraps to the last', () => {
      expect(arrowTargetIndex(0, 'left', rows)).toBe(2);
    });
    it('Up/Down are no-ops with a single row', () => {
      expect(arrowTargetIndex(1, 'up', rows)).toBe(1);
      expect(arrowTargetIndex(1, 'down', rows)).toBe(1);
    });
  });

  // 4 nodes — GridSplitter, rows [2, 2]. True 2x2.
  describe('2x2 grid', () => {
    const rows = [2, 2];
    it('Right and Left stay within the current row', () => {
      expect(arrowTargetIndex(0, 'right', rows)).toBe(1);
      expect(arrowTargetIndex(1, 'left', rows)).toBe(0);
    });
    it('Down from row 0 col 0 lands on row 1 col 0', () => {
      expect(arrowTargetIndex(0, 'down', rows)).toBe(2);
    });
    it('Down from row 0 col 1 lands on row 1 col 1', () => {
      expect(arrowTargetIndex(1, 'down', rows)).toBe(3);
    });
    it('Up from row 1 col 0 lands on row 0 col 0', () => {
      expect(arrowTargetIndex(2, 'up', rows)).toBe(0);
    });
    it('Up/Down at the grid vertical edges are no-ops', () => {
      expect(arrowTargetIndex(0, 'up', rows)).toBe(0);
      expect(arrowTargetIndex(3, 'down', rows)).toBe(3);
    });
  });

  // 5 nodes — GridSplitter, rows [3, 2]. Top row 3 wide, bottom row 2 wide.
  describe('jagged grid [3, 2]', () => {
    const rows = [3, 2];
    it('Down from top-right (col 2) clamps to bottom-right (col 1)', () => {
      // The narrower bottom row only has 2 cells, so col 2 is clamped to col 1.
      expect(arrowTargetIndex(2, 'down', rows)).toBe(4);
    });
    it('Down from top-middle (col 1) lands directly below', () => {
      expect(arrowTargetIndex(1, 'down', rows)).toBe(4);
    });
    it('Down from top-left (col 0) lands on bottom-left', () => {
      expect(arrowTargetIndex(0, 'down', rows)).toBe(3);
    });
    it('Up from bottom-right (col 1) lands on top col 1', () => {
      expect(arrowTargetIndex(4, 'up', rows)).toBe(1);
    });
    it('Right from bottom-right wraps to bottom-left', () => {
      expect(arrowTargetIndex(4, 'right', rows)).toBe(3);
    });
    it('Right from bottom-left goes to bottom-right', () => {
      expect(arrowTargetIndex(3, 'right', rows)).toBe(4);
    });
  });

  // 6 nodes — GridSplitter, rows [3, 3]. Top row 3 wide, bottom row 3 wide.
  describe('rectangular 3x2 grid', () => {
    const rows = [3, 3];
    it('Down from top-right lands on bottom-right', () => {
      expect(arrowTargetIndex(2, 'down', rows)).toBe(5);
    });
    it('Left from top-left wraps to top-right', () => {
      expect(arrowTargetIndex(0, 'left', rows)).toBe(2);
    });
    it('Up from bottom-left lands on top-left', () => {
      expect(arrowTargetIndex(3, 'up', rows)).toBe(0);
    });
  });

  it('returns the same index for an empty layout', () => {
    expect(arrowTargetIndex(0, 'right', [])).toBe(0);
    expect(arrowTargetIndex(5, 'down', [])).toBe(5);
  });
});
