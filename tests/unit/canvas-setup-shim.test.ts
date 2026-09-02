/**
 * Regression test for the `HTMLCanvasElement.prototype.getContext` Proxy
 * stub installed by `tests/setup/vitest.setup.ts` (issue #1386, round-3
 * review follow-up).
 *
 * The shim exists because jsdom does not implement `getContext` and prints
 * `Not implemented: HTMLCanvasElement's getContext()` for every call. The
 * naïve `() => undefined` for every method crashes any consumer that expects
 * a returned object — `ctx.measureText('x').width` would TypeError.
 *
 * Round-3 (grumpy-senior) review surfaced four spec violations the previous
 * shim had. This file pins them all so a regression to a single global
 * Proxy with `null` for `canvas`/`null` for webgl/leaks state across
 * canvases/etc. fails the unit suite instead of corrupting an unrelated
 * component's render path.
 */
import { describe, it, expect } from 'vitest';

describe('vitest.setup.ts canvas shim (issue #1386 + round-3 review)', () => {
  // `document.createElement('canvas')` is the canonical way for tests to
  // obtain a real jsdom canvas element; getContext returns the noop stub
  // because vitest.setup.ts installed it on HTMLCanvasElement.prototype.
  function newCanvasWithCtx(): {
    canvas: HTMLCanvasElement;
    ctx: CanvasRenderingContext2D;
  } {
    const canvas = document.createElement('canvas');
    const ctx = canvas.getContext('2d') as CanvasRenderingContext2D;
    return { canvas, ctx };
  }

  it('measureText returns a TextMetrics shape with `width: 0`', () => {
    const { ctx } = newCanvasWithCtx();
    const metrics = ctx.measureText('any text');
    expect(metrics).toBeDefined();
    expect(metrics.width).toBe(0);
  });

  it('createLinearGradient returns a gradient whose addColorStop is callable', () => {
    const { ctx } = newCanvasWithCtx();
    const gradient = ctx.createLinearGradient(0, 0, 100, 100);
    expect(gradient).toBeDefined();
    expect(() => gradient.addColorStop(0, 'red')).not.toThrow();
  });

  it('createRadialGradient returns a gradient whose addColorStop is callable', () => {
    const { ctx } = newCanvasWithCtx();
    const gradient = ctx.createRadialGradient(50, 50, 0, 50, 50, 100);
    expect(gradient).toBeDefined();
    expect(() => gradient.addColorStop(0, '#000')).not.toThrow();
  });

  it('createPattern returns a Pattern whose setTransform is callable', () => {
    const { ctx } = newCanvasWithCtx();
    const pattern = ctx.createPattern(ctx.canvas, 'repeat');
    expect(pattern).toBeDefined();
    expect(() => pattern!.setTransform({ a: 1 } as DOMMatrix)).not.toThrow();
  });

  it('getImageData returns an ImageData shape with a non-null `data` array', () => {
    const { ctx } = newCanvasWithCtx();
    const img = ctx.getImageData(0, 0, 4, 4);
    expect(img).toBeDefined();
    expect(img.data).toBeInstanceOf(Uint8ClampedArray);
    expect(img.data.length).toBe(4 * 4 * 4); // 4×4 RGBA
    expect(img.width).toBe(4);
    expect(img.height).toBe(4);
  });

  it('createImageData also returns a shape (used by offscreen renderers)', () => {
    const { ctx } = newCanvasWithCtx();
    const img = ctx.createImageData(2, 2);
    expect(img).toBeDefined();
    expect(img.data.length).toBe(2 * 2 * 4);
  });

  it('void methods stay callable and return undefined', () => {
    const { ctx } = newCanvasWithCtx();
    expect(() => ctx.clearRect(0, 0, 100, 100)).not.toThrow();
    expect(() => ctx.fillRect(0, 0, 100, 100)).not.toThrow();
    expect(() => ctx.beginPath()).not.toThrow();
    expect(() => ctx.closePath()).not.toThrow();
    expect(() => ctx.arc(50, 50, 10, 0, Math.PI)).not.toThrow();
  });

  it('property writes (e.g. fillStyle = "red") succeed silently', () => {
    const { ctx } = newCanvasWithCtx();
    expect(() => {
      ctx.fillStyle = 'red';
      ctx.strokeStyle = 'blue';
      ctx.globalAlpha = 0.5;
      ctx.lineWidth = 2;
    }).not.toThrow();
  });

  // ─── Round-3 review fixes (issue #1386 follow-up) ───────────────────

  describe('round-3 spec compliance', () => {
    it('A. ctx.canvas refers to the host HTMLCanvasElement (not null)', () => {
      // Per HTML Living Standard: `CanvasRenderingContext2D.canvas` is the
      // canvas the context was created from. A regression to `null` crashes
      // any consumer that does `const { width, height } = ctx.canvas;`.
      const { canvas, ctx } = newCanvasWithCtx();
      expect(ctx.canvas).toBe(canvas);
      expect(ctx.canvas).not.toBeNull();
    });

    it('B. each canvas gets a fresh context (no singleton leak)', () => {
      // A single global Proxy shared across every canvas means state set
      // on one canvas's "context" leaks to all the others. Per-element
      // construction (round-3 critique B) restores the spec invariant.
      const c1 = document.createElement('canvas');
      const c2 = document.createElement('canvas');
      const ctx1 = c1.getContext('2d') as CanvasRenderingContext2D;
      const ctx2 = c2.getContext('2d') as CanvasRenderingContext2D;
      expect(ctx1).not.toBe(ctx2);
      expect(ctx1.canvas).toBe(c1);
      expect(ctx2.canvas).toBe(c2);
    });

    it('C. getContext returns null for non-2d contextTypes', () => {
      // Chrome returns `null` for `'webgl'`/`'webgl2'`/`'bitmaprenderer'`
      // when the requested backing type isn't available. A 2D Proxy handed
      // to a WebGL caller would silently mis-render — `gl.clearColor(...)`
      // would no-op instead of throwing.
      const canvas = document.createElement('canvas');
      expect(canvas.getContext('webgl')).toBeNull();
      expect(canvas.getContext('webgl2')).toBeNull();
      expect(canvas.getContext('bitmaprenderer')).toBeNull();
      // And the 2d path still works.
      expect(canvas.getContext('2d')).not.toBeNull();
    });

    it('D. getImageData clamps negative dimensions to an empty buffer', () => {
      // A negative width or height to `new Uint8ClampedArray(w*h*4)` throws
      // `RangeError: Invalid array length` deep in an unrelated component.
      // Clamping the negative dim at 0 produces an empty buffer; a positive
      // co-dim passes through unchanged. Spec-compliant for out-of-range
      // inputs without surfacing the V8 throw.
      const { ctx } = newCanvasWithCtx();
      const img = ctx.getImageData(0, 0, -1, 100);
      expect(img).toBeDefined();
      expect(img.data.length).toBe(0); // not -1*100*4 = -400 bytes
      expect(img.width).toBe(0); // the clamped (negative) one
      expect(img.height).toBe(100); // the non-negative one passes through
    });

    it('D. createImageData clamps negative dimensions the same way', () => {
      const { ctx } = newCanvasWithCtx();
      const img = ctx.createImageData(-5, 0);
      expect(img).toBeDefined();
      expect(img.data.length).toBe(0);
    });

    it('getContext(undefined) defaults to 2d (spec fallback)', () => {
      // Per spec, calling `getContext()` with no argument returns the
      // default 2D context. The shim honours that fallback so callers who
      // elide the arg don't silently get `null` and confuse downstream code.
      const canvas = document.createElement('canvas');
      expect(canvas.getContext()).not.toBeNull();
      expect(canvas.getContext()).toBe(canvas.getContext('2d'));
    });
  });
});
