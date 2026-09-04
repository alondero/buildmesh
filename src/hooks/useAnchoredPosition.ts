/**
 * Position a fixed menu against a trigger while keeping it inside the
 * viewport. The menu may live in a portal, so coordinates are viewport
 * relative and ancestor overflow does not affect it.
 *
 * Positioning is kept in one hook because menus must also follow their
 * trigger while a grid, sidebar, or terminal scrolls. Scroll work is
 * coalesced into one animation-frame measurement, and ResizeObserver
 * handles menu content changing while it is open.
 */
import { useLayoutEffect, type RefObject } from 'react';

export interface UseAnchoredPositionOptions {
  /** Align the menu's start or end edge with the trigger. */
  align?: 'start' | 'end';
  /** Gap between the trigger and the menu in pixels. */
  gap?: number;
  /** Minimum distance from the viewport edges in pixels. */
  margin?: number;
}

export function useAnchoredPosition(
  triggerRef: RefObject<HTMLElement | null>,
  menuRef: RefObject<HTMLElement | null>,
  open: boolean,
  { align = 'start', gap = 4, margin = 4 }: UseAnchoredPositionOptions = {},
): void {
  useLayoutEffect(() => {
    if (!open) return;
    const trigger = triggerRef.current;
    const menu = menuRef.current;
    if (!trigger || !menu) return;

    let frame: number | null = null;

    const position = () => {
      frame = null;
      const triggerRect = trigger.getBoundingClientRect();
      const menuRect = menu.getBoundingClientRect();
      const triggerHasLayout = triggerRect.width > 0 && triggerRect.height > 0;
      const offscreen = triggerHasLayout && (
        triggerRect.bottom <= 0 ||
        triggerRect.top >= window.innerHeight ||
        triggerRect.right <= 0 ||
        triggerRect.left >= window.innerWidth
      );

      if (offscreen) {
        menu.style.visibility = 'hidden';
        return;
      }

      const maxLeft = Math.max(margin, window.innerWidth - menuRect.width - margin);
      const anchoredLeft = align === 'end'
        ? triggerRect.right - menuRect.width
        : triggerRect.left;
      const left = Math.max(margin, Math.min(anchoredLeft, maxLeft));

      const belowTop = triggerRect.bottom + gap;
      const aboveTop = triggerRect.top - menuRect.height - gap;
      const fitsBelow = belowTop + menuRect.height <= window.innerHeight - margin;
      const fitsAbove = aboveTop >= margin;
      const maxTop = Math.max(margin, window.innerHeight - menuRect.height - margin);
      const top = fitsBelow
        ? belowTop
        : fitsAbove
          ? aboveTop
          : Math.max(margin, Math.min(belowTop, maxTop));

      menu.style.visibility = '';
      menu.style.left = `${left}px`;
      menu.style.top = `${top}px`;
    };

    const schedulePosition = () => {
      if (frame !== null) return;
      frame = window.requestAnimationFrame(position);
    };

    position();
    window.addEventListener('resize', schedulePosition);
    window.addEventListener('scroll', schedulePosition, true);

    const Observer = typeof ResizeObserver === 'undefined' ? null : ResizeObserver;
    const observer = Observer ? new Observer(schedulePosition) : null;
    observer?.observe(trigger);
    observer?.observe(menu);

    return () => {
      window.removeEventListener('resize', schedulePosition);
      window.removeEventListener('scroll', schedulePosition, true);
      if (frame !== null) window.cancelAnimationFrame(frame);
      observer?.disconnect();
      menu.style.visibility = '';
    };
  }, [align, gap, margin, menuRef, open, triggerRef]);
}
