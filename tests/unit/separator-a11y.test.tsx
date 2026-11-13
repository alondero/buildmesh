/**
 * The shared accessible separator primitive (issue #2424).
 *
 * The sidebar, the Probe panel, the grid's column/row splitters and the
 * flat two-pane row all resize a pane with a drag handle. Before this
 * primitive existed, only the Probe panel's handle was reachable with the
 * keyboard, and each implementation rolled its own ARIA attributes and key
 * map. These tests pin the contract they now rely on:
 *
 *   - the element is a `separator` with an orientation, an accessible
 *     name, the current/min/max values and a tab stop;
 *   - it carries the app's visible focus indicator;
 *   - the arrow keys move the value in the direction the handle travels
 *     (and `reverse` flips that for the Probe panel, whose pane sits
 *     AFTER its handle);
 *   - Shift is the coarse modifier and PageUp/PageDown take a coarse step;
 *   - Home/End jump to the ends of the range;
 *   - the value stays inside [min, max];
 *   - the pointer drag still starts from the same element.
 */
import { describe, it, expect, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { useState } from 'react';
import { Separator } from '../../src/components/shared/Separator';

/**
 * A controlled host, so the primitive sees the value its own resize
 * produced — the same shape as the real call sites (the sidebar, the
 * grid, the Probe panel all own their width).
 */
function Host({
  initial = 200,
  min = 100,
  max = 400,
  orientation = 'vertical',
  reverse = false,
  onResize,
}: {
  initial?: number;
  min?: number;
  max?: number;
  orientation?: 'vertical' | 'horizontal';
  reverse?: boolean;
  onResize?: (next: number) => void;
}) {
  const [value, setValue] = useState(initial);
  return (
    <Separator
      label="Resize thing"
      orientation={orientation}
      value={value}
      min={min}
      max={max}
      reverse={reverse}
      onResize={next => {
        setValue(next);
        onResize?.(next);
      }}
    />
  );
}

describe('Separator (shared accessible separator primitive)', () => {
  it('exposes the separator role, orientation, name, range and a tab stop', () => {
    render(<Host initial={250} min={192} max={480} />);

    const separator = screen.getByRole('separator', { name: 'Resize thing' });
    expect(separator.getAttribute('aria-orientation')).toBe('vertical');
    expect(separator.getAttribute('aria-valuenow')).toBe('250');
    expect(separator.getAttribute('aria-valuemin')).toBe('192');
    expect(separator.getAttribute('aria-valuemax')).toBe('480');
    expect(separator.getAttribute('tabindex')).toBe('0');
  });

  it('announces a horizontal orientation for a row splitter', () => {
    render(<Host orientation="horizontal" />);
    expect(screen.getByRole('separator').getAttribute('aria-orientation')).toBe('horizontal');
  });

  it('carries the app focus indicator so a keyboard user can see the handle', () => {
    render(<Host />);
    const className = screen.getByRole('separator').className;
    // `focus-visible:outline-solid` is load-bearing: Tailwind v4's
    // `outline-2` resolves `outline-style` through `--tw-outline-style`,
    // which the base `outline-none` sets to `none` — without the
    // `outline-solid` variant the ring does not paint.
    expect(className).toContain('focus-visible:outline-solid');
    expect(className).toContain('focus-visible:outline-2');
    expect(className).toContain('focus-visible:outline-accent-cyan');
  });

  it('grows the value with ArrowRight on a vertical separator', () => {
    const onResize = vi.fn();
    render(<Host initial={200} onResize={onResize} />);

    fireEvent.keyDown(screen.getByRole('separator'), { key: 'ArrowRight' });

    expect(onResize).toHaveBeenCalledWith(208);
  });

  it('shrinks the value with ArrowLeft on a vertical separator', () => {
    const onResize = vi.fn();
    render(<Host initial={200} onResize={onResize} />);

    fireEvent.keyDown(screen.getByRole('separator'), { key: 'ArrowLeft' });

    expect(onResize).toHaveBeenCalledWith(192);
  });

  it('grows the value with ArrowDown on a horizontal separator', () => {
    const onResize = vi.fn();
    render(<Host initial={200} orientation="horizontal" onResize={onResize} />);

    fireEvent.keyDown(screen.getByRole('separator'), { key: 'ArrowDown' });

    expect(onResize).toHaveBeenCalledWith(208);
  });

  it('flips the arrow direction in reverse mode (the Probe panel grows to the left)', () => {
    const onResize = vi.fn();
    render(<Host initial={200} reverse onResize={onResize} />);

    fireEvent.keyDown(screen.getByRole('separator'), { key: 'ArrowLeft' });
    expect(onResize).toHaveBeenLastCalledWith(208);

    // The value is 208 now, so the reverse arrow takes it back down.
    fireEvent.keyDown(screen.getByRole('separator'), { key: 'ArrowRight' });
    expect(onResize).toHaveBeenLastCalledWith(200);
  });

  it('takes a four-times step with Shift held', () => {
    const onResize = vi.fn();
    render(<Host initial={200} onResize={onResize} />);

    fireEvent.keyDown(screen.getByRole('separator'), { key: 'ArrowRight', shiftKey: true });

    expect(onResize).toHaveBeenCalledWith(232);
  });

  it('takes a coarse step with PageUp on a vertical separator and PageDown on a horizontal one', () => {
    const onResize = vi.fn();
    const { unmount } = render(<Host initial={200} onResize={onResize} />);
    fireEvent.keyDown(screen.getByRole('separator'), { key: 'PageUp' });
    expect(onResize).toHaveBeenLastCalledWith(280);
    unmount();

    const horizontal = vi.fn();
    render(<Host initial={200} orientation="horizontal" onResize={horizontal} />);
    fireEvent.keyDown(screen.getByRole('separator'), { key: 'PageDown' });
    expect(horizontal).toHaveBeenLastCalledWith(280);
  });

  it('jumps to the ends of the range with Home and End', () => {
    const onResize = vi.fn();
    render(<Host initial={200} min={192} max={480} onResize={onResize} />);
    const separator = screen.getByRole('separator');

    fireEvent.keyDown(separator, { key: 'Home' });
    expect(onResize).toHaveBeenLastCalledWith(480);

    fireEvent.keyDown(separator, { key: 'End' });
    expect(onResize).toHaveBeenLastCalledWith(192);
  });

  it('keeps the value inside the announced range', () => {
    const onResize = vi.fn();
    render(<Host initial={195} min={192} max={480} onResize={onResize} />);

    fireEvent.keyDown(screen.getByRole('separator'), { key: 'ArrowLeft' });
    expect(onResize).toHaveBeenLastCalledWith(192);

    fireEvent.keyDown(screen.getByRole('separator'), { key: 'End' });
    expect(onResize).toHaveBeenLastCalledWith(192);
  });

  it('announces the clamped value after a resize', () => {
    render(<Host initial={196} min={192} max={480} />);
    const separator = screen.getByRole('separator');

    fireEvent.keyDown(separator, { key: 'ArrowLeft' });

    expect(separator.getAttribute('aria-valuenow')).toBe('192');
  });

  it('prevents the default scroll on the keys it handles, and ignores the rest', () => {
    render(<Host />);
    const separator = screen.getByRole('separator');

    const handled = new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true });
    const preventDefault = vi.spyOn(handled, 'preventDefault');
    fireEvent(separator, handled);
    expect(preventDefault).toHaveBeenCalled();

    const ignored = new KeyboardEvent('keydown', { key: 'a', bubbles: true, cancelable: true });
    const ignoredPrevent = vi.spyOn(ignored, 'preventDefault');
    fireEvent(separator, ignored);
    expect(ignoredPrevent).not.toHaveBeenCalled();
  });

  it('reads a live measurement when the caller supplies one', () => {
    // The Probe panel's width is a DOM measurement: a held arrow key
    // must take the NEXT step from what is on screen, not repeat a step
    // from the last committed value.
    const onResize = vi.fn();
    render(
      <Separator
        label="Resize thing"
        value={200}
        min={100}
        max={400}
        onResize={onResize}
        getValue={() => 260}
      />,
    );

    fireEvent.keyDown(screen.getByRole('separator'), { key: 'ArrowRight' });

    expect(onResize).toHaveBeenCalledWith(268);
  });

it('still starts the pointer drag from the same element', () => {
    const onMouseDown = vi.fn();
    render(
      <Separator
        label="Resize thing"
        value={200}
        min={100}
        max={400}
        onResize={() => {}}
        onMouseDown={onMouseDown}
      />,
    );

    fireEvent.mouseDown(screen.getByRole('separator'));

    expect(onMouseDown).toHaveBeenCalled();
  });
});
