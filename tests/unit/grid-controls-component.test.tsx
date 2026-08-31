/**
 * Tests for the GridControls component (issue #998).
 *
 * The component is the minimum View-Header surface #998 needs: a
 * controlled search input bound to `uiStore.gridSearchQuery`, plus a
 * clear button, an Esc handler, and a subscription to
 * `uiStore.focusGridSearchRequest` (the counter that App.tsx bumps on
 * every `focus-grid-search` Tauri global-shortcut press). The filter
 * popover, sort selector, badges, and reset button called out in #997
 * are intentionally absent here.
 *
 * What this file pins:
 *
 *   - The input renders with the live `gridSearchQuery` value and
 *     reflects keystrokes back into the store (round-trip through the
 *     `setGridSearchQuery` setter).
 *   - The clear button is hidden when the query is empty and visible
 *     when non-empty; clicking it sets the query back to ''.
 *   - The input's onKeyDown handles Escape contextually: clears the
 *     text when the query is non-empty (user can immediately type a
 *     new search; staying focused is the helpful default), and blurs
 *     the input when the query is empty (yields focus back to the
 *     canvas). Both branches call `e.preventDefault()` /
 *     `e.stopPropagation()` so the modal / Single-mode Esc handlers
 *     don't fire when the user is interacting with the search.
 *   - The `focus-grid-search` request counter subscription: bumping
 *     `useUIStore.focusGridSearchRequest` calls `.focus()` *and*
 *     `.select()` on the input (universal find behaviour — select
 *     existing text so the next keystroke replaces it). The bump is
 *     incremental (each press fires the effect), not idempotent
 *     (re-pressing while already focused still re-selects).
 *   - The mount-time cold-load case: if the counter is already > 0
 *     when the component first mounts (the user pressed ⌘+F in the
 *     window between Tauri-start and TitleBar-mount), the input
 *     focuses on mount, not on the next bump.
 *   - The WebKit / Blink native-search ✕ is suppressed via the
 *     `[&::-webkit-search-cancel-button]:appearance-none` Tailwind
 *     arbitrary variant in the className.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { act } from 'react';

import { GridControls } from '../../src/components/TitleBar/GridControls';
import { useUIStore } from '../../src/stores/uiStore';

describe('GridControls (issue #998)', () => {
  beforeEach(() => {
    // Reset every store field this component reads or writes. The
    // request counter must start at 0 so the "no focus on initial
    // mount" assertion below is meaningful; the query and clear
    // tests start fresh; splash-style fields aren't read by this
    // component.
    useUIStore.setState({
      gridSearchQuery: '',
      focusGridSearchRequest: 0,
    });
  });

  afterEach(() => {
    cleanup();
  });

  it('renders the search input wired to uiStore.gridSearchQuery', () => {
    useUIStore.setState({ gridSearchQuery: 'alpha' });
    render(<GridControls />);

    const input = screen.getByTestId('grid-search-input') as HTMLInputElement;
    expect(input.value).toBe('alpha');
    expect(input.getAttribute('placeholder')).toMatch(/search/i);
  });

  it('writes keystrokes back to uiStore.setGridSearchQuery', () => {
    render(<GridControls />);
    const input = screen.getByTestId('grid-search-input') as HTMLInputElement;

    fireEvent.change(input, { target: { value: 'beta' } });

    expect(useUIStore.getState().gridSearchQuery).toBe('beta');
  });

  it('hides the clear button when the query is empty, shows it when non-empty', () => {
    const { rerender } = render(<GridControls />);
    expect(screen.queryByTestId('grid-search-clear')).toBeNull();

    useUIStore.setState({ gridSearchQuery: 'gamma' });
    rerender(<GridControls />);
    expect(screen.getByTestId('grid-search-clear')).toBeTruthy();
  });

  it('clicking the clear button empties the query', () => {
    useUIStore.setState({ gridSearchQuery: 'gamma' });
    render(<GridControls />);

    fireEvent.click(screen.getByTestId('grid-search-clear'));

    expect(useUIStore.getState().gridSearchQuery).toBe('');
  });

  it('pressing Escape with a non-empty query clears it (stays focused for a new search)', () => {
    // Universal search-bar behaviour: the first Esc wipes what the
    // user typed so they can immediately start over. The user stays
    // focused — they didn't ask to leave the field. The second
    // Esc (or first Esc on an empty input) is the "leave" gesture
    // and is covered by the next test.
    useUIStore.setState({ gridSearchQuery: 'gamma' });
    render(<GridControls />);
    const input = screen.getByTestId('grid-search-input');

    const event = new KeyboardEvent('keydown', {
      key: 'Escape',
      bubbles: true,
      cancelable: true,
    });
    const preventDefaultSpy = vi.spyOn(event, 'preventDefault');
    const stopPropagationSpy = vi.spyOn(event, 'stopPropagation');
    fireEvent(input, event);

    expect(useUIStore.getState().gridSearchQuery).toBe('');
    expect(preventDefaultSpy).toHaveBeenCalled();
    expect(stopPropagationSpy).toHaveBeenCalled();
  });

  it('pressing Escape with an empty query blurs the input (yields focus back to the canvas)', () => {
    // PR review feedback: the previous version called
    // `e.preventDefault()` + `stopPropagation()` even on an empty
    // input, which trapped focus inside the input — the user had to
    // click out manually. The fix: on an empty query, Esc blurs
    // instead, returning focus to the canvas so the existing xterm /
    // arrow shortcuts work without an extra click.
    useUIStore.setState({ gridSearchQuery: '' });
    render(<GridControls />);
    const input = screen.getByTestId('grid-search-input') as HTMLInputElement;

    const event = new KeyboardEvent('keydown', {
      key: 'Escape',
      bubbles: true,
      cancelable: true,
    });
    const stopPropagationSpy = vi.spyOn(event, 'stopPropagation');
    const blurSpy = vi.spyOn(input, 'blur');

    fireEvent(input, event);

    expect(useUIStore.getState().gridSearchQuery).toBe('');
    expect(stopPropagationSpy).toHaveBeenCalled();
    expect(blurSpy).toHaveBeenCalledOnce();
  });

  it('bumping the focusGridSearchRequest counter focuses and selects the input', () => {
    render(<GridControls />);
    const input = screen.getByTestId('grid-search-input') as HTMLInputElement;
    const focusSpy = vi.spyOn(input, 'focus');
    const selectSpy = vi.spyOn(input, 'select');

    // The dispatch handler in App.tsx does exactly this — bump the
    // counter, let the consumer's `useLayoutEffect` see the change
    // and call `.focus()` + `.select()` on the input. Wrap in
    // `act()` so React flushes the store update + re-render + layout
    // effect before the assertion; the action is a sync Zustand
    // write but the re-render is async, so without `act()` the spy
    // would be called *after* the expect.
    act(() => {
      useUIStore.getState().requestFocusGridSearch();
    });

    expect(focusSpy).toHaveBeenCalledOnce();
    // `.select()` is the universal "find" behaviour: select existing
    // text so the next keystroke replaces it. Without it the user
    // lands at end-of-field (WebKit) or start (Blink) and has to
    // backspace manually. PR review feedback.
    expect(selectSpy).toHaveBeenCalledOnce();
    expect(useUIStore.getState().focusGridSearchRequest).toBe(1);
  });

  it('a second bump (re-press) re-focuses and re-selects — not idempotent', () => {
    // The counter is *not* guarded by an equality check (unlike the
    // other uiStore setters) so two ⌘+F presses bump the counter
    // 0 → 1 → 2 and the effect fires twice. This matters when the
    // user is already in the search and presses ⌘+F to start a new
    // search: the second press re-selects the existing text, ready
    // for the new keystroke.
    //
    // Each press is a separate React event with its own render
    // cycle, so the two bumps must be wrapped in SEPARATE `act()`
    // calls — a single `act` would batch the two store updates
    // into one re-render and the effect would only fire once with
    // the final counter value (2), not twice with intermediate
    // values (1, then 2).
    render(<GridControls />);
    const input = screen.getByTestId('grid-search-input') as HTMLInputElement;
    const focusSpy = vi.spyOn(input, 'focus');
    const selectSpy = vi.spyOn(input, 'select');

    act(() => {
      useUIStore.getState().requestFocusGridSearch();
    });
    act(() => {
      useUIStore.getState().requestFocusGridSearch();
    });

    expect(focusSpy).toHaveBeenCalledTimes(2);
    expect(selectSpy).toHaveBeenCalledTimes(2);
    expect(useUIStore.getState().focusGridSearchRequest).toBe(2);
  });

  it('mounting with focusGridSearchRequest > 0 focuses on first render (cold-load case)', () => {
    // The user pressed ⌘+F in the brief window between Tauri-start
    // and the TitleBar mounting this component. The counter is
    // already 1 (App.tsx bumped it). When this component finally
    // mounts, the `useLayoutEffect` runs with the current value
    // and `focusRequest > 0` is true, so the input focuses
    // immediately — no second press required. Without this, the
    // user would have to press ⌘+F *again* after the window
    // finished loading, which feels like the binding was lost.
    //
    // `useLayoutEffect` runs synchronously after the DOM mutation
    // in jsdom, so the focus + select have already happened by the
    // time `render` returns. The prototype spies are installed
    // BEFORE `render` so they catch the mount-time call.
    useUIStore.setState({ focusGridSearchRequest: 1 });

    const focusSpy = vi.spyOn(HTMLInputElement.prototype, 'focus');
    const selectSpy = vi.spyOn(HTMLInputElement.prototype, 'select');

    try {
      render(<GridControls />);

      expect(focusSpy).toHaveBeenCalled();
      expect(selectSpy).toHaveBeenCalled();
    } finally {
      focusSpy.mockRestore();
      selectSpy.mockRestore();
    }
  });

  it('suppresses the WebKit / Blink native-search ✕ button via a Tailwind arbitrary variant', () => {
    // <input type="search"> renders a native ✕ in WebKit (macOS
    // Tauri) and Blink (Windows Tauri). The component suppresses
    // it via `[&::-webkit-search-cancel-button]:appearance-none` so
    // the user doesn't see a stacked double-✕ (native + our
    // custom clear button). Pin the class here so a future edit
    // that drops the suppression (or typos the arbitrary-variant
    // syntax) is caught before it ships.
    render(<GridControls />);
    const input = screen.getByTestId('grid-search-input');
    const className = input.getAttribute('class') ?? '';
    expect(className).toMatch(/\[&::-webkit-search-cancel-button\]:appearance-none/);
  });
});
