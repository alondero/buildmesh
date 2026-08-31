/**
 * Tests for the GridControls component (issue #998).
 *
 * The component is the minimum View-Header surface #998 needs: a
 * controlled search input bound to `uiStore.gridSearchQuery`, plus a
 * clear button and an Esc-to-clear keyboard handler. The filter
 * popover, sort selector, badges, and reset button called out in
 * #997 are intentionally absent here.
 *
 * What this file pins:
 *
 *   - The input renders with the live `gridSearchQuery` value and
 *     reflects keystrokes back into the store (round-trip through the
 *     `setGridSearchQuery` setter).
 *   - The clear button is hidden when the query is empty and visible
 *     when non-empty; clicking it sets the query back to ''.
 *   - The input's onKeyDown handles Escape by clearing the value
 *     *and* calling `e.preventDefault()` / `e.stopPropagation()` so
 *     the Modal/AgentNodeView Esc listeners don't fire when the user
 *     is clearing a search (vs. closing a dialog).
 *   - The input is registered with the `gridSearchFocus` singleton on
 *     mount and unregistered on unmount — the contract App.tsx's
 *     `focus-grid-search` shortcut depends on.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { act } from 'react';

import { GridControls } from '../../src/components/AgentNodeView/GridControls';
import { useUIStore } from '../../src/stores/uiStore';
import {
  focusGridSearch,
  __resetGridSearchInputForTests,
} from '../../src/lib/gridSearchFocus';

describe('GridControls (issue #998)', () => {
  beforeEach(() => {
    // Reset both stores and the focus singleton between cases so a
    // leaked registration in one test doesn't bleed into the next.
    useUIStore.setState({
      gridSearchQuery: '',
    });
    __resetGridSearchInputForTests();
  });

  afterEach(() => {
    cleanup();
    __resetGridSearchInputForTests();
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

    act(() => {
      useUIStore.setState({ gridSearchQuery: 'gamma' });
    });
    rerender(<GridControls />);
    expect(screen.getByTestId('grid-search-clear')).toBeTruthy();
  });

  it('clicking the clear button empties the query', () => {
    useUIStore.setState({ gridSearchQuery: 'gamma' });
    render(<GridControls />);

    fireEvent.click(screen.getByTestId('grid-search-clear'));

    expect(useUIStore.getState().gridSearchQuery).toBe('');
  });

  it('pressing Escape inside the input clears the query and stops propagation', () => {
    useUIStore.setState({ gridSearchQuery: 'gamma' });
    render(<GridControls />);
    const input = screen.getByTestId('grid-search-input');

    // jsdom doesn't run real keyboard routing, so we use
    // `fireEvent.keyDown` with a constructed event and assert the
    // *contract* the handler is supposed to maintain: clear the
    // value, preventDefault, stopPropagation. A regression that drops
    // either preventDefault or stopPropagation would let the
    // Modal/Single-mode Esc listeners close a dialog the user is
    // actively typing in — the bug issue #998 is explicitly designed
    // to avoid.
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

  it('pressing Escape on an empty query still stops propagation (no-op clear, but Esc must not bubble)', () => {
    render(<GridControls />);
    const input = screen.getByTestId('grid-search-input');

    const event = new KeyboardEvent('keydown', {
      key: 'Escape',
      bubbles: true,
      cancelable: true,
    });
    const stopPropagationSpy = vi.spyOn(event, 'stopPropagation');
    fireEvent(input, event);

    expect(useUIStore.getState().gridSearchQuery).toBe('');
    expect(stopPropagationSpy).toHaveBeenCalled();
  });

  it('registers the input with the focus singleton on mount, unregisters on unmount', () => {
    // The singleton contract: a mounted GridControls means
    // `focusGridSearch()` returns true; unmounting flips it back to
    // false. App.tsx's `focus-grid-search` shortcut depends on this
    // for the cold-load case (the very first Ctrl+F from a fresh
    // boot must find the input, even though the layout effect runs
    // synchronously after mount).
    const { unmount } = render(<GridControls />);

    expect(focusGridSearch()).toBe(true);

    unmount();

    expect(focusGridSearch()).toBe(false);
  });
});
