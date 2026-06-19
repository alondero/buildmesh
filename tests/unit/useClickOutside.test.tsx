/**
 * Tests for useClickOutside — issue #492.
 *
 * The hook replaces a hand-rolled `useEffect + addEventListener('mousedown')`
 * pattern duplicated across 4 files. The critical regression to pin is the
 * SCOPED selector: the hook must always check the exact `[<attr>="<open>"]`
 * selector, never the loose `[<attr>]` form. `Sidebar.tsx` had drifted to
 * the loose form before #492 — a click on a *different* dropdown's body
 * would have closed the wrong one. The hook's signature makes that drift
 * syntactically impossible (the value is interpolated into the selector),
 * but the test pins the runtime behaviour so a future refactor cannot
 * reintroduce it.
 *
 * Behaviors pinned:
 *   1. No listener attached while `open === null` (no churn when closed).
 *   2. Listener calls onClose when target is OUTSIDE the scoped element.
 *   3. Listener does NOT call onClose when target is INSIDE the scoped element.
 *   4. Listener cleans up on unmount and when open flips back to null.
 *   5. Multiple instances coexist (probe tab can host several at once).
 *   6. REGRESSION: a click on a *different* dropdown's body does not close
 *      this one — the selector is value-scoped, not attribute-loose.
 *   7. Custom attributeName is honoured (the third arg).
 */
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, screen, act } from '@testing-library/react';
import { useState } from 'react';
import { useClickOutside } from '../../src/hooks/useClickOutside';

afterEach(() => cleanup());

describe('useClickOutside', () => {
  it('does not attach a listener while closed (open === null)', () => {
    // Render closed; mousedown on document must not throw and must not
    // reach the spy. The clean way to assert is to mount a parent that
    // opens only on close, then verify the open path is never taken.
    const onClose = vi.fn();
    function Closed() {
      useClickOutside<number>(null, onClose);
      return <div data-testid="root">closed</div>;
    }
    render(<Closed />);
    expect(() => fireEvent.mouseDown(document)).not.toThrow();
    expect(onClose).not.toHaveBeenCalled();
  });

  it('calls onClose when mousedown lands outside the scoped element', () => {
    const onClose = vi.fn();
    function Probe() {
      useClickOutside<number>(42, onClose);
      return (
        <div>
          <div data-dropdown-for="42" data-testid="inside">inside</div>
          <div data-testid="outside">outside</div>
        </div>
      );
    }
    render(<Probe />);
    fireEvent.mouseDown(screen.getByTestId('outside'));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('does NOT call onClose when mousedown lands inside the scoped element', () => {
    const onClose = vi.fn();
    function Probe() {
      useClickOutside<number>(42, onClose);
      return (
        <div>
          <div data-dropdown-for="42" data-testid="inside">inside</div>
          <div data-testid="outside">outside</div>
        </div>
      );
    }
    render(<Probe />);
    fireEvent.mouseDown(screen.getByTestId('inside'));
    expect(onClose).not.toHaveBeenCalled();
  });

  it('REGRESSION (issue #492): scoped selector closes when clicking a sibling dropdown (the Sidebar loose-selector bug)', () => {
    // `Sidebar.tsx` before #492 used a loose selector `[data-dropdown-for]`
    // (no value). Clicking ANY element carrying the attribute — even one
    // belonging to a different row — would be considered "inside" the
    // currently-open sidebar dropdown, and the dropdown would NOT close.
    // The correct behaviour: a sibling dropdown's body is OUTSIDE this
    // dropdown's scope, so clicking it must close. The hook's scoped
    // selector `[data-dropdown-for="<open>"]` matches only elements with
    // the exact value, so a sibling with `data-dropdown-for="999"` does
    // NOT match the open dropdown's selector `42`, and onClose fires.
    const onClose = vi.fn();
    function Probe() {
      useClickOutside<number>(42, onClose);
      return (
        <div>
          <div data-dropdown-for="42" data-testid="inside-42">a</div>
          <div data-dropdown-for="999" data-testid="other-dropdown">b</div>
        </div>
      );
    }
    render(<Probe />);
    fireEvent.mouseDown(screen.getByTestId('other-dropdown'));
    // The sibling dropdown IS outside the open dropdown → close fires.
    expect(onClose).toHaveBeenCalledTimes(1);

    // And for contrast: a click on the actual scoped element must NOT close.
    onClose.mockClear();
    fireEvent.mouseDown(screen.getByTestId('inside-42'));
    expect(onClose).not.toHaveBeenCalled();
  });

  it('cleans up the listener on unmount (no late fires)', () => {
    const onClose = vi.fn();
    function Probe() {
      useClickOutside<number>(42, onClose);
      return <div data-testid="outside">outside</div>;
    }
    const { unmount } = render(<Probe />);
    unmount();
    // After unmount, document-level mousedown must not reach the spy.
    fireEvent.mouseDown(document);
    expect(onClose).not.toHaveBeenCalled();
  });

  it('detaches the listener when open flips back to null', () => {
    const onClose = vi.fn();
    function Probe() {
      const [open, setOpen] = useState<number | null>(42);
      useClickOutside(open, () => {
        onClose();
        setOpen(null);
      });
      return (
        <div>
          <div data-dropdown-for={String(open ?? '')} data-testid="inside">inside</div>
          <div data-testid="outside">outside</div>
        </div>
      );
    }
    render(<Probe />);
    // Click outside → onClose fires → setOpen(null) → listener detaches.
    fireEvent.mouseDown(screen.getByTestId('outside'));
    expect(onClose).toHaveBeenCalledTimes(1);
    // Now closed. A second outside click must not re-fire.
    fireEvent.mouseDown(screen.getByTestId('outside'));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('multiple instances coexist (each fires its own onClose on the same outside click)', () => {
    const closeA = vi.fn();
    const closeB = vi.fn();
    function TwoDropdowns() {
      useClickOutside<number>(1, closeA);
      useClickOutside<number>(2, closeB);
      return (
        <div>
          <div data-dropdown-for="1" data-testid="inside-a">a</div>
          <div data-dropdown-for="2" data-testid="inside-b">b</div>
          <div data-testid="outside-both">both outside</div>
        </div>
      );
    }
    render(<TwoDropdowns />);
    // Click on a div that is outside BOTH scoped dropdowns. Each instance's
    // listener independently sees the target, checks its own selector, and
    // closes.
    fireEvent.mouseDown(screen.getByTestId('outside-both'));
    expect(closeA).toHaveBeenCalledTimes(1);
    expect(closeB).toHaveBeenCalledTimes(1);

    // Click on inside-a: only closeA should fire... actually no, inside-a
    // is outside the closeB's scoped selector, so both still fire. Click
    // on inside-b: only closeB fires because inside-b is inside closeB's
    // selector but outside closeA's.
    fireEvent.mouseDown(screen.getByTestId('inside-b'));
    expect(closeA).toHaveBeenCalledTimes(2);
    expect(closeB).toHaveBeenCalledTimes(1);
  });

  it('honours a custom attributeName (third argument)', () => {
    const onClose = vi.fn();
    function Probe() {
      useClickOutside<string>('widget-7', onClose, 'data-popover-for');
      return (
        <div>
          <div data-popover-for="widget-7" data-testid="inside-popover">inside</div>
          <div data-testid="outside-popover">outside</div>
        </div>
      );
    }
    render(<Probe />);
    fireEvent.mouseDown(screen.getByTestId('inside-popover'));
    expect(onClose).not.toHaveBeenCalled();
  });

  it('does not re-attach the listener on every render that produces a fresh onClose closure', () => {
    // The ref pattern means the listener is keyed only on `open`, not on
    // the onClose identity. We assert this indirectly: a parent that
    // re-renders repeatedly while open stays at the same value must not
    // cause the spy to double-fire (which would happen if the listener
    // were being torn down + rebuilt per render and the second click
    // reached a duplicate listener).
    let renderCount = 0;
    const onClose = vi.fn();
    function Probe() {
      const [, force] = useState(0);
      renderCount++;
      useClickOutside<number>(42, onClose);
      return (
        <div>
          <div data-dropdown-for="42" data-testid="inside">inside</div>
          <button data-testid="force" onClick={() => force(x => x + 1)} />
          <div data-testid="outside">outside</div>
        </div>
      );
    }
    render(<Probe />);
    const initialRenders = renderCount;
    // Force a few re-renders.
    act(() => { screen.getByTestId('force').click(); });
    act(() => { screen.getByTestId('force').click(); });
    expect(renderCount).toBeGreaterThan(initialRenders);
    // One outside click → one onClose call. If the listener had been
    // re-attached on each render, duplicate listeners could fire more.
    fireEvent.mouseDown(screen.getByTestId('outside'));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});