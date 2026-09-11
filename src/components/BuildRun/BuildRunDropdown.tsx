import { useState, useRef, useId } from 'react';
import { createPortal } from 'react-dom';
import { AgentNode } from '../../stores/agentNodeStore';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useAriaMenu } from '../../hooks/useAriaMenu';
import { useAnchoredPosition } from '../../hooks/useAnchoredPosition';
import { dropdownId } from '../../lib/dropdownId';

interface BuildRunDropdownProps {
  node: AgentNode;
  onBuildRun: (nodeId: number, mode: 'build' | 'run' | 'terminal') => void;
}

export function BuildRunDropdown({ node, onBuildRun }: BuildRunDropdownProps) {
  const [isOpen, setIsOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  // Build/Run/Terminal is a fixed 3-item menu (index 0 = Build, 1 = Run,
  // 2 = Terminal). The `useAriaMenu` hook reads this count directly and
  // walks menuitems via `querySelectorAll('[role="menuitem"]')`, so
  // there is no need for an `itemRefs` array here (the pre-#837 version
  // owned one for O(1) focus targeting that the hook now does via the
  // DOM walk).

  // Issue #814 — consolidated close-on-outside-click via the shared
  // `useClickOutside` hook (issue #492). The hook attaches a document-
  // level `mousedown` listener while `open !== null`; the listener uses
  // the scoped `[data-dropdown-for="<value>"]` selector so a click on a
  // *different* dropdown's body doesn't close this one. Scoping by
  // `node.id` (not a boolean) matters in the grid — multiple
  // `BuildRunDropdown`s render simultaneously (one per agent node),
  // so a click on a sibling's body must close *this* one.
  //
  // The menu is portaled to `document.body` (see below), so the trigger
  // AND the popup both carry the attribute — same dual placement the
  // portaled kebab uses in `GridNodeHeader` (issue #814). The portal
  // moves the menu out of the wrapper's subtree, so a click inside the
  // portaled menu would otherwise read as "outside".
  useClickOutside<string>(isOpen ? dropdownId('buildrun', node.id) : null, () => setIsOpen(false));

  // Issue #814 / #837 — Escape closes the menu and returns focus to the
  // trigger. The `useAriaMenu` hook attaches the document-level keydown
  // listener only while `enabled` is true (gated on `isOpen`), so the
  // page-level Escape binding (e.g. modal dismiss) isn't shadowed when
  // the menu is closed. The hook fires `onClose` on Escape; this closure
  // flips `isOpen` and uses `requestAnimationFrame` so the trigger ref
  // is still attached when `focus()` lands (the rAF runs after the
  // unmount).
  const closeAndReturnFocus = () => {
    const trigger = triggerRef.current;
    setIsOpen(false);
    requestAnimationFrame(() => trigger?.focus());
  };
  useAriaMenu({
    rootRef: menuRef,
    // Build/Run/Terminal is a fixed 3-item menu.
    itemCount: 3,
    activeIndex,
    setActiveIndex,
    onClose: closeAndReturnFocus,
    enabled: isOpen,
  });

  // The menu is portaled to `document.body` and anchored
  // with `useAnchoredPosition` `fixed` coordinates, for the same reason
  // the PR pill (#1585) and the kebab (#1589) were portaled before it:
  // the header row is `overflow-hidden` (#1650 compact title bar) and an
  // inline `absolute right-0 top-full` menu gets clipped to the ~36px
  // header strip — only one menu item was ever visible, and the
  // `useViewportClamp` shift made it worse by pulling the menu up under
  // the clip window. Fixed coordinates are viewport-scoped and immune
  // to ancestor overflow; the hook also follows the trigger on scroll
  // and clamps the menu inside the viewport (flipping above when there
  // is no room below).
  useAnchoredPosition(triggerRef, menuRef, isOpen, { align: 'end' });

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
    // Issue #814 — `data-dropdown-for={node.id}` lives on the trigger
    // (and on the portaled menu popup below). The `useClickOutside`
    // hook's selector is `[data-dropdown-for="<open>"]`, so a click on
    // the trigger is "inside" and the hook won't fire on it — without
    // that, the document mousedown would close the menu, then the
    // trigger's onClick would toggle it back open in the same tick (a
    // flicker + state race). The menu popup carries the attribute too
    // because the portal removes it from the trigger's subtree.
    // No wrapper element: the menu is portaled, so nothing needs a
    // `relative` positioning context — the header trio stays flat
    // children of the header row, matching the kebab/PR-pill peers.
    <>
      <button
        ref={triggerRef}
        data-dropdown-for={isOpen ? dropdownId('buildrun', node.id) : undefined}
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

      {isOpen && createPortal(
        // Issue #814 — `role="menu"` + `aria-label` complete the WAI-ARIA
        // menu contract (item role + keyboard nav lives below). The menu
        // id is wired to the trigger's `aria-controls` for screen-reader
        // navigation. Positioning is handled by `useAnchoredPosition`
        // (fixed, viewport-scoped, scroll-following) — see the comment
        // above the hook call.
        //
        // `data-dropdown-for` lives on the trigger (above) AND this
        // portaled popup so `useClickOutside` treats both as "inside".
        <div
          ref={menuRef}
          id={menuId}
          role="menu"
          aria-label="Build, run, or open a terminal"
          data-dropdown-for={dropdownId('buildrun', node.id)}
          // z-[100] — same stacking tier as the other portaled menus
          // from this header (PrPill, kebab, NodeActivityTabs) so a z-50
          // overlay elsewhere in document.body can never interleave.
          className="fixed bg-bg-card border border-border-default rounded-md shadow-md z-[100] animate-scale-in origin-top-right py-1 min-w-[176px]"
          style={{ top: 0, left: 0, visibility: 'hidden' }}
        >
          <button
            role="menuitem"
            tabIndex={activeIndex === 0 ? 0 : -1}
            onClick={handleBuild}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            {node.use_worktree ? 'Build from worktree' : 'Build'}
          </button>
          <button
            role="menuitem"
            tabIndex={activeIndex === 1 ? 0 : -1}
            onClick={handleRun}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            {node.use_worktree ? 'Run from worktree' : 'Run'}
          </button>
          <div className="my-1 border-t border-border-default" />
          <button
            role="menuitem"
            tabIndex={activeIndex === 2 ? 0 : -1}
            onClick={handleTerminal}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            {node.use_worktree ? 'Terminal in worktree' : 'Terminal'}
          </button>
        </div>,
        document.body,
      )}
    </>
  );
}
