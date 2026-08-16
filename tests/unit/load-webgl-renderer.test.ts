/**
 * Issue #1122: focused tests for the WebGL renderer fallback ladder
 * defined in `src/components/Terminal/loadWebglRenderer.ts`. The
 * loader's three failure modes are:
 *
 * 1. `new WebglAddon()` throws — WebGL not supported on this host
 *    (headless WebView2, no GPU, remote desktop without GPU passthrough).
 * 2. The constructor succeeds but the GL context is lost after attach
 *    (driver reset, RDP reconnect, GPU hibernate).
 * 3. The constructor succeeds, attaches, and stays attached (happy
 *    path — pinned by the registry's load-addon flow).
 *
 * Each mode is exercised below. The mocks are module-level
 * (`vi.mock('@xterm/addon-webgl', ...)`) so the production loader
 * imports the mock class; `mockFactory` controls whether the constructor
 * succeeds or throws per test, and the term's `loadAddon` callback
 * captures the addon so the test can invoke `onContextLoss` directly.
 *
 * Run with: npm test -- --run tests/unit/load-webgl-renderer.test.ts
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import type { Terminal } from '@xterm/xterm';

interface MockWebglAddon {
  dispose: ReturnType<typeof vi.fn>;
  onContextLoss: ReturnType<typeof vi.fn>;
}

/**
 * Test-internal factory controlling what `new WebglAddon()` returns. The
 * factory is wired to the `vi.mock` below so changing it in `beforeEach`
 * affects the next `new WebglAddon()` call. When `mockFactory` throws,
 * the loader must catch the error and fall back to the DOM renderer.
 * When it returns an object, the term's `loadAddon` callback captures
 * the addon so the test can simulate a context loss.
 */
let mockFactory: () => MockWebglAddon = () => {
  throw new Error('WebGL not supported');
};

vi.mock('@xterm/addon-webgl', () => {
  return {
    WebglAddon: class MockWebglAddon {
      dispose = vi.fn();
      onContextLoss = vi.fn();
      constructor() {
        // Return the factory's result (which may throw). The throwing
        // case propagates to the loader's catch branch; the success
        // case is captured by the term's `loadAddon` for the test to
        // exercise.
        return mockFactory();
      }
    },
  };
});

import { loadWebglRenderer } from '../../src/components/Terminal/loadWebglRenderer';

function makeMockTerm() {
  const term = {
    loadAddon: vi.fn(),
  } as unknown as Terminal;
  return term;
}

function makeAddonCapturingTerm() {
  // Returns the term + a handle that invokes the `onContextLoss`
  // callback the loader registered, so the test can simulate a
  // runtime GPU context loss.
  const term = {
    loadAddon: vi.fn(),
  } as unknown as Terminal;
  let capturedAddon: MockWebglAddon | null = null;
  (term.loadAddon as ReturnType<typeof vi.fn>).mockImplementation((addon: MockWebglAddon) => {
    capturedAddon = addon;
  });
  return {
    term,
    capturer: () => {
      if (!capturedAddon) throw new Error('loadAddon was not called');
      return capturedAddon;
    },
  };
}

describe('loadWebglRenderer (issue #1122 fallback ladder)', () => {
  beforeEach(() => {
    // Default: `new WebglAddon()` throws (no WebGL). Tests that
    // expect success override this in their body.
    mockFactory = () => {
      throw new Error('WebGL not supported');
    };
  });

  it('attaches the addon when the constructor succeeds (happy path)', () => {
    const onContextLoss = vi.fn();
    mockFactory = () => ({ dispose: vi.fn(), onContextLoss });
    const term = makeMockTerm();
    loadWebglRenderer(term);
    expect(term.loadAddon).toHaveBeenCalledTimes(1);
    expect(onContextLoss).toHaveBeenCalledTimes(1);
  });

  it('falls back to DOM silently when new WebglAddon throws', () => {
    // Default `beforeEach` impl: throws. The production loader
    // catches and warns; the test asserts the loadAddon was NEVER
    // called (no broken addon attached) and the console was warned.
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    try {
      const term = makeMockTerm();
      expect(() => loadWebglRenderer(term)).not.toThrow();
      expect(term.loadAddon).not.toHaveBeenCalled();
      expect(warnSpy).toHaveBeenCalledWith(
        expect.stringContaining('WebGL renderer unavailable'),
        expect.any(Error),
      );
    } finally {
      warnSpy.mockRestore();
    }
  });

  it('disposes the addon when the context is lost after attach', () => {
    const dispose = vi.fn();
    mockFactory = () => ({ dispose, onContextLoss: vi.fn() });
    const { term, capturer } = makeAddonCapturingTerm();
    loadWebglRenderer(term);
    expect(term.loadAddon).toHaveBeenCalledTimes(1);
    // Simulate the GPU context loss — the loader's registered
    // callback calls `addon.dispose()` to drop the addon so xterm
    // falls back to the DOM renderer.
    const addon = capturer();
    addon.onContextLoss.mock.calls[0]?.[0]();
    expect(dispose).toHaveBeenCalledTimes(1);
  });

  it('does not double-dispose when the context-loss callback fires twice', () => {
    const dispose = vi.fn();
    mockFactory = () => ({ dispose, onContextLoss: vi.fn() });
    const { term, capturer } = makeAddonCapturingTerm();
    loadWebglRenderer(term);
    const addon = capturer();
    const cb = addon.onContextLoss.mock.calls[0]?.[0];
    expect(cb).toBeDefined();
    // First loss — wipes the addon.
    cb();
    expect(dispose).toHaveBeenCalledTimes(1);
    // Second loss — the loader's internal `webgl = null` reset
    // must prevent a second dispose call from reaching the addon.
    cb();
    expect(dispose).toHaveBeenCalledTimes(1);
  });

  it('cleans up the addon reference on context loss so a subsequent reload is safe', () => {
    // After a context loss, the loader nulls its internal `webgl`
    // reference. If the same Terminal's `loadWebglRenderer` is
    // called again (e.g. after a renderer swap), the new load
    // constructs a fresh addon and re-registers onContextLoss.
    // No exception should escape.
    const firstDispose = vi.fn();
    const secondDispose = vi.fn();
    let firstSingleton = true;
    mockFactory = () => {
      if (firstSingleton) {
        firstSingleton = false;
        return { dispose: firstDispose, onContextLoss: vi.fn() };
      }
      return { dispose: secondDispose, onContextLoss: vi.fn() };
    };
    const term = makeMockTerm();
    loadWebglRenderer(term);
    // Capture the first addon's onContextLoss callback.
    const firstCallArg = (term.loadAddon as ReturnType<typeof vi.fn>).mock.calls[0]?.[0];
    const firstCb = firstCallArg.onContextLoss.mock.calls[0]?.[0];
    expect(firstCb).toBeDefined();
    firstCb();
    expect(firstDispose).toHaveBeenCalledTimes(1);
    // A second load constructs a fresh addon — the old dispose
    // must NOT fire again.
    loadWebglRenderer(term);
    expect(firstDispose).toHaveBeenCalledTimes(1);
    expect(secondDispose).not.toHaveBeenCalled();
  });
});
