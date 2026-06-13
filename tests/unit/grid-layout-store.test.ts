import { describe, it, expect, beforeEach } from 'vitest';
import { useGridLayoutStore, resolveLayout, type GridLayout } from '../../src/stores/gridLayoutStore';

const layout32: GridLayout = {
  colWidths: [[40, 30, 30], [60, 40]],
  rowHeights: [55, 45],
};

describe('useGridLayoutStore', () => {
  beforeEach(() => {
    useGridLayoutStore.setState({ byMesh: {} });
  });

  it('stores a layout per mesh id', () => {
    useGridLayoutStore.getState().setLayout(7, layout32);
    expect(useGridLayoutStore.getState().byMesh[7]).toEqual(layout32);
  });

  it('keeps meshes independent', () => {
    useGridLayoutStore.getState().setLayout(1, layout32);
    useGridLayoutStore.getState().setLayout(2, { colWidths: [[100]], rowHeights: [100] });
    expect(useGridLayoutStore.getState().byMesh[1]).toEqual(layout32);
    expect(useGridLayoutStore.getState().byMesh[2]).toEqual({ colWidths: [[100]], rowHeights: [100] });
  });
});

describe('resolveLayout', () => {
  it('returns the stored layout when the shape matches', () => {
    expect(resolveLayout(layout32, [3, 2])).toBe(layout32);
  });

  it('falls back to equal sizes when stored is undefined', () => {
    expect(resolveLayout(undefined, [3, 2])).toEqual({
      colWidths: [[100 / 3, 100 / 3, 100 / 3], [50, 50]],
      rowHeights: [50, 50],
    });
  });

  it('falls back when the row count differs', () => {
    // stored has 2 rows, current grid has 3 rows
    expect(resolveLayout(layout32, [3, 2, 2])).toEqual({
      colWidths: [[100 / 3, 100 / 3, 100 / 3], [50, 50], [50, 50]],
      rowHeights: [100 / 3, 100 / 3, 100 / 3],
    });
  });

  it('falls back when a row column count differs', () => {
    // stored row 0 has 3 cols, current grid wants 2 in that row
    expect(resolveLayout(layout32, [2, 2])).toEqual({
      colWidths: [[50, 50], [50, 50]],
      rowHeights: [50, 50],
    });
  });
});
