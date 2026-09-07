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
function fakeMouseEvent(clientX: number, clientY: number = clientX): React.MouseEvent {
  return { preventDefault: () => {}, clientX, clientY } as unknown as React.MouseEvent;
}

/** Build a fake DOM MouseEvent with a given clientX/clientY for `fireEvent.mouseMove`. */
function fakeDomMouseEvent(clientX: number, clientY: number = clientX) {
  return { clientX, clientY };
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
    const { Probe, _captured, parent } = makeProbe();
    render(<Probe width={256} />);
    act(() => {
      fireEvent.mouseMove(document, { clientX: 500 });
    });
    expect(parent.current!.width).toBe(256);
  });
});

// ===== Splitter shape (multi-pane splitters) =================================
//
// Issue #726 collapses the two AgentNodeView splitter implementations
// (ResizablePanes and GridSplitter) onto one shared hook. Both call sites
// pass the same `compute / axis / throttle / lockBody / onChange / onEnd`
// shape; these tests pin the behavior contract:
//   - body cursor + userSelect are set on mousedown and cleared on mouseup;
//   - body cursor is cleared when the component unmounts MID-DRAG (the
//     regression the modernization pass closed for GridSplitter — now
//     guaranteed by the hook for every splitter);
//   - onEnd fires once on mouseup with the settled value;
//   - raf throttle coalesces many mousemoves into one onChange per frame;
//   - axis: 'row' reads clientY (not clientX);
//   - the #301 stale-closure fix still holds for arrays.
// A separate top-level `describe` keeps the splitter tests visibly grouped.

describe('useResizable — splitter shape (issue #726)', () => {
  /**
   * Probe factory for the splitter shape. Renders a single handle; tests
   * record every `onChange` value via a mutable ref so we can assert on the
   * final array contents without re-rendering the probe. `idxRef` mimics the
   * ResizablePanes / GridSplitter call-site pattern: the per-handle click
   * records the separator index, the splitter's `compute` reads it.
   */
  function makeSplitterProbe(opts?: { throttle?: 'sync' | 'raf'; axis?: 'col' | 'row' }) {
    const captured: { current: ReturnType<typeof useResizable<number[]>> | null } = { current: null };
    const log: { current: number[][] } = { current: [] };
    const endFired: { current: number } = { current: 0 };
    const idxRef: { current: number } = { current: 0 };
    const baselineRef: { current: number[] } = { current: [] };

    function Probe() {
      const [widths, setWidths] = useState([50, 50]);
      baselineRef.current = widths;
      captured.current = useResizable<number[]>({
        value: widths,
        axis: opts?.axis ?? 'col',
        throttle: opts?.throttle ?? 'sync',
        lockBody: true,
        compute: (baseline, deltaPx) => {
          // 1000-px container mental model: each mousemove = deltaPercent
          // wide. Same shape as ResizablePanes / GridSplitter, but with no
          // MIN clamp so this probe is about the hook's plumbing, not the
          // call-site math.
          const deltaPercent = deltaPx / 10; // 100 px per percent.
          const i = idxRef.current;
          const next = [...baseline];
          next[i] = baseline[i] + deltaPercent;
          next[i + 1] = baseline[i + 1] - deltaPercent;
          return next;
        },
        onChange: (next: number[]) => {
          log.current.push(next);
          setWidths(next);
        },
        onEnd: () => {
          endFired.current += 1;
        },
      });
      return (
        <div
          data-testid="handle"
          onMouseDown={(e) => {
            idxRef.current = 0;
            captured.current!.handleMouseDown(e);
          }}
        />
      );
    }
    return { Probe, captured, log, endFired, idxRef, baselineRef };
  }

  it('locks document.body cursor + userSelect on mousedown and releases on mouseup', () => {
    const { Probe, captured } = makeSplitterProbe();
    render(<Probe />);

    // Pre-drag: body lock is clean.
    expect(document.body.style.cursor).toBe('');
    expect(document.body.style.userSelect).toBe('');

    act(() => {
      captured.current!.handleMouseDown(fakeMouseEvent(0));
    });
    expect(document.body.style.cursor).toBe('col-resize');
    expect(document.body.style.userSelect).toBe('none');

    act(() => {
      fireEvent.mouseUp(document);
    });
    expect(document.body.style.cursor).toBe('');
    expect(document.body.style.userSelect).toBe('');
  });

  it('REGRESSION (modernization pass): body cursor cleared when component unmounts mid-drag', () => {
    // The modernization pass fixed this leak in GridSplitter; issue #726
    // promoted the fix to the shared hook so every splitter inherits it.
    // Pin the contract: if a drag is in progress and the host unmounts,
    // document.body MUST NOT keep `col-resize` / `user-select: none`.
    const { Probe, captured } = makeSplitterProbe();
    const { unmount } = render(<Probe />);

    act(() => {
      captured.current!.handleMouseDown(fakeMouseEvent(0));
    });
    expect(document.body.style.cursor).toBe('col-resize');
    expect(document.body.style.userSelect).toBe('none');

    // Unmount with the drag still active — no mouseup fired by the user.
    act(() => {
      unmount();
    });
    expect(document.body.style.cursor).toBe('');
    expect(document.body.style.userSelect).toBe('');
  });

  it('onEnd fires exactly once on mouseup, after the last onChange', () => {
    const { Probe, captured, log, endFired } = makeSplitterProbe();
    render(<Probe />);

    act(() => {
      captured.current!.handleMouseDown(fakeMouseEvent(0));
    });
    act(() => {
      fireEvent.mouseMove(document, { clientX: 10 });
    });
    act(() => {
      fireEvent.mouseMove(document, { clientX: 20 });
    });
    act(() => {
      fireEvent.mouseUp(document);
    });

    // Two onChange calls (one per mousemove), then one onEnd.
    expect(log.current.length).toBe(2);
    expect(endFired.current).toBe(1);
    // The latest onChange value is the array we expect to find persisted
    // once the drag settled.
    expect(log.current[1]).toEqual([52, 48]);
  });

  it('raf throttle coalesces a burst of mousemoves into one onChange per frame', async () => {
    // Synchronous-shape variant: under `throttle: 'raf'`, two mousemoves
    // dispatched synchronously (one right after the other) must NOT both
    // queue a fresh raf frame. The hook's `if (rafId !== null) return;`
    // coalesces the second one onto the first.
    //
    // jsdom doesn't define requestAnimationFrame; we polyfill it with
    // setImmediate and verify that after firing two mousemoves synchronously,
    // the LAST recorded value matches the latest mousemove (proving the
    // coalesce didn't drop the latest delta) AND that there were fewer
    // onChange calls than the count of mousemoves (proving coalescing).
    const origRaf = (globalThis as { requestAnimationFrame?: (cb: FrameRequestCallback) => number }).requestAnimationFrame;
    let queuedCount = 0;
    (globalThis as unknown as { requestAnimationFrame: (cb: FrameRequestCallback) => number }).requestAnimationFrame = (cb) => {
      queuedCount += 1;
      return setImmediate(() => cb(performance.now())) as unknown as number;
    };

    try {
      const { Probe, captured, log } = makeSplitterProbe({ throttle: 'raf' });
      render(<Probe />);

      queuedCount = 0;
      act(() => {
        captured.current!.handleMouseDown(fakeMouseEvent(0));
      });
      // Two synchronously-dispatched mousemoves before any tick.
      act(() => {
        fireEvent.mouseMove(document, fakeDomMouseEvent(10));
        fireEvent.mouseMove(document, fakeDomMouseEvent(30));
      });
      // Let setImmediate fire the queued raf.
      await new Promise((r) => setImmediate(r));
      await new Promise((r) => setImmediate(r));

      // Coalesced: exactly ONE frame was queued despite two mousemoves.
      // A naive non-throttled impl would queue two; a buggy rAF impl that
      // failed to dedupe would queue two as well. Exactly-one is the
      // contract.
      expect(queuedCount).toBe(1);
      // And the recorded onChange values: a single fire with the latest
      // delta (clientX=30 → +3 pct on the first pane).
      expect(log.current.length).toBe(1);
      expect(log.current[0]).toEqual([53, 47]);
    } finally {
      if (origRaf) {
        (globalThis as unknown as { requestAnimationFrame: typeof origRaf }).requestAnimationFrame = origRaf;
      } else {
        delete (globalThis as unknown as { requestAnimationFrame?: unknown }).requestAnimationFrame;
      }
    }
  });

  it('axis: "row" reads clientY (not clientX) for the delta', () => {
    const { Probe, captured, _log } = makeSplitterProbe({ axis: 'row' });
    render(<Probe />);

    act(() => {
      captured.current!.handleMouseDown(fakeMouseEvent(0)); // clientX irrelevant; clientY=0.
    });
    // Move along X only — should NOT register a delta because axis='row'.
    act(() => {
      fireEvent.mouseMove(document, { clientX: 200, clientY: 0 });
    });
    // First move fires +0 delta (clientY same as start). Move along Y now.
    act(() => {
      fireEvent.mouseUp(document);
    });

    // After axis=row, mousemove with clientY=0 records 0 delta → onChange
    // gets baseline. Re-run with a real Y delta to prove axis routing.
    const { Probe: Probe2, captured: c2, log: log2 } = makeSplitterProbe({ axis: 'row' });
    render(<Probe2 />);
    act(() => {
      c2.current!.handleMouseDown(fakeMouseEvent(0));
    });
    act(() => {
      fireEvent.mouseMove(document, { clientX: 9999, clientY: 40 });
    });
    act(() => {
      fireEvent.mouseUp(document);
    });
    // deltaPercent = 40/10 = 4, so the first pane should be 54 and the
    // second 46. clientX=9999 should NOT contribute to the delta.
    expect(log2.current[0][0]).toBe(54);
    expect(log2.current[0][1]).toBe(46);
  });

  it('stale-closure fix (issue #301) extends to the splitter shape: ref-baseline survives a render between drags', () => {
    // Compactly mimic the `#301` regression-test pattern: render twice with
    // a stale `handleMouseDown` reference (the one captured during the
    // pre-drag render), confirm the second drag still uses the latest
    // `widths` via the valueRef-snapshot, not the closure.
    const captured: { current: ReturnType<typeof useResizable<number[]>> | null } = { current: null };
    const log: { current: number[][] } = { current: [] };

    function Probe() {
      const [widths, setWidths] = useState([50, 50]);
      captured.current = useResizable<number[]>({
        value: widths,
        axis: 'col',
        throttle: 'sync',
        lockBody: true,
        compute: (baseline, deltaPx) => {
          const deltaPercent = deltaPx / 10;
          const next = [...baseline];
          next[0] = baseline[0] + deltaPercent;
          next[1] = baseline[1] - deltaPercent;
          return next;
        },
        onChange: (next: number[]) => {
          log.current.push(next);
          setWidths(next);
        },
      });
      return (
        <div
          data-testid="handle"
          onMouseDown={(e) => captured.current!.handleMouseDown(e)}
        />
      );
    }
    render(<Probe />);
    const staleHandler = captured.current!.handleMouseDown;

    // First drag: +20px → widths = [52, 48].
    act(() => {
      captured.current!.handleMouseDown(fakeMouseEvent(0));
    });
    act(() => {
      fireEvent.mouseMove(document, { clientX: 20 });
    });
    act(() => {
      fireEvent.mouseUp(document);
    });
    expect(log.current.at(-1)).toEqual([52, 48]);

    // Second drag via the STALE handler (the one captured during the very
    // first render, before setWidths had committed). Without the ref fix,
    // the closure-captured `widths` would be [50, 50] and the first
    // mousemove would log [50, 50] baseline. With the ref fix the second
    // mousedown snapshots the latest widths via `valueRef.current`.
    act(() => {
      staleHandler(fakeMouseEvent(0));
    });
    act(() => {
      fireEvent.mouseMove(document, { clientX: 10 });
    });
    act(() => {
      fireEvent.mouseUp(document);
    });
    // Expected: widths start at [52, 48] (the post-first-drag baseline),
    // +10 px → widths = [53, 47]. With a stale-closure bug, the widths
    // would have started at [50, 50], and the second drag would have left
    // them at [51, 49] — a 2-pct jump.
    expect(log.current.at(-1)).toEqual([53, 47]);
  });

  it('REGRESSION (issue #726 code review): baselineOverride overrides the render-time `value` at mousedown', () => {
    // GridSplitter passes `value: []` as a stable placeholder and supplies
    // the real per-row baseline via `handleMouseDown(e, override)`. Without
    // the override the hook would have to choose between:
    //   (a) reading `valueRef.current` set at render time (stale for any
    //       caller whose value is a derived slice like `colWidths[rowIdx]`),
    //   (b) re-evaluating `opts.value` at mousedown (broken: mousedown
    //       closures capture stale `value` too).
    //
    // This test pins the override path: two consecutive "drags" with
    // DIFFERENT baseline overrides produce DIFFERENT onChange histories,
    // each rooted in its own override. If `baselineOverride` is ignored
    // and the hook falls back to `value: []`, both drags log `[<empty>]`
    // and the assertion fails.
    const captured: { current: ReturnType<typeof useResizable<number[]>> | null } = { current: null };
    const log: { current: { baseline: number[]; next: number[] }[] } = { current: [] };
    const draggedBaselineRef: { current: number[] } = { current: [] };

    function Probe() {
      const [, setWidths] = useState<number[]>([]);
      captured.current = useResizable<number[]>({
        value: [],
        axis: 'col',
        throttle: 'sync',
        lockBody: true,
        compute: (baseline, deltaPx) => {
          // 1 px → 1 percent on the left side, -1 on the right, no clamping.
          const next = [...baseline];
          next[0] = baseline[0] + deltaPx;
          next[1] = baseline[1] - deltaPx;
          return next;
        },
        onChange: (next: number[]) => {
          log.current.push({ baseline: [...draggedBaselineRef.current], next });
          setWidths(next);
        },
      });
      return (
        <div
          data-testid="handle"
          onMouseDown={(e) => {
            const override: number[] = draggedBaselineRef.current;
            captured.current!.handleMouseDown(e, override);
          }}
        />
      );
    }
    render(<Probe />);

    // Drag 1: row 0 widths baseline. Pointer delta = +30 → left=80, right=20.
    draggedBaselineRef.current = [50, 50];
    act(() => {
      captured.current!.handleMouseDown(fakeMouseEvent(0), draggedBaselineRef.current);
    });
    act(() => {
      fireEvent.mouseMove(document, fakeDomMouseEvent(30));
    });
    act(() => {
      fireEvent.mouseUp(document);
    });
    expect(log.current.at(-1)).toEqual({ baseline: [50, 50], next: [80, 20] });

    // Drag 2: row 1 widths baseline (different shape: [30, 70]). Without
    // `baselineOverride` we'd be operating on the previous drag's baseline
    // of [50, 50]; with it, the compute sees [30, 70] from the click.
    draggedBaselineRef.current = [30, 70];
    act(() => {
      // Simulate the JSX onMouseDown firing captured.current!.handleMouseDown
      // via the override — exactly the path the GridSplitter call site uses.
      captured.current!.handleMouseDown(fakeMouseEvent(0), draggedBaselineRef.current);
    });
    act(() => {
      fireEvent.mouseMove(document, fakeDomMouseEvent(10));
    });
    act(() => {
      fireEvent.mouseUp(document);
    });
    expect(log.current.at(-1)).toEqual({ baseline: [30, 70], next: [40, 60] });
  });

  it('startValueRef is a defensive shallow clone — caller-side mutation does not leak across drags', () => {
    // `useResizable` snapshots the baseline once at mousedown via
    // `shallowClone`. A caller whose `compute` mutates its `baseline`
    // argument in place (anti-pattern, but could happen by accident)
    // should not poison the startValueRef's store of the next drag's
    // baseline — each drag's snapshot is independent.
    const captured: { current: ReturnType<typeof useResizable<number[]>> | null } = { current: null };
    const valuePerDrag: { current: number[][] } = { current: [] };
    const BASELINE: [number, number] = [10, 90];

    function Probe() {
      captured.current = useResizable<number[]>({
        value: [],
        axis: 'col',
        throttle: 'sync',
        lockBody: false,
        compute: (baseline, deltaPx) => {
          // Anti-pattern: in-place mutation of the snapshotted baseline.
          baseline[0] = baseline[0] + deltaPx;
          return baseline;
        },
        onChange: (next: number[]) => {
          valuePerDrag.current.push([...next]);
        },
      });
      return <div data-testid="handle" onMouseDown={(e) => captured.current!.handleMouseDown(e, BASELINE)} />;
    }
    render(<Probe />);

    // Pass the override explicitly — the JSX onMouseDown isn't actually
    // dispatched in this test; we're driving handleMouseDown directly.
    act(() => captured.current!.handleMouseDown(fakeMouseEvent(0), [...BASELINE]));
    act(() => fireEvent.mouseMove(document, fakeDomMouseEvent(20)));
    act(() => fireEvent.mouseUp(document));

    // The drag mutated baseline: 10→30. End-of-drag state: left=30, right=90.
    expect(valuePerDrag.current.at(-1)).toEqual([30, 90]);
    // The shallow-clone snapshot lost the mutation cleanly — the next
    // drag (if any) would START from the immutable override `[10, 90]`,
    // not from the mutated `[30, 90]`. (Pinned by the REGRESSION test
    // above for the override path.)
  });
});
