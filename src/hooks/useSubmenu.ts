import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';

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

export interface UseSubmenuOptions {
  disabled: boolean;
  itemCount: number;
  /**
   * Delay, in ms, between the cursor leaving the wrapper and the
   * submenu actually closing. Cancelled on re-entry into the wrapper
   * or submenu body. Defaults to 120 ms — long enough to absorb the
   * 10–30 ms the cursor spends in the L-shaped gap between trigger
   * and submenu on a diagonal path, short enough that a real "move
   * away" still feels immediate. Set to 0 to disable (matches the
   * pre-fix behaviour). The diagonal-cursor test pins the default;
   * do not lower it without revisiting that test.
   */
  closeDelay?: number;
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
 * - Hover callers bind `onPointerOver` / `onMouseEnter` / `onMouseLeave`
 *   to the wrapper directly. The hook owns:
 *     - the #1293 arm gate (`pointerover` must precede `mouseenter`
 *       so a synchronous mount-time `mouseenter` under an existing
 *       cursor does not pop the picker);
 *     - a cancellable close delay so a diagonal cursor path from
 *       trigger into the submenu does not flash the picker shut when
 *       it crosses the L-shaped hit-area gap.
 * - `closeSubmenu` stays immediate — pickers, keyboard Escape, and
 *   `useClickOutside` outside-mousedown all want the close to land
 *   synchronously with their gesture. Only the hover-leave path goes
 *   through the delayed arm.
 */
export function useSubmenu(opts: UseSubmenuOptions) {
  const { disabled, itemCount, closeDelay = 120 } = opts;
  const [submenuOpen, setSubmenuOpen] = useState(false);
  const submenuRef = useRef<HTMLDivElement | null>(null);
  // Armed only by `openViaKeyboard`; hover opens leave it false so the
  // layout effect below doesn't yank focus on mouse users.
  const focusOnOpenRef = useRef(false);
  const openRef = useRef(submenuOpen);
  openRef.current = submenuOpen;
  // #1293 — pointerover arms, mouseenter opens only when armed.
  const armRef = useRef(false);
  // Hover-leave close is delayed and cancellable so a diagonal cursor
  // path from trigger to submenu does not flash the picker shut when
  // it crosses the L-shaped gap. Real closes (pick, Escape, outside
  // click) go through `closeSubmenu` and are always immediate.
  const closeTimerRef = useRef<number | null>(null);

  useLayoutEffect(() => {
    if (submenuOpen && focusOnOpenRef.current) {
      focusOnOpenRef.current = false;
      focusWithoutScroll(liveItems(submenuRef.current)[0]);
    }
  }, [submenuOpen]);

  const cancelPendingClose = useCallback(() => {
    if (closeTimerRef.current !== null) {
      window.clearTimeout(closeTimerRef.current);
      closeTimerRef.current = null;
    }
  }, []);

  const armCloseOnLeave = useCallback(() => {
    cancelPendingClose();
    if (closeDelay <= 0) {
      armRef.current = false;
      setSubmenuOpen(false);
      return;
    }
    closeTimerRef.current = window.setTimeout(() => {
      closeTimerRef.current = null;
      armRef.current = false;
      setSubmenuOpen(false);
    }, closeDelay);
  }, [cancelPendingClose, closeDelay]);

  const closeSubmenu = useCallback(() => {
    cancelPendingClose();
    armRef.current = false;
    setSubmenuOpen(false);
  }, [cancelPendingClose]);

  // Drop any pending close on unmount so a teardown mid-delay cannot
  // leak a setState into an unmounted component.
  useEffect(() => cancelPendingClose, [cancelPendingClose]);

  const onPointerOver = useCallback(() => {
    armRef.current = true;
  }, []);

  const onMouseEnter = useCallback(() => {
    cancelPendingClose();
    if (armRef.current && !disabled) {
      setSubmenuOpen(true);
    }
  }, [cancelPendingClose, disabled]);

  const onMouseLeave = useCallback(() => {
    armRef.current = false;
    armCloseOnLeave();
  }, [armCloseOnLeave]);

  // Bind on the submenu BODY (the picker panel) so the cursor entering
  // a row inside the picker also cancels a pending close. The
  // wrapper's own `onMouseEnter` already covers this today (entering
  // the picker means entering the wrapper, so `mouseenter` fires on
  // the wrapper and `onMouseEnter` cancels), so the body handler is
  // strictly redundant for the current geometry. It exists as a
  // defence-in-depth so the close still cancels if a future refactor
  // portals the picker outside the wrapper (`SpawnConfigurationMenu`
  // is already portalled and relies on its own click-outside / Escape
  // handling rather than this hook — but if a follow-up brings the
  // Regenerate picker into that shape, this handler stops being
  // redundant without further work).
  const onSubmenuMouseEnter = useCallback(() => {
    cancelPendingClose();
  }, [cancelPendingClose]);

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
    // Bind these to the wrapper's onPointerOver / onMouseEnter /
    // onMouseLeave and the submenu body's onMouseEnter respectively —
    // the hook owns the arm gate (#1293) and the hover-leave delay
    // (diagonal-cursor UX) so the two picker surfaces never drift.
    onPointerOver,
    onMouseEnter,
    onMouseLeave,
    onSubmenuMouseEnter,
  };
}
