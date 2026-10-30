/**
 * Title-bar width tiers (#2421) — the viewport widths at which the bar mounts
 * a different set of controls. Chosen in JS (`useMinWidth`), not by CSS media
 * classes, because each tier swaps which component owns a destination.
 *
 *   < 1440px     `ViewSelector`: one labelled control naming the view and its
 *                scope, with the Mesh picker inside. Usage, Zoom, Settings and
 *                Mobile are rows in the More panel.
 *   1440–1699px  the five labelled `ViewModeSwitcher` segments and the
 *                `ScopeIndicator` picker; the utilities stay in More.
 *   >= 1700px    the utilities are labelled pills in the bar again.
 *
 * Both numbers are measured against the Filtered header grid, the tightest
 * one (`auto minmax(0,1fr) auto`: both side tracks content-sized, the
 * "Search or open" field absorbing the rest). At 1440px the labelled segments
 * plus the Filtered search bar take ~774px; with the utility pills' ~538px
 * also in the bar the centre was left ~105px against its 143px min-content,
 * so the field clipped and its shortcut hint spilled over the bell. Folding
 * the utilities frees that 538px until 1700px, where the scope label's own
 * tier (see `ScopeIndicator`) already measured the centre at 270px with every
 * label shown.
 */
export const LABELLED_SWITCHER_MIN_WIDTH = 1440;
export const UTILITIES_IN_BAR_MIN_WIDTH = 1700;

/**
 * A tier threshold at the active interface text size.
 *
 * Both numbers above were measured at the shipped 13px type scale — "the
 * labelled segments plus the Filtered search bar take ~774px" is a 100%
 * measurement. A user reading the app at 150% needs about 150% of that width
 * for the same controls to fit, so the thresholds move with the setting
 * (Settings → General → Appearance). At the default scale this returns the
 * measured numbers unchanged — the tiers existing screenshots and tests pin.
 */
export function tierWidthAtTextScale(px: number, textScale: number): number {
  return Math.round(px * textScale);
}
