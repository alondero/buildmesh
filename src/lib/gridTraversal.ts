/**
 * Compute the next flat node index when the user presses an arrow key.
 *
 * Used by the keyboard-shortcut handler that traverses the on-screen agent-node
 * grid (Ctrl/Cmd+Alt+←/→/↑/↓). The grid is laid out row-first using
 * `getGridRows(nodeCount)`; nodes are addressed by their flat index in the
 * scoped, on-screen list (the active View Mode's node set) — i.e. index 0 is
 * the top-left, increasing left-to-right, top-to-bottom.
 *
 * Edge semantics, locked in by user testing:
 *   • Left / Right — wrap within the current row. A row of one node is a dead
 *     end (no movement). When there's only one row (1–2 nodes), Up / Down
 *     are no-ops; Left / Right still wrap.
 *   • Up / Down   — try the same column in the adjacent row. If that row is
 *     narrower than the source column, clamp to its last column. The top
 *     and bottom rows have no row to step into, so the press is a no-op
 *     (no wrap-around at the grid's vertical edges).
 *
 * Pure function. `traversalTargetId` (below) scopes the visible node set and
 * maps the returned index back to a node id.
 */

import { getGridRows } from '../hooks/useGridLayout';

export type ArrowDirection = 'left' | 'right' | 'up' | 'down';

export function arrowTargetIndex(
  currentIndex: number,
  direction: ArrowDirection,
  rows: number[],
): number {
  if (rows.length === 0) return currentIndex;

  // Locate the current (row, col) from the flat index. `acc` is the offset
  // of the start of the current row in the flat array.
  let acc = 0;
  let rowIdx = 0;
  let col = 0;
  for (let r = 0; r < rows.length; r++) {
    const rowLen = rows[r];
    if (currentIndex < acc + rowLen) {
      rowIdx = r;
      col = currentIndex - acc;
      break;
    }
    acc += rowLen;
  }

  if (direction === 'left') {
    const rowLen = rows[rowIdx];
    if (rowLen <= 1) return currentIndex;
    return acc + (col - 1 + rowLen) % rowLen;
  }

  if (direction === 'right') {
    const rowLen = rows[rowIdx];
    if (rowLen <= 1) return currentIndex;
    return acc + (col + 1) % rowLen;
  }

  if (direction === 'up') {
    if (rowIdx === 0) return currentIndex;
    const prevRowLen = rows[rowIdx - 1];
    const prevRowStart = acc - rows[rowIdx - 1];
    const targetCol = Math.min(col, prevRowLen - 1);
    return prevRowStart + targetCol;
  }

  // direction === 'down'
  if (rowIdx >= rows.length - 1) return currentIndex;
  const nextRowLen = rows[rowIdx + 1];
  const nextRowStart = acc + rows[rowIdx];
  const targetCol = Math.min(col, nextRowLen - 1);
  return nextRowStart + targetCol;
}

/**
 * The node an arrow press moves focus to, given the *visible* node set for the
 * active View Mode (wayfinder #982, ticket #987). The caller scopes it with
 * `scopeNodesForMode` (Mesh, Pinned — cross-mesh — or All), replacing the old
 * mesh-only filter that stranded Ctrl+Alt+Arrow on the active node's mesh.
 *
 * `visibleNodes` must be in on-screen order — the store's canonical
 * (mesh_id, position) order that `scopeNodesForMode` preserves — so a flat
 * index maps to its grid cell. Returns null for a no-op press (active node not
 * in the set, or an edge that stays put) so the caller leaves focus alone.
 */
export function traversalTargetId(
  visibleNodes: readonly { id: number }[],
  activeNodeId: number,
  direction: ArrowDirection,
): number | null {
  const currentIndex = visibleNodes.findIndex(n => n.id === activeNodeId);
  if (currentIndex === -1) return null;
  const rows = getGridRows(visibleNodes.length);
  const targetIndex = arrowTargetIndex(currentIndex, direction, rows);
  const target = visibleNodes[targetIndex];
  return target && target.id !== activeNodeId ? target.id : null;
}
