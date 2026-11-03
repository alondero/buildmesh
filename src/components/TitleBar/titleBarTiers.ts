/**
 * Title-bar width tiers (#2421) — the viewport widths at which the bar mounts
 * a different set of controls. Chosen in JS (`useMinWidth`), not by CSS media
 * classes, because each tier swaps which component owns a destination.
 *
 *   < 1540px     `ViewSelector`: one labelled control naming the view and its
 *                scope, with the Mesh picker inside. Usage, Zoom, Settings and
 *                Mobile are rows in the More panel.
 *   1540–1699px  the four labelled `ViewModeSwitcher` segments and the
 *                `ScopeIndicator` picker; the utilities stay in More.
 *   >= 1700px    the utilities are labelled pills in the bar again.
 *   >= 1900px    the scope indicator shows its label, not just its glyph.
 *
 * Every number is measured against the CENTRED header grid
 * (`minmax(0,1fr) auto minmax(0,1fr)`, from 1200px up), because that grid
 * CAPS both side tracks at half of what the command field leaves: the left
 * cluster's controls have to fit a track that is roughly a third of the
 * window, not a track that can grow to whatever the cluster needs.
 *
 * #2683 put the grid search in the left cluster for every View Mode (it used
 * to mount only in Filtered, whose header grid was content-sized and
 * therefore never capped). That added ~178px to the cluster, so every number
 * below is re-derived against the capped grid, measured in the real window:
 *
 *   viewport   left track   left content   overflow
 *   1440px     582px        609px          27px
 *   1500px     612px        616px          4px
 *   1520px     622px        622px          0
 *
 * 1540px is the first width with room to spare above that fit, and the scope
 * label waits for 1900px: with the grid search in the cluster the label needs
 * 121px of track it cannot have before then (108px of text plus its gap), and
 * before 1900px the same track is what the search input needs instead.
 */
export const LABELLED_SWITCHER_MIN_WIDTH = 1540;
export const SCOPE_LABEL_MIN_WIDTH = 1900;
export const UTILITIES_IN_BAR_MIN_WIDTH = 1700;

/**
 * A tier threshold at the active interface text size.
 *
 * The numbers above were measured at the shipped 13px type scale — the
 * overflow table above is a 100% measurement. A user reading the app at 150%
 * needs about 150% of that width for the same controls to fit (the labelled
 * cluster measured 2305px clear at 150% against 1540px at 100%), so the
 * thresholds move with the setting (Settings → General → Appearance). At the
 * default scale this returns the measured numbers unchanged — the tiers
 * existing screenshots and tests pin.
 */
export function tierWidthAtTextScale(px: number, textScale: number): number {
  return Math.round(px * textScale);
}
