/**
 * Interface type density — issue #2739.
 *
 * A second, coarser lever beside the interface text scale
 * (`textScale.ts`). The scale says "make everything 1.5× bigger"; the density
 * says "this is a dense tool, put the functional labels back where they
 * were". They are independent on purpose:
 *
 *   - Density picks WHICH of the two role ladders the app renders with.
 *     Comfortable (the default) sets a functional label at 13px — the root
 *     size, the size the interface reads at. Compact sets it at 11.375px and
 *     the metadata role at 9.75px, microcopy staying at the 10px floor.
 *     Compact is a LADDER, not a restoration of the old screens: before the
 *     roles existed every surface chose its own size, so a surface that used
 *     to be micro-sized reads at the label size and a surface that used to be
 *     drawn at the interface size (the Settings chrome) follows the label
 *     ladder too. What is guaranteed — and what the tests pin — is the size
 *     of each role.
 *   - The scale multiplies whichever ladder is active, so 150% of Compact is
 *     still 150%.
 *
 * Only the type ladder moves. `rem` — and therefore spacing, control heights
 * and the title bar's width tiers — is untouched, exactly as with the text
 * scale.
 *
 * Terminal typography is outside both levers: xterm.js takes an absolute
 * pixel size from the title bar's Zoom control (8–18px).
 *
 * Persistence matches the other per-machine presentation choices (`theme.ts`
 * and `textScale.ts`): localStorage, read SYNCHRONOUSLY at module load, so
 * `main.tsx` can apply the density before React mounts — the app renders with
 * the user's ladder from the first React paint.
 */

export type TypeDensity = 'comfortable' | 'compact';

/** The shipped ladder. A user who never touches the setting gets this. */
export const TYPE_DENSITY_DEFAULT: TypeDensity = 'comfortable';

const STORAGE_KEY = 'buildmesh.type-density';
const HTML_ATTRIBUTE = 'data-type-density';

function loadTypeDensity(): TypeDensity {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === 'compact') return 'compact';
  } catch {
    // localStorage unavailable (test env, private-mode Safari before user
    // interaction) — fall through to the default.
  }
  return TYPE_DENSITY_DEFAULT;
}

let _currentTypeDensity: TypeDensity = loadTypeDensity();

type TypeDensityListener = (density: TypeDensity) => void;
const listeners = new Set<TypeDensityListener>();

/** The active density. Read-only; mutate via setTypeDensity(). */
export function currentTypeDensity(): TypeDensity {
  return _currentTypeDensity;
}

/**
 * Write the density to `<html>` and localStorage.
 *
 * The default removes both — the same convention `theme.ts` follows for
 * dark, so a user who tried Compact once and went back leaves no trace
 * behind.
 *
 * Listeners fire only when the in-memory value actually changes; the DOM and
 * storage are always synced, because a caller re-applying the active density
 * should not be able to skip the write that got lost with a previous failed
 * boot.
 */
export function setTypeDensity(next: TypeDensity): void {
  const changed = next !== _currentTypeDensity;
  _currentTypeDensity = next;
  writeDensity(next);
  if (changed) listeners.forEach((cb) => cb(next));
}

/**
 * Subscribe to density changes. The subscriber is NOT called on registration
 * — read `currentTypeDensity()` for the starting value.
 */
export function onTypeDensityChange(cb: TypeDensityListener): () => void {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}

/**
 * Apply the active density to `<html>`. Called once at boot from main.tsx,
 * before React mounts, so the first paint is already at the user's density.
 * Safe to call repeatedly.
 */
export function applyTypeDensity(): void {
  writeDensity(_currentTypeDensity);
}

function writeDensity(density: TypeDensity): void {
  try {
    if (density === TYPE_DENSITY_DEFAULT) {
      document.documentElement.removeAttribute(HTML_ATTRIBUTE);
      localStorage.removeItem(STORAGE_KEY);
    } else {
      document.documentElement.setAttribute(HTML_ATTRIBUTE, density);
      localStorage.setItem(STORAGE_KEY, density);
    }
  } catch {
    // document / localStorage unavailable — the in-memory density (and any
    // listener that re-reads it) stays correct even when the DOM write cannot
    // land.
  }
}
