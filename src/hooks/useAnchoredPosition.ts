/**
 * Position a fixed menu against a trigger while keeping it inside the
 * viewport. The menu may live in a portal, so coordinates are viewport
 * relative and ancestor overflow does not affect it.
 *
 * Positioning is kept in one hook because menus must also follow their
 * trigger while a grid, sidebar, or terminal scrolls. Scroll work is
 * coalesced into one animation-frame measurement, and ResizeObserver
 * handles menu content changing while it is open.
 *
 * Issue #2773 added the `constrainToViewport` mode. The default mode
 * keeps the historical behaviour (align, flip above, clamp into the
 * viewport) for the menus that already rely on it. The constrained mode
 * adds the two things a panel needs when it can be opened against an
 * edge: it flips its horizontal alignment when the preferred edge would
 * push it past the viewport, and it bounds its height to the room
 * available on the side it opens on so long content scrolls inside the
 * panel instead of running off the screen. The geometry itself lives in
 * `src/lib/overlayLayout.ts` so the Circuit editor — which measures in
 * canvas-wrapper pixels rather than viewport pixels — can reuse it.
 */
import { useLayoutEffect, type RefObject } from 'react';
import { fitPanelToBounds } from '../lib/overlayLayout';

export interface UseAnchoredPositionOptions {
  /** Align the menu's start or end edge with the trigger. */
  align?: 'start' | 'end';
  /** Gap between the trigger and the menu in pixels. */
  gap?: number;
  /** Minimum distance from the viewport edges in pixels. */
  margin?: number;
  /**
   * Keep the menu inside the viewport (issue #2773): flip the horizontal
   * alignment when the preferred edge would overflow, and bound the
   * menu's height to the room available on the side it opens on. The menu
   * must scroll its own content (`overflow-y: auto`) for the height
   * bound to keep long content reachable.
   */
  constrainToViewport?: boolean;
}

export function useAnchoredPosition(
  triggerRef: RefObject<HTMLElement | null>,
  menuRef: RefObject<HTMLElement | null>,
  open: boolean,
  { align = 'start', gap = 4, margin = 4, constrainToViewport = false }: UseAnchoredPositionOptions = {},
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

      if (constrainToViewport) {
        // The natural content height, ignoring the cap this hook applied
        // on the previous pass: `scrollHeight` reports the content height
        // even while the box is shorter, and the border sits outside the
        // border-box height the cap applies to. The width is left to the
        // menu's own styles — a caller that caps it more tightly than the
        // viewport keeps that cap.
        const computed = window.getComputedStyle(menu);
        const border = (parseFloat(computed.borderTopWidth) || 0) + (parseFloat(computed.borderBottomWidth) || 0);
        const naturalHeight = menu.scrollHeight + border;

        const preferredLeft = align === 'end' ? triggerRect.right - menuRect.width : triggerRect.left;
        const alternateLeft = align === 'end' ? triggerRect.left : triggerRect.right - menuRect.width;
        const fit = fitPanelToBounds({
          size: { width: menuRect.width, height: naturalHeight },
          preferred: { left: preferredLeft, top: triggerRect.bottom + gap },
          alternate: { left: alternateLeft, bottom: triggerRect.top - gap },
          bounds: { width: window.innerWidth, height: window.innerHeight },
          margin,
        });

        menu.style.visibility = '';
        menu.style.left = `${fit.left}px`;
        menu.style.top = `${fit.top}px`;
        menu.style.maxHeight = `${fit.maxHeight}px`;
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
      if (constrainToViewport) {
        menu.style.maxHeight = '';
      }
    };
  }, [align, constrainToViewport, gap, margin, menuRef, open, triggerRef]);
}
