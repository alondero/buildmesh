/**
 * QuickConnectMenu — placement inside the visible Circuit editor
 * (issue #2773).
 *
 * The menu used to be placed at a fixed offset from the drop point, with
 * no collision calculation. A connection dropped near the bottom or the
 * right edge of the canvas put the search box and the results outside the
 * editor, where the user could not reach them.
 *
 * These tests pin the geometry the user sees: the menu stays inside the
 * visible canvas at every corner, it flips above the drop point when
 * there is no room below, it shifts left when the drop is near the right
 * edge, and its own list scrolls when the editor is short.
 */
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import { createRef } from 'react';
import { QuickConnectMenu } from '../../src/components/Circuits/QuickConnectMenu';
import type { NodeKindSpec } from '../../src/components/Circuits/circuitGraphModel';

const MENU_WIDTH = 208;
const MENU_HEIGHT = 220;

/** Restores the prototype overrides a test installed. */
let restorePrototype: (() => void) | null = null;

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  restorePrototype?.();
  restorePrototype = null;
});

/** A canvas of `width` x `height` pixels, with a menu of a known size. */
function renderMenu(
  position: { x: number; y: number },
  bounds: { width: number; height: number },
  onSelect: (spec: NodeKindSpec) => void = () => {},
) {
  // The geometry the component measures is stubbed before the render, so
  // the menu's own layout effect sees a canvas with a real size.
  vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockImplementation(function (this: HTMLElement) {
    return this.dataset.testid === 'canvas' ? bounds.width : 0;
  });
  vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
    return this.dataset.testid === 'canvas' ? bounds.height : 0;
  });
  vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockImplementation(function (this: HTMLElement) {
    return this.dataset.testid === 'quick-connect-menu' ? MENU_WIDTH : 0;
  });
  vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockImplementation(function (this: HTMLElement) {
    return this.dataset.testid === 'quick-connect-menu' ? MENU_HEIGHT : 0;
  });
  const originalOffsetParent = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'offsetParent');
  Object.defineProperty(HTMLElement.prototype, 'offsetParent', {
    configurable: true,
    get(this: HTMLElement) {
      const parent = this.parentElement;
      return parent?.dataset.testid === 'canvas' ? parent : null;
    },
  });
  const boundsRef = createRef<HTMLDivElement>();
  render(
    <div ref={boundsRef} data-testid="canvas">
      <QuickConnectMenu position={position} onSelect={onSelect} onDismiss={() => {}} boundsRef={boundsRef} />
    </div>,
  );
  restorePrototype = () => {
    if (originalOffsetParent) {
      Object.defineProperty(HTMLElement.prototype, 'offsetParent', originalOffsetParent);
    } else {
      delete (HTMLElement.prototype as unknown as Record<string, unknown>).offsetParent;
    }
  };
}

function menu(): HTMLElement {
  return screen.getByTestId('quick-connect-menu');
}

function menuBox(): { left: number; top: number; maxHeight: number; width: number } {
  const element = menu();
  return {
    left: Number.parseFloat(element.style.left),
    top: Number.parseFloat(element.style.top),
    maxHeight: Number.parseFloat(element.style.maxHeight),
    width: Number.parseFloat(element.style.maxWidth),
  };
}

describe('QuickConnectMenu placement', () => {
  it('keeps the menu inside the canvas at the bottom-right corner', () => {
    const bounds = { width: 800, height: 600 };
    renderMenu({ x: 790, y: 590 }, bounds);
    const box = menuBox();
    expect(box.left + MENU_WIDTH).toBeLessThanOrEqual(bounds.width - 8);
    expect(box.top + MENU_HEIGHT).toBeLessThanOrEqual(bounds.height - 8);
  });

  it('keeps the menu inside the canvas at the bottom-left corner', () => {
    const bounds = { width: 800, height: 600 };
    renderMenu({ x: 4, y: 590 }, bounds);
    const box = menuBox();
    expect(box.left).toBeGreaterThanOrEqual(8);
    expect(box.top + MENU_HEIGHT).toBeLessThanOrEqual(bounds.height - 8);
  });

  it('keeps the menu inside the canvas at the top-right corner', () => {
    const bounds = { width: 800, height: 600 };
    renderMenu({ x: 790, y: 4 }, bounds);
    const box = menuBox();
    expect(box.left + MENU_WIDTH).toBeLessThanOrEqual(bounds.width - 8);
    expect(box.top).toBeGreaterThanOrEqual(8);
  });

  it('keeps the menu inside the canvas at the top-left corner', () => {
    const bounds = { width: 800, height: 600 };
    renderMenu({ x: 4, y: 4 }, bounds);
    const box = menuBox();
    expect(box.left).toBeGreaterThanOrEqual(8);
    expect(box.top).toBeGreaterThanOrEqual(8);
  });

  it('flips the menu above the drop point when there is no room below', () => {
    const bounds = { width: 800, height: 600 };
    renderMenu({ x: 400, y: 560 }, bounds);
    const box = menuBox();
    // The drop point is at the bottom, so the menu opens upwards.
    expect(box.top + MENU_HEIGHT).toBeLessThanOrEqual(560);
    expect(box.top).toBeLessThan(560);
  });

  it('bounds the menu height and keeps the search box when the editor is short', () => {
    // A 200px-tall editor leaves 170px above the drop point — less than
    // the menu's 220px — so the menu's height is capped and its result
    // list scrolls inside it.
    const bounds = { width: 800, height: 200 };
    renderMenu({ x: 400, y: 190 }, bounds);
    const box = menuBox();
    expect(box.maxHeight).toBeLessThan(MENU_HEIGHT);
    expect(box.maxHeight).toBeGreaterThan(0);
    expect(box.top + box.maxHeight).toBeLessThanOrEqual(bounds.height - 8);
    // The search box stays outside the scrolling list.
    expect(screen.getByTestId('quick-connect-input')).toBeTruthy();
  });

  it('caps the menu width to the canvas', () => {
    const bounds = { width: 260, height: 600 };
    renderMenu({ x: 120, y: 300 }, bounds);
    expect(menuBox().width).toBeLessThanOrEqual(bounds.width - 16);
  });

  it('keeps the search box and the results reachable', () => {
    renderMenu({ x: 400, y: 300 }, { width: 800, height: 600 });
    const input = screen.getByTestId('quick-connect-input');
    expect(input).toBeTruthy();
    fireEvent.change(input, { target: { value: 'notify' } });
    expect(screen.getByTestId('quick-connect-notify')).toBeTruthy();
  });

  it('still creates the node the user picked', () => {
    const onSelect = vi.fn();
    renderMenu({ x: 400, y: 300 }, { width: 800, height: 600 }, onSelect);
    fireEvent.click(screen.getByTestId('quick-connect-notify'));
    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(onSelect.mock.calls[0][0].discriminator).toBe('notify');
  });
});
