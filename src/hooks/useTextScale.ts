import { useSyncExternalStore } from 'react';
import { currentTextScale, onTextScaleChange } from '../lib/textScale';

/**
 * useTextScale — the active interface text multiplier, tracked live.
 *
 * For the handful of places that size a *container* around its text and so
 * have to follow the setting in JavaScript rather than through the CSS
 * variable: the title bar's width tiers (#2421), whose thresholds were
 * measured at the shipped 13px type scale. Everything else reads
 * `--ui-text-scale` from CSS and needs no hook.
 */
export function useTextScale(): number {
  return useSyncExternalStore(onTextScaleChange, currentTextScale);
}
