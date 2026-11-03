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
 * The track grows with the viewport until 2080px, where it reaches 902px;
 * from there the field takes over the extra width, so the track stays pinned
 * at 902px until the field hits its 640px cap at 2460px, past which the track
 * grows again.
 *
 * #2683 put the grid search in the left cluster for every View Mode (it used
 * to mount only in Filtered, whose header grid was content-sized and
 * therefore never capped). That widened the cluster, so every number below
 * is re-derived against the capped grid.
 *
 * ## What was measured, and where
 *
 * The figures come from a browser sweep in the mock-IPC Chromium harness —
 * NOT a real macOS window. The "macOS chrome" is this app's own traffic-light
 * React components in a Chromium page with `navigator.platform` overridden;
 * there is no WebKit, no macOS build and no native caption geometry in it.
 * The widths and the flex arithmetic are therefore Chromium's, and a native
 * macOS build has never been measured.
 *
 * ## The two chrome layouts
 *
 * On macOS the traffic lights live in the LEFT cluster (64px of them), so
 * that cluster is wider than the Windows one at every composition — a ladder
 * measured on Windows alone overflows on macOS. The RIGHT cluster is the
 * other way round: the caption buttons render only on Windows, so Windows is
 * the wider one there.
 *
 * The left cluster's NATURAL width — what it needs when nothing squeezes it,
 * with a deliberately long Mesh name so the scope label sits at its cap — is
 * 902px on Windows and 966px on macOS at 100% text. The 64px between them is
 * the macOS traffic lights, the only macOS-only element in this cluster:
 * three 12px circles, two 8px gaps and 12px of padding (the spacing steps
 * App.css pins in px, so they do not move with the root font). The track is
 * capped, so the cluster is squeezed to fit it rather than running under the
 * field, and the squeeze is what the measurements below record. macOS, 100%
 * text, the wider left cluster:
 *
 *   viewport  composition               track   cluster   overflow
 *   1300px    the centred grid starts   512px   508px     0
 *   1700px    the labelled switcher     712px   708px     0
 *   2000px    the scope label           862px   858px     0
 *   2310px    the pinned band           902px   898px     0
 *   2460px    the field reaches its     902px   898px     0
 *              640px cap, so the track
 *              starts growing again
 *
 * Each row is a composition step, and the fit is only ever tightest AT a step:
 * the cluster's content is a step function of the width (a component mounts at
 * a tier) while the track grows smoothly.
 *
 * The reservation is 1820px = `2 * 902 + 16`, sized for the WINDOWS natural
 * width; a 902px track carries 898px of cluster, so even Windows gives a
 * little up in the pinned band — 2px of the scope label (208px -> 206px) and
 * 2px of the grid search (130px -> 128px). macOS gives up 68px: 40px of the
 * scope label (208px -> 168px) and 28px of the grid search (130px -> 102px),
 * so nothing overflows and the label truncates. Sizing the track to the
 * macOS natural width instead (1948px = `2 * 966 + 16`) would cost the
 * command field 128px of width across the whole band and push its 640px cap
 * from 2460px to 2588px — a worse trade, because the extra 40px of scope
 * label is text that truncates on a long name anyway.
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
