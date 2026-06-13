import { describe, it, expect } from 'vitest';
import { getGridRows, equalSizes } from '../../src/hooks/useGridLayout';

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
