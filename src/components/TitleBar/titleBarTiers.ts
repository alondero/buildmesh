/**
 * Title-bar width tiers (#2421) — the viewport widths at which the bar mounts
 * a different set of controls. Chosen in JS (`useMinWidth`), not by CSS media
 * classes, because each tier swaps which component owns a destination.
 *
 *   < 1700px     `ViewSelector`: one labelled control naming the view and its
 *                scope, with the Mesh picker inside. Usage, Zoom, Settings and
 *                Mobile are rows in the More panel.
 *   1700–1999px  the four labelled `ViewModeSwitcher` segments and the
 *                `ScopeIndicator` picker; the utilities are labelled pills in
 *                the bar again.
 *   >= 2000px    the scope indicator shows its label, not just its glyph.
 *
 * Every number is measured against the CENTRED header grid
 * (`minmax(0,1fr) auto minmax(0,1fr)`, from 1300px up), because that grid
 * CAPS both side tracks at half of what the command field leaves: the left
 * cluster's controls have to fit a track that is roughly a third of the
 * window, not a track that can grow to whatever the cluster needs.
 *
 * The command field is `clamp(20rem, 100vw - 1820px, 640px)` — 260px on
 * narrow windows — and the centre cell carries 8px of padding on each side,
 * so each side track is `(viewport - 16 - field) / 2`:
 *
 *   viewport <= 2080   field 260px          track = (viewport - 276) / 2
 *   2080..2460         field grows          track = 902px (pinned)
 *   >= 2460            field 640px          track = (viewport - 656) / 2
 *
 * #2683 put the grid search in the left cluster for every View Mode (it used
 * to mount only in Filtered, whose header grid was content-sized and
 * therefore never capped). That widened the cluster, so every number below
 * is re-derived against the capped grid.
 *
 * The ladder is derived from the WORST left cluster across BOTH window chrome
 * layouts. On macOS the traffic lights live in the LEFT cluster (64px of
 * them), so that cluster is wider than the Windows one at every composition —
 * a ladder measured on Windows alone overflows on macOS. Measured in the real
 * window, worst chrome, 100% text:
 *
 *   viewport  composition               track   cluster   overflow
 *   1300px    the centred grid starts   512px   508px     0
 *   1700px    the labelled switcher     712px   708px     0
 *   2000px    the scope label           862px   858px     0
 *   2310px    the field reaches its     902px   898px     0
 *              640px cap, so the track
 *              stops growing
 *
 * Each row is a composition step, and the fit is only ever tightest AT a step:
 * the cluster's content is a step function of the width (a component mounts at
 * a tier) while the track grows smoothly. The field's 1820px reservation is
 * the same arithmetic seen from the other side — the pinned track is
 * `(1820 - 16) / 2 = 902px`, which is what the widest macOS cluster needs.
 *
 * The ladder's tiers are JS tiers, so they move with the interface text size.
 * At 150% the same two steps land at 2550px and 3000px, where the tracks are
 * 947px and 1172px against 849px and 1062px of cluster. The centred grid's
 * breakpoint is a CSS tier and does not scale: at 150% the 1300px start is the
 * tightest point of the whole ladder (the grid search input is at its 78px
 * floor there), and it still fits.
 */
export const LABELLED_SWITCHER_MIN_WIDTH = 1700;
export const SCOPE_LABEL_MIN_WIDTH = 2000;
export const UTILITIES_IN_BAR_MIN_WIDTH = 1700;

/**
 * A tier threshold at the active interface text size.
 *
 * The numbers above were measured at the shipped 13px type scale — the
 * overflow table above is a 100% measurement. A user reading the app at 150%
 * needs about 150% of that width for the same controls to fit (the labelled
 * switcher's step moves from 1700px to 2550px, and the scope label's from
 * 2000px to 3000px), so the thresholds move with the setting (Settings →
 * General → Appearance). At the default scale this returns the measured
 * numbers unchanged — the tiers existing screenshots and tests pin.
 */
export function tierWidthAtTextScale(px: number, textScale: number): number {
  return Math.round(px * textScale);
}
