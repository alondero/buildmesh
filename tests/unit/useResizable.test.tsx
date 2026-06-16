/**
 * Tests for useResizable — the shared resize hook.
 *
 * Issue #301: all resize hooks in the codebase had a stale-closure bug.
 * The `useEffect` that installs document mousemove/mouseup listeners has
 * `[]` deps (necessary — re-attaching mid-drag would lose pointer capture).
 * The `mousedown` handler closed over `width` (a state value) and snapshotted
 * `startWidthRef.current = width`. On the second drag, React may not have
 * flushed the first drag's setWidth(...) into a re-render yet, so the closed-
 * over `width` is the pre-first-drag value. The visible symptom: the handle
 * visually jumps by 10–30px on the second click of a fast double-drag.
 *
 * Fix: keep a `valueRef.current` updated synchronously on every render (it
 * runs in the render body, before any effect or event handler), and snapshot
 * from that ref in mousedown. The closed-over state is no longer consulted.
 *
 * These tests pin the fix by exercising the observable behaviour: a second
 * drag immediately after the first must not jump.
 */
import { describe, it, expect, afterEach } from 'vitest';
import { render, fireEvent, cleanup, act } from '@testing-library/react';
import { useState } from 'react';
import { useResizable } from '../../src/hooks/useResizable';

/**
 * Renders a single div with an `onMouseDown={handleMouseDown}`. Exposes the
 * hook result (and a parent-controlled `width`) through a mutable ref so
 * tests can assert state without re-rendering the probe.
 */
function makeProbe() {
  const captured: { current: ReturnType<typeof useResizable> | null } = { current: null };
  const parent: { current: { width: number; setWidth: (w: number) => void } | null } = { current: null };

  function Probe({ width }: { width: number }) {
    const [internal, setInternal] = useState(width);
    parent.current = { width: internal, setWidth: setInternal };
    captured.current = useResizable({
      value: internal,
      min: 100,
      max: 800,
      side: 'right',
      onChange: setInternal,
    });
    return (
      <div
        data-testid="handle"
        onMouseDown={(e) => {
          // mousedown events from the DOM carry a real clientX; programmatic
          // calls (in tests) use a mock event with clientX=0. Both flow
          // through the same handler.
          captured.current!.handleMouseDown(e);
        }}
      />
    );
  }
  return { Probe, captured, parent };
}

/** Build a fake React.MouseEvent with a given clientX for unit-test dispatches. */
function fakeMouseEvent(clientX: number): React.MouseEvent {
  return { preventDefault: () => {}, clientX } as unknown as React.MouseEvent;
}

afterEach(() => cleanup());

describe('useResizable', () => {
  it('returns the initial value and a stable mousedown handler', () => {
    const { Probe, captured, parent } = makeProbe();
    render(<Probe width={256} />);
    expect(captured.current).not.toBeNull();
    expect(typeof captured.current!.handleMouseDown).toBe('function');
    expect(parent.current!.width).toBe(256);
  });

  it('updates value when dragged in the "right" direction (delta grows width)', () => {
    const { Probe, captured, parent } = makeProbe();
    render(<Probe width={256} />);

    act(() => {
      captured.current!.handleMouseDown(fakeMouseEvent(0));
    });
    act(() => {
      fireEvent.mouseMove(document, { clientX: 40 });
    });
    act(() => {
      fireEvent.mouseUp(document);
    });
    // Initial mousedown recorded clientX=0, so 40px right = +40 width
    expect(parent.current!.width).toBe(256 + 40);
    expect(captured.current!.isResizing).toBe(false);
  });

  it('clamps the new value to the configured [min, max] range', () => {
    const { Probe, captured, parent } = makeProbe();
    render(<Probe width={500} />);

    act(() => {
      captured.current!.handleMouseDown(fakeMouseEvent(0));
    });
    // Try to grow by 1000 — should clamp to max=800
    act(() => {
      fireEvent.mouseMove(document, { clientX: 1000 });
    });
    act(() => {
      fireEvent.mouseUp(document);
    });
    expect(parent.current!.width).toBe(800);

    // Try to shrink below min=100
    const { Probe: Probe2, captured: captured2, parent: parent2 } = makeProbe();
    render(<Probe2 width={150} />);
    act(() => {
      captured2.current!.handleMouseDown(fakeMouseEvent(0));
    });
    act(() => {
      fireEvent.mouseMove(document, { clientX: -500 });
    });
    act(() => {
      fireEvent.mouseUp(document);
    });
    expect(parent2.current!.width).toBe(100);
  });

  it('REGRESSION (issue #301): a stale mousedown handler from a prior render still sees the latest value (via ref)', () => {
    // The actual bug from #301: between the first drag's setState and React
    // committing the re-render, the DOM still has the OLD mousedown handler
    // attached (the one created during the pre-drag render). If that old
    // handler reads `width` from its closed-over state, it sees the stale
    // value. With the ref-based fix, the old handler still reads the latest
    // value via the always-up-to-date `valueRef.current`.
    //
    // We model that race by capturing the mousedown handler from the initial
    // render and re-invoking it AFTER the first drag has changed the value.
    // `useCallback([side])` returns the same function reference (side is
    // stable), so the captured handler IS the handler still attached to the
    // DOM. What makes it "stale" is that its render-time closure would have
    // captured `value=256` if the implementation read from `value` directly.
    // The fix routes through `valueRef.current`, which the second render
    // updated to 306 before this test invokes the captured handler.
    const { Probe, captured, parent } = makeProbe();
    render(<Probe width={256} />);

    // Snapshot the handler attached to the handle during the initial render.
    // Its render-time closure had `value=256` in scope at the time.
    const staleHandler = captured.current!.handleMouseDown;

    // ---- First drag: +50 → width = 306. Commits a new render. ----
    act(() => {
      captured.current!.handleMouseDown(fakeMouseEvent(0));
    });
    act(() => {
      fireEvent.mouseMove(document, { clientX: 50 });
    });
    act(() => {
      fireEvent.mouseUp(document);
    });
    expect(parent.current!.width).toBe(306);

    // ---- Simulate the race: invoke the captured (render-#1) handler. ----
    // If the hook reads from `value` (the closed-over prop), the captured
    // handler sees value=256 and the new width collapses to 256.
    // If the hook reads from `valueRef.current`, the handler sees 306 and
    // a 0-px move leaves the width at 306.
    act(() => {
      staleHandler(fakeMouseEvent(0));
    });
    act(() => {
      fireEvent.mouseMove(document, { clientX: 0 });
    });
    act(() => {
      fireEvent.mouseUp(document);
    });
    expect(parent.current!.width).toBe(306);
  });

  it('REGRESSION (issue #301): stale handler applies the second-drag delta from the latest value', () => {
    // Companion to the test above — covers a non-zero delta to make sure
    // the ref-baseline + delta math is right when the handler is stale.
    const { Probe, captured, parent } = makeProbe();
    render(<Probe width={256} />);
    const staleHandler = captured.current!.handleMouseDown;

    act(() => {
      captured.current!.handleMouseDown(fakeMouseEvent(0));
    });
    act(() => {
      fireEvent.mouseMove(document, { clientX: 50 });
    });
    act(() => {
      fireEvent.mouseUp(document);
    });
    expect(parent.current!.width).toBe(306);

    // Stale handler with +20 delta. Expected width = 306 + 20 = 326.
    // Buggy: would be 256 + 20 = 276.
    act(() => {
      staleHandler(fakeMouseEvent(0));
    });
    act(() => {
      fireEvent.mouseMove(document, { clientX: 20 });
    });
    act(() => {
      fireEvent.mouseUp(document);
    });
    expect(parent.current!.width).toBe(326);
  });

  it('isResizing flips true during drag and false on mouseup', () => {
    const { Probe, captured } = makeProbe();
    render(<Probe width={256} />);

    expect(captured.current!.isResizing).toBe(false);
    act(() => {
      captured.current!.handleMouseDown(fakeMouseEvent(0));
    });
    expect(captured.current!.isResizing).toBe(true);
    act(() => {
      fireEvent.mouseMove(document, { clientX: 30 });
    });
    expect(captured.current!.isResizing).toBe(true);
    act(() => {
      fireEvent.mouseUp(document);
    });
    expect(captured.current!.isResizing).toBe(false);
  });

  it('mouseup with no drag in progress is a no-op (does not throw)', () => {
    const { Probe, captured } = makeProbe();
    render(<Probe width={256} />);
    expect(() => fireEvent.mouseUp(document)).not.toThrow();
    expect(captured.current!.isResizing).toBe(false);
  });

  it('does not respond to mousemove when not dragging', () => {
    const { Probe, captured, parent } = makeProbe();
    render(<Probe width={256} />);
    act(() => {
      fireEvent.mouseMove(document, { clientX: 500 });
    });
    expect(parent.current!.width).toBe(256);
  });
});
