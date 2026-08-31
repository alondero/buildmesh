/**
 * Module-level ref to the grid search <input> (issue #998).
 *
 * Why a module-level singleton rather than a Zustand store field or a
 * React context? Two reasons:
 *
 *   1. The "focus this element" gesture is a side-effect, not state. The
 *      store tracks what the input contains (`uiStore.gridSearchQuery`),
 *      not the DOM node. Adding a `gridSearchInputElement: HTMLInputElement
 *      | null` field to `uiStore` would force React re-renders on every
 *      mount/unmount, just to keep a stale `HTMLElement` reference.
 *
 *   2. The caller (App.tsx's Tauri-shortcut dispatch handler) lives
 *      outside the component tree. App.tsx isn't a parent of the input,
 *      so a `useImperativeHandle`/`forwardRef` chain would need a
 *      context provider, an effect, and a ref forwarded through any
 *      wrapper the View Header adds. A module-level `let` is the
 *      lighter-weight alternative — a single ref, three free functions,
 *      no provider.
 *
 * The input registers itself in a layout effect (synchronous, before the
 * browser paints, so the first ⌘+F from a cold load can't land before
 * the ref is set). On unmount it clears the registration, so a remount
 * of the View Header (mesh change, view-mode switch) doesn't leave a
 * stale detached node that `.focus()` would target.
 *
 * The test seam (`__resetGridSearchInputForTests`) lets unit tests
 * clear the singleton between cases without standing up a full
 * component mount cycle. The double-underscore `__resetXyzForTests`
 * pattern is the same one `lib/updater.ts` uses for its identifier
 * cache (`__resetIdentifierCacheForTests`).
 */
let registeredInput: HTMLInputElement | null = null;

/**
 * Register the live search input. Called by `GridControls` in a layout
 * effect on mount and clear-on-unmount. Replaces any prior registration
 * — there is only ever one grid search input in the app.
 */
export function registerGridSearchInput(el: HTMLInputElement | null): void {
  registeredInput = el;
}

/**
 * Focus the grid search input, if it is currently registered. Returns
 * `true` when focus was moved, `false` when no input is in the tree
 * (e.g. the View Header is unmounted in a view mode that hides it).
 * The return value is for test assertions and is not currently read by
 * the production caller — but keeping it lets future callers branch on
 * "the chord landed but the target was missing" if that ever matters.
 */
export function focusGridSearch(): boolean {
  if (registeredInput === null) return false;
  // `.focus()` is safe on a detached node (it throws nothing, it just
  // does nothing), but skipping it on a detached input is cheap and
  // makes the intent ("focus the visible input") clearer than relying
  // on the no-op.
  if (!registeredInput.isConnected) {
    registeredInput = null;
    return false;
  }
  registeredInput.focus();
  return true;
}

/**
 * Test-only seam: clear the singleton between vitest cases. Mirrors
 * `useSidebarResize`'s `__resetSidebarResizeForTests`. Not exported
 * from any public barrel — the `__` prefix is the "don't import from
 * app code" signal.
 */
export function __resetGridSearchInputForTests(): void {
  registeredInput = null;
}
