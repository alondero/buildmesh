/**
 * Computes grid dimensions based on the number of nodes to display.
 */
export function getGridLayout(nodeCount: number): { cols: number; rows: number } {
  if (nodeCount <= 1) return { cols: 1, rows: 1 };
  if (nodeCount === 2) return { cols: 2, rows: 1 };
  if (nodeCount <= 4) return { cols: 2, rows: 2 };
  if (nodeCount <= 6) return { cols: 3, rows: 2 };
  return { cols: 3, rows: Math.ceil(nodeCount / 3) };
}

export function equalSizes(n: number): number[] {
  return Array.from({ length: n }, () => 100 / n);
}
