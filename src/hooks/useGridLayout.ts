/**
 * Number of nodes to place in each row, balanced across rows and capped at 3
 * wide. The last (partial) row spreads its terminals across the full width
 * rather than leaving an empty cell, e.g. 5 -> [3, 2], 7 -> [3, 2, 2].
 */
export function getGridRows(nodeCount: number): number[] {
  if (nodeCount <= 0) return [];
  const rows = nodeCount <= 2 ? 1 : Math.ceil(nodeCount / 3);
  const base = Math.floor(nodeCount / rows);
  const extra = nodeCount % rows;
  // Bigger rows first: [3, 2, 2] rather than [2, 2, 3].
  return Array.from({ length: rows }, (_, i) => base + (i < extra ? 1 : 0));
}

export function equalSizes(n: number): number[] {
  return Array.from({ length: n }, () => 100 / n);
}
