import { useEffect, useRef, useState, type RefObject } from 'react';

/** Keep the content size fixed during a split adjustment until its viewport changes. */
export function useGridResizeFloor(viewport: RefObject<HTMLElement | null>, layoutKey: string) {
  const [floor, setFloor] = useState({ width: 0, height: 0 });
  const previousSize = useRef<{ width: number; height: number } | null>(null);

  useEffect(() => { setFloor({ width: 0, height: 0 }); }, [layoutKey]);

  useEffect(() => {
    if (!viewport.current || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(entries => {
      const bounds = entries[entries.length - 1]?.contentRect;
      if (!bounds) return;
      const previous = previousSize.current;
      previousSize.current = { width: bounds.width, height: bounds.height };
      if (!previous) return;
      setFloor(current => {
        const width = previous.width === bounds.width ? current.width : 0;
        const height = previous.height === bounds.height ? current.height : 0;
        return width === current.width && height === current.height ? current : { width, height };
      });
    });
    observer.observe(viewport.current);
    return () => observer.disconnect();
  }, [viewport]);

  const capture = (axis: 'width' | 'height', content: HTMLElement | null) => {
    if (!content) return;
    const size = content.getBoundingClientRect()[axis];
    setFloor(current => ({ ...current, [axis]: size }));
  };

  return { floor, capture };
}
