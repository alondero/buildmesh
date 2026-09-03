import { useCallback, useLayoutEffect, useRef, useState } from 'react';

/**
 * Focus a menuitem without scrolling overflow ancestors. The sidebar
 * list and the node grid both scroll under open menus; a bare
 * `.focus()` scrolls the ancestor to the item's layout box and the
 * menu jumps.
 */
export function focusWithoutScroll(el: HTMLElement | null | undefined) {
  el?.focus({ preventScroll: true });
}

const SUBMENU_ITEM_SELECTOR = 'button[role="menuitem"]';

/** Live DOM query for the submenu rows in render order. Queried fresh on
 *  every use (rather than mirrored into a ref array on mount) so a
 *  re-render while open — e.g. the provider list landing mid-open —
 *  can never leave stale elements behind. */
function liveItems(container: HTMLElement | null): HTMLButtonElement[] {
  if (!container) return [];
  return Array.from(container.querySelectorAll<HTMLButtonElement>(SUBMENU_ITEM_SELECTOR));
}

/**
 * Issue #1502 — shared hover/click picker-submenu state + keyboard
 * contract (WAI-ARIA menu-with-menubutton pattern), used by the sidebar
 * `NodeItem` context menu and the header kebab submenu alike so the two
 * never diverge again.
 *
 * - `openViaKeyboard` opens AND moves focus to the first row. Focus lands
 *   in a layout effect (post-commit, container mounted — deterministic),
 *   never in a `queueMicrotask` racing the React commit.
 * - `step` walks the rows with wrap-around. An unfocused start (`-1`,
 *   focus hasn't settled) goes to the first row on ArrowDown and the
 *   last row on ArrowUp — never the middle.
 * - Hover callers use `setSubmenuOpen` directly, which deliberately does
 *   NOT steal focus.
 */
export function useSubmenu(opts: { disabled: boolean; itemCount: number }) {
  const { disabled, itemCount } = opts;
  const [submenuOpen, setSubmenuOpen] = useState(false);
  const submenuRef = useRef<HTMLDivElement | null>(null);
  // Armed only by `openViaKeyboard`; hover opens leave it false so the
  // layout effect below doesn't yank focus on mouse users.
  const focusOnOpenRef = useRef(false);
  const openRef = useRef(submenuOpen);
  openRef.current = submenuOpen;

  useLayoutEffect(() => {
    if (submenuOpen && focusOnOpenRef.current) {
      focusOnOpenRef.current = false;
      focusWithoutScroll(liveItems(submenuRef.current)[0]);
    }
  }, [submenuOpen]);

  const openSubmenuViaKeyboard = useCallback(() => {
    if (disabled || itemCount === 0) return;
    if (openRef.current) {
      // Already open (e.g. hovered first): focus is a plain DOM query —
      // the container is mounted, so no commit to race.
      focusWithoutScroll(liveItems(submenuRef.current)[0]);
      return;
    }
    focusOnOpenRef.current = true;
    setSubmenuOpen(true);
  }, [disabled, itemCount]);

  const closeSubmenu = useCallback(() => {
    focusOnOpenRef.current = false;
    setSubmenuOpen(false);
  }, []);

  // Live open-state read for key handlers. The boolean itself can't sit
  // in a document-listener dep list (hover toggles would churn the
  // subscription); this stable callback reads the ref instead, so
  // ArrowLeft can check "is the picker actually open" for free.
  const isSubmenuOpen = useCallback((): boolean => openRef.current, []);

  const stepSubmenuFocus = useCallback((dir: 1 | -1) => {
    const items = liveItems(submenuRef.current);
    if (items.length === 0) return;
    const current = items.findIndex((el) => el === document.activeElement);
    const next =
      current === -1
        ? dir === 1
          ? 0
          : items.length - 1
        : (current + dir + items.length) % items.length;
    focusWithoutScroll(items[next]);
  }, []);

  const submenuContainsFocus = useCallback((): boolean => {
    const container = submenuRef.current;
    const active = document.activeElement;
    return !!container && active instanceof Node && container.contains(active);
  }, []);

  return {
    submenuOpen,
    setSubmenuOpen,
    closeSubmenu,
    isSubmenuOpen,
    openSubmenuViaKeyboard,
    submenuRef,
    stepSubmenuFocus,
    submenuContainsFocus,
  };
}
