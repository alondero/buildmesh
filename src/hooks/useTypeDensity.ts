import { useSyncExternalStore } from 'react';
import {
  currentTypeDensity,
  onTypeDensityChange,
  type TypeDensity,
} from '../lib/typeDensity';

/**
 * useTypeDensity — the active type density, tracked live.
 *
 * Only the Settings control needs to re-render when the density changes:
 * everything else reads the `data-type-density` attribute from CSS. The
 * value is returned so the control can show which ladder is active.
 */
export function useTypeDensity(): TypeDensity {
  return useSyncExternalStore(onTypeDensityChange, currentTypeDensity);
}
