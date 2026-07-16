import { useState, useRef, useEffect, useLayoutEffect, useId } from 'react';
import { AgentNode } from '../../stores/agentNodeStore';
import { useClickOutside } from '../../hooks/useClickOutside';

interface BuildRunDropdownProps {
  node: AgentNode;
  onBuildRun: (nodeId: number, mode: 'build' | 'run' | 'terminal') => void;
}

export function BuildRunDropdown({ node, onBuildRun }: BuildRunDropdownProps) {
  const [isOpen, setIsOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  // The Build/Run/Terminal menu has a fixed 3-item order; index 0 = Build,
  // 1 = Run, 2 = Terminal. The refs array is filled as the buttons mount
  // and read by the keydown handler when an arrow key moves focus.
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([null, null, null]);

  // Issue #814 — consolidated close-on-outside-click via the shared
  // `useClickOutside` hook (issue #492). The hook attaches a document-
  // level `mousedown` listener while `open !== null`; the listener uses
  // the scoped `[data-dropdown-for="<value>"]` selector so a click on a
  // *different* dropdown's body doesn't close this one. Scoping by
  // `node.id` (not a boolean) matters in the grid — multiple
  // `BuildRunDropdown`s render simultaneously (one per agent node),
  // so a click on a sibling's body must close *this* one.
  useClickOutside<number>(isOpen ? node.id : null, () => setIsOpen(false));

  // Issue #814 — Escape closes the menu and returns focus to the trigger.
  // The listener is attached only while open (gated on `isOpen`) so the
  // page-level Escape binding (e.g. modal dismiss) isn't shadowed when
  // the menu is closed. The handler reads `document.activeElement` (not
  // `e.target`) for the same reason as `MeshItem`'s menu: in jsdom tests
  // events dispatch on `document` while focus is on a menuitem.
  useEffect(() => {
    if (!isOpen) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      const menu = menuRef.current;
      const active = document.activeElement;
      // Only react if focus is in this menu — avoids hijacking Escape /
      // ArrowUp / ArrowDown typed elsewhere on the page.
      if (menu && active instanceof Node && !menu.contains(active)) return;
      if (e.key === 'Escape') {
        e.preventDefault();
        const trigger = triggerRef.current;
        setIsOpen(false);
        // Return focus to the trigger so the keyboard user lands somewhere
        // predictable. `requestAnimationFrame` waits for the unmount so
        // the trigger ref is still attached when focus() lands.
        requestAnimationFrame(() => trigger?.focus());
        return;
      }
      if (e.key === 'Tab') {
        // WAI-ARIA menu: Tab leaves the menu and closes it. Don't
        // preventDefault — let the browser move focus naturally.
        setIsOpen(false);
        return;
      }
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setActiveIndex((i) => {
          const next = (i + 1) % itemRefs.current.length;
          itemRefs.current[next]?.focus();
          return next;
        });
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        setActiveIndex((i) => {
          const next = (i - 1 + itemRefs.current.length) % itemRefs.current.length;
          itemRefs.current[next]?.focus();
          return next;
        });
        return;
      }
      if (e.key === 'Home') {
        e.preventDefault();
        setActiveIndex(0);
        itemRefs.current[0]?.focus();
        return;
      }
      if (e.key === 'End') {
        e.preventDefault();
        const last = itemRefs.current.length - 1;
        setActiveIndex(last);
        itemRefs.current[last]?.focus();
        return;
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [isOpen]);

  // Issue #814 — viewport clamping (flip-up when the menu would overflow
  // the bottom of the viewport). Mirrors the pattern at
  // `GridNodeHeader.tsx:439` (kebab menu). `useLayoutEffect` runs BEFORE
  // the browser paints so the user never sees the unclamped position.
  // `right-0 top-full mt-1` anchoring is preserved; only a `translateY`
  // offset is applied, so the open animation (`animate-scale-in
  // origin-top-right`) still plays cleanly.
  //
  // The shift cap is `rect.top - MARGIN` (the space above the menu's
  // rendered top), NOT `rect.top - rect.height - MARGIN` — subtracting
  // the menu's own height would under-cap and leave the menu still
  // overflowing when the menu is taller than the gap between the
  // trigger and the viewport's top.
  useLayoutEffect(() => {
    if (!isOpen) return;
    const menu = menuRef.current;
    if (!menu) return;
    const rect = menu.getBoundingClientRect();
    const vh = window.innerHeight;
    const MARGIN = 4;
    const overflow = rect.bottom - (vh - MARGIN);
    if (overflow <= 0) return;
    const maxShift = Math.max(0, rect.top - MARGIN);
    const shift = Math.min(overflow, maxShift);
    if (shift <= 0) return;
    menu.style.transform = `translateY(-${shift}px)`;
    return () => {
      menu.style.transform = '';
    };
  }, [isOpen]);

  // Issue #814 — on open, reset the roving index to the first item and
  // move focus into the menu so subsequent arrow keys work. Mirrors the
  // pattern at `MeshItem.tsx:284` (mesh context menu).
  useLayoutEffect(() => {
    if (!isOpen) return;
    setActiveIndex(0);
    itemRefs.current[0]?.focus();
  }, [isOpen]);

  const handleBuild = async () => {
    setIsOpen(false);
    onBuildRun(node.id, 'build');
  };

  const handleRun = async () => {
    setIsOpen(false);
    onBuildRun(node.id, 'run');
  };

  const handleTerminal = async () => {
    setIsOpen(false);
    onBuildRun(node.id, 'terminal');
  };

  // Issue #814 — stable id for the menu-button disclosure pattern.
  // `useId` gives each `BuildRunDropdown` instance (one per agent node
  // in the grid) a distinct id, so a screen reader walking the page
  // can match trigger → menu without ambiguity.
  const menuId = useId();

  return (
    <div className="relative">
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setIsOpen(!isOpen)}
        aria-haspopup="menu"
        aria-expanded={isOpen}
        aria-controls={isOpen ? menuId : undefined}
        aria-label="Open build menu"
        title="Build / Run / Terminal"
        // Icon-only trigger — saves ~34 px vs the former "Build ▼" pill,
        // and reads as a peer of the close + expand buttons because all
        // three share `h-7` + bg-bg-base/60 + border-border-default with no
        // shadow, so the trio looks like one control group against the
        // mesh-tinted header.
        className="flex items-center gap-0.5 h-7 px-1.5 rounded-md bg-bg-base/60 border border-border-default text-accent-cyan hover:bg-accent-cyan/15 hover:border-accent-cyan/60 transition-colors"
      >
        {/* Wrench — "build / tools" semantic, the common IDE shorthand for
            the "execute build or run" family. 12 px matches the close /
            expand icon sizes so the three controls are visually balanced. */}
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" />
        </svg>
        <svg width="8" height="8" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" className="opacity-70">
          <path d="M6 9l6 6 6-6" />
        </svg>
      </button>

      {isOpen && (
        // Issue #814 — `role="menu"` + `aria-label` complete the WAI-ARIA
        // menu contract (item role + keyboard nav lives below). The menu
        // id is wired to the trigger's `aria-controls` for screen-reader
        // navigation. Viewport clamp applies a transient `translateY` so
        // the menu doesn't overflow the bottom edge of the window when
        // the trigger sits low on the page.
        //
        // `data-dropdown-for={node.id}` is the scoped attribute the
        // `useClickOutside` hook reads; `node.id` (not a boolean) keeps
        // sibling BuildRunDropdowns (one per agent node in the grid) from
        // treating each other's bodies as "inside" and never closing.
        <div
          ref={menuRef}
          id={menuId}
          data-dropdown-for={node.id}
          role="menu"
          aria-label="Build, run, or open a terminal"
          className="absolute right-0 top-full mt-1 w-44 bg-bg-card border border-border-default rounded-md shadow-md z-50 animate-scale-in origin-top-right"
        >
          <button
            ref={(el) => { itemRefs.current[0] = el; }}
            role="menuitem"
            tabIndex={activeIndex === 0 ? 0 : -1}
            onClick={handleBuild}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            {node.use_worktree ? 'Build from worktree' : 'Build'}
          </button>
          <button
            ref={(el) => { itemRefs.current[1] = el; }}
            role="menuitem"
            tabIndex={activeIndex === 1 ? 0 : -1}
            onClick={handleRun}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            {node.use_worktree ? 'Run from worktree' : 'Run'}
          </button>
          <div className="my-1 border-t border-border-default" />
          <button
            ref={(el) => { itemRefs.current[2] = el; }}
            role="menuitem"
            tabIndex={activeIndex === 2 ? 0 : -1}
            onClick={handleTerminal}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            {node.use_worktree ? 'Terminal in worktree' : 'Terminal'}
          </button>
        </div>
      )}
    </div>
  );
}