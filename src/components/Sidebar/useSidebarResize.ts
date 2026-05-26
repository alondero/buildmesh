import { useState, useEffect, useRef } from 'react';

const MIN_WIDTH = 192;
const MAX_WIDTH = 480;
const DEFAULT_WIDTH = 256;

export function useSidebarResize(initialWidth = DEFAULT_WIDTH) {
  const [width, setWidth] = useState(initialWidth);
  const [isResizing, setIsResizing] = useState(false);
  const resizingRef = useRef(false);
  const startXRef = useRef(0);
  const startWidthRef = useRef(initialWidth);

  const handleMouseDown = (e: React.MouseEvent) => {
    e.preventDefault();
    resizingRef.current = true;
    startXRef.current = e.clientX;
    startWidthRef.current = width;
    setIsResizing(true);
  };

  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      if (!resizingRef.current) return;
      const delta = e.clientX - startXRef.current;
      setWidth(Math.max(MIN_WIDTH, Math.min(MAX_WIDTH, startWidthRef.current + delta)));
    };

    const handleMouseUp = () => {
      if (resizingRef.current) {
        resizingRef.current = false;
        setIsResizing(false);
      }
    };

    document.addEventListener('mousemove', handleMouseMove);
    document.addEventListener('mouseup', handleMouseUp);
    return () => {
      document.removeEventListener('mousemove', handleMouseMove);
      document.removeEventListener('mouseup', handleMouseUp);
    };
  }, []);

  return { width, isResizing, handleMouseDown };
}
