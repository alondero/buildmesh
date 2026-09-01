/**
 * Regression test for the `HTMLCanvasElement.prototype.getContext` Proxy
 * stub installed by `tests/setup/vitest.setup.ts` (issue #1386).
 *
 * The shim exists because jsdom does not implement `getContext` and prints
 * `Not implemented: HTMLCanvasElement's getContext()` for every call. The
 * naïve `() => undefined` for every method crashes any consumer that expects
 * a returned object — `ctx.measureText('x').width` would TypeError. The
 * follow-up fix returns minimal stub objects for the chained-return methods
 * consumers depend on (`measureText` -> TextMetrics,
 * `createLinearGradient`/`RadialGradient` -> gradient shape, `getImageData`
 * -> ImageData shape, etc).
 *
 * Each test below pins a single chained-call contract; a regression that
 * returns `undefined` for one of these methods would fail the assertion
 * below and surface as a unit test failure, not a mysterious runtime
 * TypeError inside an unrelated component.
 */
import { describe, it, expect } from 'vitest';

describe('vitest.setup.ts canvas shim (issue #1386 follow-up)', () => {
  // The shim is installed in vitest.setup.ts before any test runs; we just
  // grab it here. `document.createElement('canvas')` is the canonical way
  // for tests to obtain a real jsdom canvas element.
  function ctx(): CanvasRenderingContext2D {
    const canvas = document.createElement('canvas');
    return canvas.getContext('2d') as CanvasRenderingContext2D;
  }

  it('measureText returns a TextMetrics shape with `width: 0`', () => {
    const metrics = ctx().measureText('any text');
    // The most-consumed field; a regression to `undefined` would throw here.
    expect(metrics).toBeDefined();
    expect(metrics.width).toBe(0);
  });

  it('createLinearGradient returns a gradient whose addColorStop is callable', () => {
    const gradient = ctx().createLinearGradient(0, 0, 100, 100);
    expect(gradient).toBeDefined();
    // A regression to `() => undefined` for createLinearGradient would
    // throw `Cannot read properties of undefined` here.
    expect(() => gradient.addColorStop(0, 'red')).not.toThrow();
  });

  it('createRadialGradient returns a gradient whose addColorStop is callable', () => {
    const gradient = ctx().createRadialGradient(50, 50, 0, 50, 50, 100);
    expect(gradient).toBeDefined();
    expect(() => gradient.addColorStop(0, '#000')).not.toThrow();
  });

  it('createPattern returns a Pattern whose setTransform is callable', () => {
    const pattern = ctx().createPattern(ctx().canvas, 'repeat');
    expect(pattern).toBeDefined();
    // CanvasPattern exposes `setTransform`; the gradient fallback inherits
    // it from the no-op proxy so this stays inert for either shape.
    expect(() => pattern!.setTransform({ a: 1 } as DOMMatrix)).not.toThrow();
  });

  it('getImageData returns an ImageData shape with a non-null `data` array', () => {
    const img = ctx().getImageData(0, 0, 4, 4);
    expect(img).toBeDefined();
    expect(img.data).toBeInstanceOf(Uint8ClampedArray);
    expect(img.data.length).toBe(4 * 4 * 4); // 4×4 RGBA
    expect(img.width).toBe(4);
    expect(img.height).toBe(4);
  });

  it('createImageData also returns a shape (used by offscreen renderers)', () => {
    const img = ctx().createImageData(2, 2);
    expect(img).toBeDefined();
    expect(img.data.length).toBe(2 * 2 * 4);
  });

  it('void methods stay callable and return undefined', () => {
    // These were the original no-op-fn cases. They must remain callable
    // so a `clearRect()` or `fillRect()` call doesn't TypeError.
    const c = ctx();
    expect(() => c.clearRect(0, 0, 100, 100)).not.toThrow();
    expect(() => c.fillRect(0, 0, 100, 100)).not.toThrow();
    expect(() => c.beginPath()).not.toThrow();
    expect(() => c.closePath()).not.toThrow();
    expect(() => c.arc(50, 50, 10, 0, Math.PI)).not.toThrow();
  });

  it('property writes (e.g. fillStyle = "red") succeed silently', () => {
    // The Proxy `set` trap swallows assignments. A regression that
    // returned a primitive would surface here as "Cannot set properties
    // of undefined".
    const c = ctx();
    expect(() => {
      c.fillStyle = 'red';
      c.strokeStyle = 'blue';
      c.globalAlpha = 0.5;
      c.lineWidth = 2;
    }).not.toThrow();
  });
});
