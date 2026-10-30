/**
 * Interface text scale — the accessibility size lever (Settings → General →
 * Appearance).
 *
 * Android's display-size slider is the model: one multiplier, applied to the
 * text the app draws, with the shipped size as the default. Here the
 * multiplier is a single CSS custom property, `--ui-text-scale`, written on
 * `<html>`. Every `text-*` step in App.css multiplies its own size by that
 * property (see the `@theme` typography block), so the sidebar, probe, title
 * bar and dialogs all move together and no component carries a second copy of
 * the scale.
 *
 * What it deliberately does NOT scale:
 *   - `rem`. The root font-size stays 13px, so spacing, control heights and
 *     the title bar's width tiers keep the geometry they were measured at.
 *   - Terminal text. xterm.js takes an absolute pixel size from the title
 *     bar's Zoom control (8–18px); a HUD readability preference has no
 *     business changing how much of a 120-column build log is on screen.
 *   - The mobile SPA and the Android client. Both are separate applications
 *     with their own type scales.
 *
 * Persistence matches the other per-machine presentation choices (`theme.ts`
 * and `terminalConfig.ts`): localStorage, read SYNCHRONOUSLY at module load,
 * so main.tsx can apply the scale before React mounts and the first paint is
 * already at the user's size — no flash of small text on launch.
 */

/** Smallest selectable scale. The shipped size is the floor: every step down
 *  from here would take `text-xs` under the 10px type floor the design system
 *  contracts to, which is why the slider starts at the default rather than
 *  below it. */
export const TEXT_SCALE_MIN = 1;
/** Largest selectable scale. Beyond 150% the dense chrome (28px node title
 *  bars holding 24px controls, 25px tab strips) starts clipping instead of
 *  reflowing, and this app has no room to reflow — several terminals share one
 *  window. */
export const TEXT_SCALE_MAX = 1.5;
/** Slider increment, as a fraction. Whole 5% steps read cleanly in the
 *  percentage readout and correspond to roughly 0.5px on the base body. */
export const TEXT_SCALE_STEP = 0.05;
export const TEXT_SCALE_DEFAULT = 1;

const STORAGE_KEY = 'buildmesh.text-scale';
const CSS_VARIABLE = '--ui-text-scale';

/** Round to whole percent — the readout and the slider both speak percent, and
 *  a stored 1.2500001 would render a readout of "125%" beside a slider handle
 *  that is not quite on the 125% tick. */
function quantise(scale: number): number {
  return Math.round(scale * 100) / 100;
}

function clamp(scale: number): number {
  if (!Number.isFinite(scale)) return TEXT_SCALE_DEFAULT;
  return Math.min(TEXT_SCALE_MAX, Math.max(TEXT_SCALE_MIN, quantise(scale)));
}

function loadTextScale(): number {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored !== null) {
      const percent = Number.parseInt(stored, 10);
      if (Number.isFinite(percent)) return clamp(percent / 100);
    }
  } catch {
    // localStorage unavailable (test env, private-mode Safari before user
    // interaction) — fall through to the default.
  }
  return TEXT_SCALE_DEFAULT;
}

let _currentTextScale = loadTextScale();

type TextScaleListener = (scale: number) => void;
const listeners = new Set<TextScaleListener>();

/** The active scale as a multiplier (1 = the size the app ships with). */
export function currentTextScale(): number {
  return _currentTextScale;
}

/**
 * Write the scale to `<html>` and localStorage. The default removes both —
 * the same convention `theme.ts` follows for dark, so a user who tried a
 * larger size once and went back leaves no trace behind.
 *
 * Listeners fire only when the in-memory value actually changes; the DOM and
 * storage are always synced, because the scale is the one place that renders
 * the value and a caller re-applying the active scale should not be able to
 * skip the write that got lost with a previous failed boot.
 */
export function setTextScale(scale: number): void {
  const next = clamp(scale);
  const changed = next !== _currentTextScale;
  _currentTextScale = next;
  writeScale(next);
  if (changed) listeners.forEach((cb) => cb(next));
}

/** Subscribe to scale changes. The subscriber is NOT called on registration —
 *  read `currentTextScale()` for the starting value. */
export function onTextScaleChange(cb: TextScaleListener): () => void {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}

/**
 * Apply the active scale to `<html>`. Called once at boot from main.tsx,
 * before React mounts, so the first paint is already at the user's size. Safe
 * to call repeatedly.
 */
export function applyTextScale(): void {
  writeScale(_currentTextScale);
}

function writeScale(scale: number): void {
  try {
    if (scale === TEXT_SCALE_DEFAULT) {
      document.documentElement.style.removeProperty(CSS_VARIABLE);
      localStorage.removeItem(STORAGE_KEY);
    } else {
      document.documentElement.style.setProperty(CSS_VARIABLE, String(scale));
      localStorage.setItem(STORAGE_KEY, String(Math.round(scale * 100)));
    }
  } catch {
    // document / localStorage unavailable — the in-memory scale (and any
    // listener that re-reads it) stays correct even when the DOM write cannot
    // land.
  }
}
