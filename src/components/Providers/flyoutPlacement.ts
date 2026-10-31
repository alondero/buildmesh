/**
 * Where a spawn-menu flyout (the login submenu or the configuration submenu)
 * sits beside the row it was opened from.
 */
export interface FlyoutPlacement { left: number; top: number; maxHeight: number }

/**
 * Room the flyout needs below its row before it opens above the row instead.
 * Below this the panel would be a sliver, so flipping up (with the panel's
 * bottom edge on the row) reads better than a scrollbar in a strip.
 */
export const MIN_ROOM_BELOW = 120;

const MARGIN = 8;

/**
 * Place a flyout of `width` × `height` (its natural content height) beside
 * `row`, inside a `viewportWidth` × `viewportHeight` window.
 */
export function placeFlyout(
  row: { left: number; right: number; top: number; bottom: number },
  width: number,
  height: number,
  viewportWidth: number,
  viewportHeight: number,
): FlyoutPlacement {
  // Horizontal: open to the right of the row; flip to the left only when that
  // side fits on screen, otherwise keep the row's left edge so the panel stays
  // on the row it was opened from.
  let left = row.right;
  if (left + width > viewportWidth - MARGIN) {
    const flipped = row.left - width;
    left = flipped >= MARGIN ? flipped : row.left;
  }
  left = Math.max(MARGIN, Math.min(left, viewportWidth - width - MARGIN));
  // Vertical: hang from the row's top line and scroll inside the panel. Open
  // above the row only when there is no usable room below it and more room
  // above, so the panel keeps touching the row. The row edges are clipped to
  // the viewport: a row only partly visible at a scroll boundary must not hand
  // the panel room past the viewport edge.
  const rowTop = Math.min(Math.max(row.top, MARGIN), viewportHeight - MARGIN);
  const rowBottom = Math.min(Math.max(row.bottom, MARGIN), viewportHeight - MARGIN);
  const roomBelow = viewportHeight - MARGIN - rowTop;
  const roomAbove = rowBottom - MARGIN;
  let top = rowTop;
  let maxHeight = roomBelow;
  if (roomBelow < MIN_ROOM_BELOW && roomAbove > roomBelow) {
    top = Math.max(MARGIN, rowBottom - Math.min(height, roomAbove));
    maxHeight = roomAbove;
  }
  return { left, top, maxHeight };
}
