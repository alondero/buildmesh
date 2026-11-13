/**
 * paneResize — the resize maths shared by the agent grid's splitters.
 *
 * Two components resize panes by a percentage of the container:
 * `GridSplitter` (the mesh-scoped column/row grid) and `ResizablePanes`
 * (the flat row in `AgentNodeView`). Both had their own copy of the
 * same two ideas — the minimum pane size and the "pull the two panes
 * either side of the handle toward each other" clamp — and that
 * duplication is what let the keyboard path (#2424) drift from the drag
 * path. One module now owns both, so a keyboard step and a mouse drag
 * obey the same floor.
 *
 * The minimum pane size is expressed in pixels (`MIN_PANE_PX`) because
 * that is what the layout needs: a pane narrower than 300px leaves too
 * little room for a terminal. It is converted to a percentage of the
 * container at the call site, because the layout model stores shares.
 */
import { SPLITTER_HANDLE_WIDTH } from '../../hooks/useResizable';

/**
 * The narrowest a pane may become, in pixels. Shared by the grid and
 * the flat pane row so both refuse to squeeze a terminal below the
 * size that still renders one.
 */
export const MIN_PANE_PX = 300;

/**
 * A percentage floor for the flat pane row: with a very wide container
 * the pixel floor alone would still allow a sliver of a pane (300px out
 * of 4000px), so the row also refuses to go below 15%.
 */
export const MIN_PANE_PERCENT = 15;

/**
 * The narrowest a pane may become when `paneCount` panes share the
 * container: the pixel floor plus the share of the splitter handles that
 * sits inside the pane's own box — a row of n panes carries n-1 handles,
 * and each pane's width allowance holds its (n-1)/n portion of them.
 */
export function minPaneSizePx(paneCount: number): number {
  if (paneCount <= 0) return MIN_PANE_PX;
  return MIN_PANE_PX + (paneCount - 1) * SPLITTER_HANDLE_WIDTH / paneCount;
}

/**
 * The same floor expressed as a percentage of the container: the pixel
 * floor converted, with the flat pane row's percentage floor applied on
 * top (a container wide enough for the pixel floor to be a sliver still
 * refuses to go below `MIN_PANE_PERCENT`).
 */
export function minPanePercent(paneCount: number, containerSizePx: number): number {
  if (!containerSizePx) return MIN_PANE_PERCENT;
  return Math.max(MIN_PANE_PERCENT, (minPaneSizePx(paneCount) / containerSizePx) * 100);
}

/**
 * Pull the i-th and (i+1)-th cells of `baseline` toward `+delta` and
 * `-delta` respectively, clamped to `minPct` on either side (when one
 * side hits the floor the other soaks up the slack so left+right stays
 * constant).
 *
 * Used by the col-axis and row-axis resizes in `GridSplitter` and by
 * the flat `ResizablePanes` row; it collapses the duplicated math into
 * one helper so future bugfixes land in a single place.
 */
export function clampResizeAdjacentPair(
  baseline: number[],
  i: number,
  deltaPct: number,
  minPct: number,
): number[] {
  let newLeft = baseline[i] + deltaPct;
  let newRight = baseline[i + 1] - deltaPct;
  if (newLeft < minPct) { newLeft = minPct; newRight = baseline[i] + baseline[i + 1] - minPct; }
  if (newRight < minPct) { newRight = minPct; newLeft = baseline[i] + baseline[i + 1] - minPct; }
  const next = [...baseline];
  next[i] = newLeft;
  next[i + 1] = newRight;
  return next;
}
