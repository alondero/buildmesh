import { create } from 'zustand';
import { equalSizes } from '../hooks/useGridLayout';

/** Pane sizes for one mesh's grid: column widths per row + one height per row. */
export interface GridLayout {
  colWidths: number[][];
  rowHeights: number[];
}

interface GridLayoutState {
  /** Remembered layout per mesh id, for this session only (resets on restart). */
  byMesh: Record<number, GridLayout>;
  setLayout: (meshId: number, layout: GridLayout) => void;
}

export const useGridLayoutStore = create<GridLayoutState>((set) => ({
  byMesh: {},
  setLayout: (meshId, layout) =>
    set((s) => ({ byMesh: { ...s.byMesh, [meshId]: layout } })),
}));

/**
 * Returns the stored sizes only if they still match the current grid shape;
 * otherwise equal sizes. A saved [3, 2]-shaped layout is meaningless once a
 * node is added or removed, so a row-count or per-row column-count mismatch
 * falls back to an even split.
 */
export function resolveLayout(stored: GridLayout | undefined, rowCounts: number[]): GridLayout {
  const valid =
    !!stored &&
    stored.rowHeights.length === rowCounts.length &&
    stored.colWidths.length === rowCounts.length &&
    stored.colWidths.every((row, i) => row.length === rowCounts[i]);

  return valid
    ? stored
    : { colWidths: rowCounts.map(equalSizes), rowHeights: equalSizes(rowCounts.length) };
}
