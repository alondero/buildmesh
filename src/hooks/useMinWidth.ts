import { useCallback, useSyncExternalStore } from 'react';

/**
 * useMinWidth — whether the viewport is at least `px` wide, tracked live
 * through `matchMedia`.
 *
 * For layouts that must MOUNT a different control per width tier rather than
 * hide one with a CSS media class (#2421): two mounted pickers would both
 * answer the same open request, and jsdom applies no media queries, so a
 * CSS-only tier cannot be pinned in a unit test. Tests stub `matchMedia`.
 *
 * Where `matchMedia` is missing (jsdom without a stub) it reports `true`, so
 * existing suites keep rendering the widest layout they were written for.
 */
export function useMinWidth(px: number): boolean {
  const query = `(min-width: ${px}px)`;
  const subscribe = useCallback((onChange: () => void) => {
    if (typeof window.matchMedia !== 'function') return () => {};
    const list = window.matchMedia(query);
    list.addEventListener('change', onChange);
    return () => list.removeEventListener('change', onChange);
  }, [query]);
  const snapshot = () => (typeof window.matchMedia === 'function' ? window.matchMedia(query).matches : true);
  return useSyncExternalStore(subscribe, snapshot);
}
