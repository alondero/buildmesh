/**
 * useAriaMenu — WAI-ARIA menu keyboard contract + auto-focus on open.
 *
 * Issue #837 — extracted from three call sites where the same Escape /
 * Tab / ArrowDown / ArrowUp / Home / End handler + roving-tabindex +
 * focus-first-on-open logic was duplicated:
 *
 *   - `src/components/Providers/GroupedProviderMenu.tsx` (#814)
 *   - `src/components/BuildRun/BuildRunDropdown.tsx` (#814)
 *   - `src/components/Sidebar/MeshItem.tsx` (#735, the third copy)
 *
 * Per the WAI-ARIA `menu` role (non-modal popover):
 *
 *   - **ArrowDown / ArrowUp** — move focus to the next / previous
 *     menuitem, wrapping at the ends.
 *   - **Home / End** — jump to the first / last menuitem.
 *   - **Tab / Shift+Tab** — leave the menu and close it (no focus trap).
 *   - **Escape** — close the menu. If the caller's `onClose` needs to
 *     return focus to a trigger, that's the caller's job (the hook
 *     only fires the callback). `BuildRunDropdown` and `MeshItem` use
 *     `requestAnimationFrame` to wait for the unmount before calling
 *     `trigger.focus()`; `GroupedProviderMenu` defers to its parent's
 *     `onClose` because the parent owns the open boolean.
 *   - **Focus-gate** — keystrokes only apply while focus is inside
 *     `rootRef`. We check `document.activeElement` (not `e.target`)
 *     because in jsdom events dispatch on `document` while focus is on
 *     a menuitem, and a real browser can deliver the keydown to the
 *     focused element while the listener is on `document`.
 *
 * The `enabled` gate lets a caller mount the menu (and run the auto-
 * focus layout effect) without attaching the global keydown listener.
 * `BuildRunDropdown` and `MeshItem` want the listener only while the
 * menu is open, so they pass `enabled={isOpen}`. `GroupedProviderMenu`
 * keeps the listener mounted for the component's lifetime (the menu is
 * only rendered while open, so unmount tears it down anyway); the hook
 * matches that with `enabled` defaulting to `true`.
 *
 * The keyboard handler reads `itemCount`, `activeIndex`, and `onClose`
 * from refs so a long-lived listener sees the live values without re-
 * attaching on every state flip. Mirrors the `itemCountRef` pattern in
 * `MeshItem.tsx:147-148` (issue #735).
 *
 * Out of scope
 * ------------
 * The outside-mousedown close path is already shared via `useClickOutside`
 * (#492). Viewport-clamping for CSS-anchored menus lives in
 * `useViewportClamp`; trigger-relative fixed menus use
 * `useAnchoredPosition`. Callers with nested submenus can scope
 * `itemSelector` to their own items, and `skipDisabled` keeps roving focus
 * on actionable entries.
 */
import { useEffect, useLayoutEffect, useRef, type RefObject } from 'react';

export interface UseAriaMenuOptions {
  /** Ref to the menu root. The focus-gate reads `rootRef.current.contains(activeElement)`. */
  rootRef: RefObject<HTMLElement | null>;
  /** Optional number of menuitems. When omitted, the rendered item selector is authoritative. */
  itemCount?: number;
  /** Current roving-tabindex position. */
  activeIndex: number;
  /** Setter for the roving-tabindex position. */
  setActiveIndex: (next: number) => void;
  /** Close handler — called on Escape (and Tab if `closeOnTab` is true). */
  onClose: () => void;
  /**
   * Close on Tab. Default `true` (WAI-ARIA `menu` is a non-modal popover —
   * Tab leaves and closes; `closeOnTab: false` is reserved for a future
   * modal variant).
   */
  closeOnTab?: boolean;
  /**
   * Gate the keydown listener AND the auto-focus layout effect. When
   * `false`, neither side-effect runs. Default `true`.
   */
  enabled?: boolean;
  /** Selector for this menu's own items when it contains a nested menu. */
  itemSelector?: string;
  /** Skip disabled menuitems while moving focus. */
  skipDisabled?: boolean;
}

export function useAriaMenu({
  rootRef,
  itemCount,
  activeIndex,
  setActiveIndex,
  onClose,
  closeOnTab = true,
  enabled = true,
  itemSelector = '[role="menuitem"]',
  skipDisabled = false,
}: UseAriaMenuOptions): void {
  // Mirror state into refs so the document-level listener (attached on
  // mount, torn down on unmount) always sees the LIVE values without
  // re-attaching on every keystroke. Without this, the closure would
  // freeze at the value current when the menu first mounted and a
  // later active-index flip wouldn't move focus.
  const itemCountRef = useRef(itemCount);
  itemCountRef.current = itemCount;
  const activeIndexRef = useRef(activeIndex);
  activeIndexRef.current = activeIndex;
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    if (!enabled) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      const root = rootRef.current;
      if (!root) return;
      // WAI-ARIA focus-gate: keystrokes only apply while focus is inside
      // the menu. `document.activeElement` (not `e.target`) — in jsdom
      // events dispatch on `document` while focus is on a menuitem, and
      // a real browser can deliver the keydown to the focused element
      // while the listener is on `document`.
      const active = document.activeElement;
      if (!(active instanceof Node) || !root.contains(active)) return;

      const total = itemCountRef.current ?? root.querySelectorAll<HTMLElement>(itemSelector).length;
      if (total === 0) return;

      if (e.key === 'Escape') {
        e.preventDefault();
        onCloseRef.current();
        return;
      }
      if (closeOnTab && e.key === 'Tab') {
        // Non-modal popover: Tab leaves the menu and closes it. Don't
        // preventDefault — let the browser move focus naturally.
        onCloseRef.current();
        return;
      }
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        const next = focusMenuItem(root, (activeIndexRef.current + 1) % total, itemSelector, skipDisabled, 1);
        if (next !== null) setActiveIndex(next);
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        const next = focusMenuItem(root, (activeIndexRef.current - 1 + total) % total, itemSelector, skipDisabled, -1);
        if (next !== null) setActiveIndex(next);
        return;
      }
      if (e.key === 'Home') {
        e.preventDefault();
        const next = focusMenuItem(root, 0, itemSelector, skipDisabled, 1);
        if (next !== null) setActiveIndex(next);
        return;
      }
      if (e.key === 'End') {
        e.preventDefault();
        const last = total - 1;
        const next = focusMenuItem(root, last, itemSelector, skipDisabled, -1);
        if (next !== null) setActiveIndex(next);
        return;
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('keydown', handleKeyDown);
    };
    // The listener attachment is keyed only on `enabled` and `closeOnTab`.
    // `rootRef` and the state setters are stable across renders.
  }, [enabled, closeOnTab, rootRef, itemSelector, skipDisabled]);

  // Auto-focus the first menuitem on mount (WAI-ARIA menu contract).
  // `useLayoutEffect` (not `useEffect`) — the layout effect fires
  // synchronously after the menu commits, so the very first ArrowDown
  // doesn't race a deferred focus call clobbering the user's keystroke.
  useLayoutEffect(() => {
    if (!enabled) return;
    const root = rootRef.current;
    if (!root) return;
    // Reset to the first item whenever the menu (re)mounts. Set the
    // index synchronously so the roving tabindex render passes `0` to
    // the first menuitem before paint.
    const first = focusMenuItem(root, 0, itemSelector, skipDisabled, 1);
    if (first !== null) setActiveIndex(first);
    // Re-run only when `enabled` flips — the menu's open/close is the
    // gate, and the layout effect mirrors the lifetime of the listener.
  }, [enabled, rootRef, setActiveIndex, itemSelector, skipDisabled]);
}

/**
 * Move focus to the n-th `[role="menuitem"]` inside `root`. Uses
 * `querySelectorAll` rather than a ref array so a re-rendered button
 * (e.g. after a Proxied filter change in `GroupedProviderMenu`) is
 * always reachable, even if a caller-side ref array went stale. The
 * selector matches the WAI-ARIA `menuitem` role; the per-component
 * roving tabindex attributes are left to the component.
 */
function focusMenuItem(root: HTMLElement, index: number, itemSelector: string, skipDisabled: boolean, direction: 1 | -1): number | null {
  const all = root.querySelectorAll<HTMLElement>(itemSelector);
  if (all.length === 0) return null;
  for (let step = 0; step < all.length; step += 1) {
    const candidateIndex = (index + direction * step + all.length) % all.length;
    const candidate = all[candidateIndex];
    const disabled = candidate instanceof HTMLButtonElement && candidate.disabled;
    if (skipDisabled && (disabled || candidate.getAttribute('aria-disabled') === 'true')) continue;
    candidate.focus();
    return candidateIndex;
  }
  return null;
}
