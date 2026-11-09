import { useSyncExternalStore } from 'react';
import { currentWindowZoom, onWindowZoomChange } from '../lib/webviewZoom';

/**
 * useWindowZoom — the active native window zoom, as a percentage.
 *
 * The Settings control reads it so the radio group reflects the value the
 * app is actually running at. The zoom itself is applied by the platform,
 * not by CSS, so there is no attribute on <html> to read.
 */
export function useWindowZoom(): number {
  return useSyncExternalStore(onWindowZoomChange, currentWindowZoom);
}
