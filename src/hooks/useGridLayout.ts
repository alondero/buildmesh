/**
 * Computes CSS grid class names based on the number of nodes to display.
 */
export function useGridLayout(nodeCount: number): { columns: string; rows: string } {
  if (nodeCount === 2) {
    return { columns: 'grid-cols-2', rows: 'grid-rows-1' };
  } else if (nodeCount === 3 || nodeCount === 4) {
    return { columns: 'grid-cols-2', rows: 'grid-rows-2' };
  } else if (nodeCount > 4) {
    return { columns: 'grid-cols-3', rows: 'grid-rows-2' };
  }
  return { columns: 'grid-cols-1', rows: 'grid-rows-1' };
}
