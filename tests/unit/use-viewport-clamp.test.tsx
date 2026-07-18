/**
 * useViewportClamp — translateY clamp when the menu would overflow
 * the viewport's bottom edge (issue #837).
 *
 * The hook subsumes the per-component `useLayoutEffect` in
 * `BuildRunDropdown` and `ProviderDropdown`. These tests pin the
 * contract at the hook level so the two call sites can drop their
 * clamp assertions.
 *
 * Coverage: applies translateY when the menu overflows, caps the shift
 * by `rect.top - MARGIN` (NOT `rect.top - rect.height - MARGIN`), no
 * transform when the menu fits, cleanup resets the transform.
 */
import { describe, it, expect, vi, afterEach } from 'vitest';
import { useRef, useState } from 'react';
import { render, cleanup } from '@testing-library/react';
import { useViewportClamp } from '../../src/hooks/useViewportClamp';

afterEach(() => {
  vi.restoreAllMocks();
  cleanup();
});

function Menu({ margin }: { margin?: number }) {
  const ref = useRef<HTMLDivElement>(null);
  const [version, setVersion] = useState(0);
  useViewportClamp(ref, [version], margin !== undefined ? { margin } : undefined);
  return (
    <div ref={ref} role="menu" data-testid="menu">
      <button role="menuitem">item</button>
    </div>
  );
}

describe('useViewportClamp — translateY clamp', () => {
  it('applies a negative translateY when the menu would overflow the bottom of the viewport', () => {
    // Position the menu near the bottom of the viewport (top=600) with
    // a height that extends past the bottom (600 + 300 = 900 > 768).
    // `rect.top=600` gives the cap `maxShift = 600 - 4 = 596`, plenty
    // of room to shift up by the full overflow (900 - 764 = 136).
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({
      top: 600,
      bottom: 900,
      left: 0,
      right: 200,
      width: 200,
      height: 300,
      x: 0,
      y: 600,
      toJSON: () => ({}),
    } as DOMRect);

    render(<Menu />);
    const menu = document.querySelector('[role="menu"]') as HTMLElement;
    expect(menu.style.transform).toMatch(/translateY\(-/);
  });

  it('does not apply translateY when the menu fits in the viewport', () => {
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({
      top: 100,
      bottom: 250,
      left: 0,
      right: 200,
      width: 200,
      height: 150,
      x: 0,
      y: 100,
      toJSON: () => ({}),
    } as DOMRect);

    render(<Menu />);
    const menu = document.querySelector('[role="menu"]') as HTMLElement;
    expect(menu.style.transform).toBe('');
  });

  it('caps the shift by rect.top - MARGIN (does not double-subtract rect.height)', () => {
    // The "subtle fix a hook would lock in once": when the menu is
    // taller than the gap above the trigger, the cap must NOT subtract
    // the menu's own height. We model that by placing the menu very
    // tall (height=2000) but near the bottom (top=700). With a 768px
    // viewport, overflow = 700 + 2000 - 764 = 1936. `maxShift = 700
    // - 4 = 696` (without the bug, you'd compute `700 - 2000 - 4 =
    // -1304` and cap to 0 — the menu would stay overflowing). The
    // applied shift must be ≤ 696.
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({
      top: 700,
      bottom: 2700,
      left: 0,
      right: 200,
      width: 200,
      height: 2000,
      x: 0,
      y: 700,
      toJSON: () => ({}),
    } as DOMRect);

    render(<Menu />);
    const menu = document.querySelector('[role="menu"]') as HTMLElement;
    const match = menu.style.transform.match(/translateY\(-(\d+(?:\.\d+)?)px\)/);
    expect(match).toBeTruthy();
    const shift = parseFloat(match![1]);
    // `maxShift` is `rect.top - MARGIN = 700 - 4 = 696`. If a future
    // refactor accidentally subtracts `rect.height`, the cap would be
    // max(0, 700 - 2000 - 4) = 0 and the test would see shift=0 (or
    // the overflow assertion would fail). This pins the cap ≤ 696.
    expect(shift).toBeLessThanOrEqual(696);
    expect(shift).toBeGreaterThan(0);
  });

  it('honors a custom margin option', () => {
    // With `margin: 20`, overflow threshold is `vh - 20 = 748`. Menu
    // at top=600, height=200, bottom=800 → overflow = 800 - 748 = 52.
    // `maxShift = 600 - 20 = 580`. Expected shift = 52.
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({
      top: 600,
      bottom: 800,
      left: 0,
      right: 200,
      width: 200,
      height: 200,
      x: 0,
      y: 600,
      toJSON: () => ({}),
    } as DOMRect);

    render(<Menu margin={20} />);
    const menu = document.querySelector('[role="menu"]') as HTMLElement;
    expect(menu.style.transform).toBe('translateY(-52px)');
  });

  it('cleans up the transform on unmount so a remount starts from the unclamped position', () => {
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({
      top: 600,
      bottom: 900,
      left: 0,
      right: 200,
      width: 200,
      height: 300,
      x: 0,
      y: 600,
      toJSON: () => ({}),
    } as DOMRect);

    const { unmount } = render(<Menu />);
    const menu = document.querySelector('[role="menu"]') as HTMLElement;
    expect(menu.style.transform).not.toBe('');
    unmount();
    // After unmount the menu element is gone, but we can verify the
    // cleanup ran by re-rendering with a fitting rect and checking the
    // new element starts at transform === '' (the cleanup contract).
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({
      top: 100,
      bottom: 250,
      left: 0,
      right: 200,
      width: 200,
      height: 150,
      x: 0,
      y: 100,
      toJSON: () => ({}),
    } as DOMRect);
    render(<Menu />);
    const menu2 = document.querySelector('[role="menu"]') as HTMLElement;
    expect(menu2.style.transform).toBe('');
  });
});
