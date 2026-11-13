/**
 * Pure geometry for anchored overlay panels (issue #2773).
 *
 * Two surfaces in Buildmesh open a small panel next to a point the user
 * interacted with: the shared settings help tip (`InfoTip`) and the Circuit
 * Quick Connect menu. Both used to place themselves with a hard-coded
 * offset, so a trigger near an edge pushed the panel outside the area the
 * user could see — and a panel clipped by an `overflow` ancestor is worse
 * than no panel at all.
 *
 * This module is the one place that decides *where* such a panel goes. It is
 * pure geometry on purpose:
 *
 *  - The coordinate space is the caller's. `InfoTip` measures in viewport
 *    pixels because it portals to `document.body`; the Circuit editor
 *    measures in canvas-wrapper pixels because its menu is an absolutely
 *    positioned child of that wrapper. Neither space converts into the
 *    other without a live DOM measurement, so the caller supplies both the
 *    anchor and the bounds in one space and this module does not guess
 *    between them.
 *  - Every edge case (flip, clamp, bounded height) is a pure function of
 *    its arguments, so the behaviour can be asserted directly instead of
 *    through class strings.
 */

/** Size of an overlay panel before it is constrained. */
export interface PanelSize {
  width: number;
  height: number;
}

/** The panel's top-left corner in the caller's coordinate space. */
export interface PanelOrigin {
  left: number;
  top: number;
}

/** The visible area the panel has to stay inside. */
export interface PanelBounds {
  width: number;
  height: number;
}

export interface FitPanelOptions {
  /** The panel's natural (unconstrained) size. */
  size: PanelSize;
  /**
   * Where the panel goes when it opens on its preferred side (below the
   * anchor). `top` is the panel's top edge.
   */
  preferred: PanelOrigin;
  /**
   * Where the panel goes when it opens on its alternate side (above the
   * anchor). `bottom` is the panel's *bottom* edge — the panel grows
   * upwards from there.
   */
  alternate: { left: number; bottom: number };
  /** The visible area the panel has to stay inside. */
  bounds: PanelBounds;
  /** Gutter kept between the panel and the bounds' edges. Default `8`. */
  margin?: number;
}

export interface PanelFit {
  /** The panel's left edge in the caller's coordinate space. */
  left: number;
  /** The panel's top edge in the caller's coordinate space. */
  top: number;
  /**
   * The height the panel may occupy at most. The panel scrolls its own
   * content when the content is taller than this.
   */
  maxHeight: number;
  /** The height the panel takes once `maxHeight` is applied. */
  height: number;
  /** Which side of the anchor the panel opened on. */
  side: 'preferred' | 'alternate';
}

/** Default gutter between an overlay panel and the edge of its bounds. */
export const OVERLAY_MARGIN = 8;

/**
 * Place a panel next to an anchor so that it stays inside `bounds`.
 *
 * The rules, in order:
 *
 *  1. **Vertical fit.** If the panel's natural height fits below the
 *     anchor it opens there. If it does not, it opens above the anchor
 *     instead. If it fits on neither side, the panel opens on the side
 *     with more room and its height is bounded to that room — the content
 *     scrolls inside the panel, so the panel never runs off the edge.
 *  2. **Horizontal fit.** The panel keeps the preferred left edge while it
 *     fits. When it would overflow, the alternate left edge (the anchor's
 *     other edge) is used, and only if that overflows too is the panel
 *     clamped into the bounds.
 *
 * The returned `maxHeight` is a *bound*, not a size: the caller renders the
 * panel with `overflow-y: auto` so long content stays reachable.
 */
export function fitPanelToBounds(options: FitPanelOptions): PanelFit {
  const margin = options.margin ?? OVERLAY_MARGIN;
  const { size, preferred, alternate, bounds } = options;

  // The anchor's own edges are clamped into the bounds first: an anchor
  // that sits outside the visible area must not push the panel out with
  // it. `preferredTop` is the panel's top edge on the preferred side,
  // `alternateBottom` the panel's bottom edge on the alternate side.
  const preferredTop = Math.min(
    Math.max(margin, preferred.top),
    Math.max(margin, bounds.height - margin),
  );
  const alternateBottom = Math.min(
    Math.max(margin, alternate.bottom),
    Math.max(margin, bounds.height - margin),
  );

  // Space available on each side of the anchor, inside the gutter.
  const spaceBelow = Math.max(0, bounds.height - margin - preferredTop);
  const spaceAbove = Math.max(0, alternateBottom - margin);

  let side: 'preferred' | 'alternate';
  let top: number;
  let maxHeight: number;
  if (size.height <= spaceBelow) {
    side = 'preferred';
    top = preferredTop;
    maxHeight = spaceBelow;
  } else if (size.height <= spaceAbove) {
    side = 'alternate';
    top = alternateBottom - size.height;
    maxHeight = spaceAbove;
  } else if (spaceBelow >= spaceAbove) {
    // Neither side holds the whole panel — open on the roomier side and
    // let the content scroll.
    side = 'preferred';
    top = preferredTop;
    maxHeight = spaceBelow;
  } else {
    side = 'alternate';
    maxHeight = spaceAbove;
    top = alternateBottom - maxHeight;
  }

  // Horizontal: keep the preferred edge while it fits, otherwise try the
  // alternate edge, otherwise clamp into the bounds.
  const overflows = (left: number) => left + size.width > bounds.width - margin;
  let left = preferred.left;
  if (overflows(left) && !overflows(alternate.left)) left = alternate.left;
  const maxLeft = Math.max(margin, bounds.width - margin - size.width);
  left = Math.min(Math.max(margin, left), maxLeft);

  return {
    left,
    top,
    maxHeight,
    height: Math.min(size.height, maxHeight),
    side,
  };
}
