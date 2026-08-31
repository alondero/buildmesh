/**
 * Tests for the grid search focus singleton (issue #998).
 *
 * The singleton in `src/lib/gridSearchFocus.ts` holds a module-level ref
 * to the live search <input>. The `GridControls` component registers on
 * mount; App.tsx's `focus-grid-search` shortcut handler calls
 * `focusGridSearch()` to invoke `.focus()` on the registered element.
 *
 * This file pins the singleton's contract — the contract the App.tsx
 * handler and the GridControls component both depend on, with no shared
 * types or React-tree context to lean on:
 *
 *   - `registerGridSearchInput(el)` is idempotent: a fresh mount replaces
 *     the prior registration (only ever one input in the tree).
 *   - `focusGridSearch()` returns `true` and focuses the input when one
 *     is registered, `false` when none is (the View Header is unmounted).
 *   - A detached registered input (the View Header unmounted without
 *     unmounting the component — shouldn't happen in practice, but
 *     happens in StrictMode double-mount races) is auto-cleared so
 *     `.focus()` doesn't silently target a detached node.
 *   - Passing `null` clears the registration (the unmount path).
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import {
  registerGridSearchInput,
  focusGridSearch,
  __resetGridSearchInputForTests,
} from '../../src/lib/gridSearchFocus';

describe('grid search focus singleton (issue #998)', () => {
  beforeEach(() => {
    __resetGridSearchInputForTests();
  });

  it('returns false when no input has been registered', () => {
    // The View Header is unmounted (e.g. the app is showing the splash
    // before a mesh is loaded). The shortcut must not throw — it just
    // lands as a no-op, and the user gets no visible focus change.
    expect(focusGridSearch()).toBe(false);
  });

  it('returns true and focuses the registered input', () => {
    // jsdom doesn't implement layout, so the focus call doesn't actually
    // move `document.activeElement`. We use a stand-in DOM element with
    // a `.focus()` spy to confirm the singleton *called* the method —
    // which is the only observable behaviour the App.tsx handler can
    // depend on.
    const focus = vi.fn();
    const input = { focus, isConnected: true } as unknown as HTMLInputElement;
    registerGridSearchInput(input);

    expect(focusGridSearch()).toBe(true);
    expect(focus).toHaveBeenCalledOnce();
  });

  it('replaces a prior registration on re-mount', () => {
    // StrictMode mounts components twice in dev. The first mount's
    // layout effect calls `registerGridSearchInput(elA)`; the cleanup
    // runs and calls `registerGridSearchInput(null)`; the second mount
    // calls `registerGridSearchInput(elB)`. The end state should
    // target `elB`, not `elA`. This test pins the idempotency contract
    // by registering two inputs back-to-back without an explicit
    // `null` clear in between.
    const focusA = vi.fn();
    const focusB = vi.fn();
    const elA = { focus: focusA, isConnected: true } as unknown as HTMLInputElement;
    const elB = { focus: focusB, isConnected: true } as unknown as HTMLInputElement;

    registerGridSearchInput(elA);
    registerGridSearchInput(elB);

    expect(focusGridSearch()).toBe(true);
    expect(focusA).not.toHaveBeenCalled();
    expect(focusB).toHaveBeenCalledOnce();
  });

  it('clears the registration when called with null (unmount path)', () => {
    const focus = vi.fn();
    const input = { focus, isConnected: true } as unknown as HTMLInputElement;
    registerGridSearchInput(input);
    registerGridSearchInput(null);

    expect(focusGridSearch()).toBe(false);
    expect(focus).not.toHaveBeenCalled();
  });

  it('auto-clears a detached registered input and returns false', () => {
    // Belt-and-braces for a path the GridControls component shouldn't
    // hit: the input was registered, the element was removed from the
    // DOM (parent unmounted) without the component's cleanup effect
    // running. Without the auto-clear, a subsequent `focusGridSearch()`
    // would `.focus()` a detached node — jsdom silently no-ops, real
    // browsers do too, but the singleton would carry a stale ref that
    // future mounts would skip over. Clear and return false so the
    // next `registerGridSearchInput()` lands cleanly.
    const focus = vi.fn();
    const detached = { focus, isConnected: false } as unknown as HTMLInputElement;
    registerGridSearchInput(detached);

    expect(focusGridSearch()).toBe(false);
    expect(focus).not.toHaveBeenCalled();

    // After the auto-clear, a new registration must work.
    const live = { focus: vi.fn(), isConnected: true } as unknown as HTMLInputElement;
    registerGridSearchInput(live);
    expect(focusGridSearch()).toBe(true);
  });
});
