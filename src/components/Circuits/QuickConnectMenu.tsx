/**
 * QuickConnectMenu — drag-to-search node menu (issue #1209).
 *
 * When a quick-connect drag releases over empty canvas, the editor
 * opens this fuzzy-search menu at the drop point. Choosing an entry
 * creates that node pre-wired from the dragged handle.
 *
 * Placement (issue #2773): the menu is an absolutely positioned child of
 * the canvas wrapper, so its coordinates are canvas-relative — not
 * viewport-relative — and it must stay inside the *visible editor* rather
 * than the window. A drop near an edge used to push the menu out of the
 * canvas, where the search box and the last result were unreachable. The
 * menu now flips above the drop point when there is no room below, shifts
 * left when the drop is near the right edge, and bounds its own height so
 * the result list scrolls inside the menu instead of running past the
 * edge of the editor. The placement is pinned by
 * `tests/unit/quick-connect-menu.test.tsx` and
 * `tests/e2e/quick-connect-fit.spec.ts`.
 */

import { useLayoutEffect, useMemo, useRef, useState, type RefObject } from 'react';
import { fuzzyFilterSpecs, categoryAccent, type NodeKindSpec } from './circuitGraphModel';
import { useEscapeKey } from '../../hooks/useEscapeKey';
import { fitPanelToBounds } from '../../lib/overlayLayout';

/** Horizontal offset from the drop point. */
const MENU_OFFSET_X = 8;
/** Vertical offset from the drop point; the menu sits just above it. */
const MENU_OFFSET_Y = -12;
/** Gutter kept between the menu and the edges of the visible editor. */
const MENU_MARGIN = 8;

interface QuickConnectMenuProps {
  /**
   * Where the drag released, in canvas-relative pixels — the same space
   * the menu is placed in. (The node the user picks is created at the
   * flow coordinates of the same point; that is the editor's business.)
   */
  position: { x: number; y: number };
  onSelect: (spec: NodeKindSpec) => void;
  onDismiss: () => void;
  /**
   * The visible canvas the menu has to stay inside. The menu is an
   * absolutely positioned child of that canvas, so this is the menu's
   * coordinate space. When omitted the menu falls back to its own
   * positioned ancestor.
   */
  boundsRef?: RefObject<HTMLElement | null>;
}

export function QuickConnectMenu({ position, onSelect, onDismiss, boundsRef }: QuickConnectMenuProps) {
  const [query, setQuery] = useState('');
  const results = useMemo(() => fuzzyFilterSpecs(query.trim()), [query]);
  const first = results[0];

  const menuRef = useRef<HTMLDivElement>(null);
  const [fit, setFit] = useState<{ left: number; top: number; maxHeight: number; maxWidth: number } | null>(null);

  // Issue #649 — Escape dismisses the menu. Routed through the shared
  // `useEscapeKey` hook so the menu (mounted after the circuit editor's
  // listener, now via the LIFO stack) claims Escape before the editor's
  // close path runs. The previous `e.stopPropagation()` on the input's
  // element-level onKeyDown *would* have worked in real browsers — the
  // original migration comment overstated the problem. The hook is
  // still the right home for this handler because (a) it removes the
  // imperative `stopPropagation` against a sibling listener (cleaner)
  // and (b) the test suite dispatches keydown to `document`, where the
  // editor's pre-PR window-level listener would have been bypassed.
  useEscapeKey(() => onDismiss());

  useLayoutEffect(() => {
    const menu = menuRef.current;
    if (!menu) return;

    // The menu's containing block is its coordinate space, so the
    // positioned ancestor is the authoritative bounds. The explicit
    // `boundsRef` wins when it resolves — it is the same element, but
    // naming it makes the contract obvious at the call site.
    const resolveContainer = (): HTMLElement | null => (
      boundsRef?.current ?? (menu.offsetParent as HTMLElement | null)
    );

    let frame: number | null = null;

    const place = () => {
      const container = resolveContainer();
      if (!container) return;
      const bounds = { width: container.clientWidth, height: container.clientHeight };
      if (bounds.width <= 0 || bounds.height <= 0) return;
      // Measure the menu's natural size with the height cap lifted, so
      // the fit sees the size the menu would take when it has all the
      // room it needs. The cap is restored before the browser paints.
      const previousMaxHeight = menu.style.maxHeight;
      menu.style.maxHeight = 'none';
      const naturalHeight = menu.offsetHeight;
      const naturalWidth = menu.offsetWidth;
      menu.style.maxHeight = previousMaxHeight;

      // A canvas narrower than the menu would push the menu's right edge
      // past the visible editor, so the menu is capped to the editor's
      // width less the gutters.
      const maxWidth = Math.max(0, bounds.width - MENU_MARGIN * 2);
      const width = Math.min(naturalWidth, maxWidth);

      const next = fitPanelToBounds({
        size: { width, height: naturalHeight },
        preferred: {
          left: position.x + MENU_OFFSET_X,
          top: position.y + MENU_OFFSET_Y,
        },
        alternate: {
          left: position.x + MENU_OFFSET_X - width,
          bottom: position.y + MENU_OFFSET_Y,
        },
        bounds,
        margin: MENU_MARGIN,
      });

      setFit((previous) => (
        previous
        && previous.left === next.left
        && previous.top === next.top
        && previous.maxHeight === next.maxHeight
        && previous.maxWidth === maxWidth
          ? previous
          : { left: next.left, top: next.top, maxHeight: next.maxHeight, maxWidth }
      ));
    };

    const schedule = () => {
      if (frame !== null) return;
      frame = window.requestAnimationFrame(() => {
        frame = null;
        place();
      });
    };

    place();
    // The container may not be attached yet when the menu mounts into a
    // freshly mounted canvas, so one retry is scheduled for the next
    // frame rather than leaving the menu unplaced.
    if (!resolveContainer()) schedule();

    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    const Observer = typeof ResizeObserver === 'undefined' ? null : ResizeObserver;
    const observer = Observer ? new Observer(place) : null;
    observer?.observe(menu);
    return () => {
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
      if (frame !== null) window.cancelAnimationFrame(frame);
      observer?.disconnect();
    };
  }, [boundsRef, position.x, position.y]);

  return (
    <div
      ref={menuRef}
      data-testid="quick-connect-menu"
      className="absolute z-30 flex w-52 flex-col overflow-hidden rounded-md border border-border-default bg-bg-overlay shadow-xl p-1"
      style={{
        left: fit?.left ?? position.x + MENU_OFFSET_X,
        top: fit?.top ?? position.y + MENU_OFFSET_Y,
        maxHeight: fit?.maxHeight,
        maxWidth: fit?.maxWidth,
      }}
    >
      <input
        autoFocus
        value={query}
        placeholder="Search nodes…"
        aria-label="Search nodes"
        data-testid="quick-connect-input"
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && first) {
            e.stopPropagation();
            onSelect(first);
          }
        }}
        className="w-full mb-1 shrink-0 px-1.5 py-0.5 bg-bg-input border border-border-subtle rounded-sm text-xs text-text-primary focus:outline-none focus:border-accent-cyan"
      />
      {/* The list owns the menu's scrolling: `flex-[1_1_auto]` lets it
          shrink with the menu's height bound while keeping its content
          height as the starting size, and `min-h-0` allows it to shrink
          below that content. */}
      <ul className="flex-[1_1_auto] min-h-0 max-h-48 overflow-y-auto">
        {results.map((spec) => {
          const accent = categoryAccent(spec.category);
          return (
            <li key={spec.discriminator}>
              <button
                type="button"
                data-testid={`quick-connect-${spec.discriminator}`}
                onClick={() => onSelect(spec)}
                className={`w-full text-left px-1.5 py-0.5 rounded-sm text-xs ${accent.text} hover:bg-bg-card-hover`}
              >
                {spec.label}
              </button>
            </li>
          );
        })}
        {results.length === 0 && <li className="px-1.5 py-1 text-2xs text-text-muted">No matches</li>}
      </ul>
    </div>
  );
}
