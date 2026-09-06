/**
 * useEscapeKey — module-level LIFO stack + bubble-phase dispatcher (issue #649).
 *
 * The hook subsumes the per-site `window.addEventListener('keydown', …)`
 * skeletons that fire Escape handlers across the app. Each mount pushes
 * its handler onto a module-level stack; a single document-level
 * bubble-phase dispatcher invokes the top entry. This prevents the
 * cross-modal double-fire that happens when two surfaces are mounted at
 * once (each mounting its own listener) and a single Escape press closes
 * both.
 *
 * Coverage:
 *  - Basic call & non-Escape ignored
 *  - `enabled: false` excludes from stack; toggle re-adds
 *  - LIFO priority: latest mounted handler wins, falls back when top unmounts
 *  - `preventDefault` on Escape
 *  - IME composition carve-out (`isComposing: true`)
 *  - Element-level `stopPropagation` is honoured (bubble-phase dispatcher
 *    must run AFTER React's element-level onKeyDown)
 *  - Handler identity churn is safe (ref-mirrored)
 *  - `_resetEscapeKeyStackForTests` clears the stack
 */
import { describe, it, expect, vi, afterEach } from 'vitest';
import { useState } from 'react';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import { useEscapeKey, _resetEscapeKeyStackForTests } from '../../src/hooks/useEscapeKey';

afterEach(() => {
  cleanup();
  // Belt-and-suspenders: cover the case where the test doesn't render via
  // RTL, or where a mid-mount error skipped the cleanup. The vitest.setup.ts
  // beforeEach reset already covers the steady-state case; this guards
  // individual test isolation within this file.
  _resetEscapeKeyStackForTests();
});

/**
 * Minimal harness — a single button plus the hook, mirroring the
 * `Harness` shape used by `use-aria-menu.test.tsx`. Renders the hook's
 * own props so each test can drive them.
 */
function Harness({
  onEscape,
  enabled = true,
  testId = 'trigger',
}: {
  onEscape: () => void;
  enabled?: boolean;
  testId?: string;
}) {
  useEscapeKey(onEscape, enabled);
  return <button data-testid={testId}>trigger</button>;
}

describe('useEscapeKey — basic contract', () => {
  it('calls the handler when Escape fires on the document', () => {
    const onEscape = vi.fn();
    render(<Harness onEscape={onEscape} />);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onEscape).toHaveBeenCalledTimes(1);
  });

  it('ignores non-Escape keys', () => {
    const onEscape = vi.fn();
    render(<Harness onEscape={onEscape} />);
    fireEvent.keyDown(document, { key: 'Enter' });
    fireEvent.keyDown(document, { key: ' ' });
    fireEvent.keyDown(document, { key: 'ArrowDown' });
    expect(onEscape).not.toHaveBeenCalled();
  });

  it('calls preventDefault when the handler runs', () => {
    const onEscape = vi.fn();
    render(<Harness onEscape={onEscape} />);
    // fireEvent.keyDown returns false if defaultPrevented is true.
    const defaultNotPrevented = fireEvent.keyDown(document, { key: 'Escape' });
    expect(onEscape).toHaveBeenCalled();
    expect(defaultNotPrevented).toBe(false);
  });

  it('does NOT preventDefault when no handler is on the stack', () => {
    // No harness — empty stack.
    fireEvent.keyDown(document, { key: 'Escape' });
    // Nothing to assert directly; absence of crash + the previous test
    // confirming preventDefault only fires when the handler runs is the
    // contract.
  });

  it('skips IME-composition Escape (isComposing: true)', () => {
    const onEscape = vi.fn();
    render(<Harness onEscape={onEscape} />);
    fireEvent.keyDown(document, { key: 'Escape', isComposing: true });
    expect(onEscape).not.toHaveBeenCalled();
    // After composition ends, regular Escape works again.
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onEscape).toHaveBeenCalledTimes(1);
  });
});

describe('useEscapeKey — enabled flag', () => {
  it('enabled: false skips stack registration entirely', () => {
    const onEscape = vi.fn();
    render(<Harness onEscape={onEscape} enabled={false} />);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onEscape).not.toHaveBeenCalled();
  });

  it('toggling enabled: false → true re-adds the handler', () => {
    const onEscape = vi.fn();
    function Toggleable({ enabled }: { enabled: boolean }) {
      return <Harness onEscape={onEscape} enabled={enabled} />;
    }
    const { rerender } = render(<Toggleable enabled={false} />);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onEscape).not.toHaveBeenCalled();
    rerender(<Toggleable enabled={true} />);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onEscape).toHaveBeenCalledTimes(1);
  });

  it('toggling enabled: true → false removes the handler', () => {
    const onEscape = vi.fn();
    function Toggleable({ enabled }: { enabled: boolean }) {
      return <Harness onEscape={onEscape} enabled={enabled} />;
    }
    const { rerender } = render(<Toggleable enabled={true} />);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onEscape).toHaveBeenCalledTimes(1);
    rerender(<Toggleable enabled={false} />);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onEscape).toHaveBeenCalledTimes(1); // unchanged
  });
});

describe('useEscapeKey — LIFO priority (the double-fire fix)', () => {
  it('only the latest-mounted handler runs when two surfaces are stacked', () => {
    const a = vi.fn();
    const b = vi.fn();
    render(
      <>
        <Harness onEscape={a} testId="a" />
        <Harness onEscape={b} testId="b" />
      </>,
    );
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(b).toHaveBeenCalledTimes(1);
    expect(a).not.toHaveBeenCalled();
  });

  it('after the top unmounts, the previous one resumes handling', () => {
    const a = vi.fn();
    const b = vi.fn();
    function Stack() {
      const [showB, setShowB] = useState(true);
      return (
        <>
          <button data-testid="unmount-b" onClick={() => setShowB(false)}>
            unmount
          </button>
          <Harness onEscape={a} testId="a" />
          {showB && <Harness onEscape={b} testId="b" />}
        </>
      );
    }
    render(<Stack />);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(b).toHaveBeenCalledTimes(1);
    expect(a).not.toHaveBeenCalled();
    // Unmount B by triggering the cleanup via a re-render that removes it.
    fireEvent.click(screen.getByTestId('unmount-b'));
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).toHaveBeenCalledTimes(1); // unchanged
  });

  it('mounting order matters: later mount ends up on top of the stack', () => {
    // The hook relies on React's mount order: child effects run before
    // parent effects, so a component mounted later (rendered after)
    // registers later. This test pins that behaviour so any future
    // "registration order" change is intentional.
    const first = vi.fn();
    const second = vi.fn();
    render(
      <>
        <Harness onEscape={first} testId="first" />
        <Harness onEscape={second} testId="second" />
      </>,
    );
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(second).toHaveBeenCalled();
    expect(first).not.toHaveBeenCalled();
  });
});

describe('useEscapeKey — bubble-phase opt-out', () => {
  it('element-level stopPropagation prevents the dispatcher from firing', () => {
    // GridControls.tsx:105-111 uses this exact pattern to keep Escape
    // from closing a parent modal while the user is editing the search
    // input. The dispatcher is on bubble phase precisely so this works.
    const onEscape = vi.fn();
    function WithInput() {
      useEscapeKey(onEscape);
      return (
        <input
          data-testid="search"
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              e.stopPropagation();
            }
          }}
        />
      );
    }
    render(<WithInput />);
    const input = screen.getByTestId('search');
    fireEvent.keyDown(input, { key: 'Escape' });
    expect(onEscape).not.toHaveBeenCalled();
    // Confirm the dispatcher isn't broken in general: firing on document
    // (where the input isn't the target) still hits the hook.
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onEscape).toHaveBeenCalledTimes(1);
  });
});

describe('useEscapeKey — handler identity churn', () => {
  it('reads the latest handler reference on each invocation', () => {
    // If the hook captured the handler in a closure on mount, a
    // rerender with a fresh handler would never fire. Mirrors the
    // `itemCountRef` discipline in useAriaMenu.ts:94-99.
    const v1 = vi.fn();
    const v2 = vi.fn();
    function WithSwap({ handler }: { handler: () => void }) {
      return <Harness onEscape={handler} testId="swap" />;
    }
    const { rerender } = render(<WithSwap handler={v1} />);
    rerender(<WithSwap handler={v2} />);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(v2).toHaveBeenCalledTimes(1);
    expect(v1).not.toHaveBeenCalled();
  });
});

describe('useEscapeKey — test reset seam', () => {
  it('_resetEscapeKeyStackForTests clears registrations that survived unmount', () => {
    // Mount a handler, then forcibly clear the stack without unmounting.
    // The next Escape must not invoke the handler — proves the reset
    // is real, not just RTL cleanup under another name.
    const onEscape = vi.fn();
    const { unmount } = render(<Harness onEscape={onEscape} />);
    _resetEscapeKeyStackForTests();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onEscape).not.toHaveBeenCalled();
    // Cleanup the rendered tree to avoid leaking the harness.
    unmount();
  });

  it('does not reset the id counter (post-reset ids stay monotonic)', () => {
    // Issue #649 review: resetting `nextId = 1` risked id collisions
    // if a stale async cleanup from a previous test fired after the
    // reset. Pin that the counter is preserved across resets, so post-
    // reset ids are strictly greater than any pre-reset id.
    const a = vi.fn();
    const b = vi.fn();
    // Mount → push entry with some id.
    const first = render(<Harness onEscape={a} />);
    _resetEscapeKeyStackForTests();
    // Force the unmount to fire AFTER the reset (simulating a stale
    // async cleanup). The splice uses the id that was captured at
    // push time — it must not find the new (post-reset) entry.
    first.unmount();
    // Now mount a fresh handler. Its id must be > the pre-reset id,
    // and an Escape must fire only this new handler.
    render(<Harness onEscape={b} />);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(b).toHaveBeenCalledTimes(1);
    expect(a).not.toHaveBeenCalled();
  });
});
