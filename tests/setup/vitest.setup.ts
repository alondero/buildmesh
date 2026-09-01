import { vi } from 'vitest';
import { cleanup } from '@testing-library/react';

// ============================================================
// jsdom canvas shim (issue #1386)
// ============================================================

// jsdom does not implement `HTMLCanvasElement.getContext`; without a stub it
// prints `Not implemented: HTMLCanvasElement's getContext()` once per call.
// The full vitest suite fires that warning ~74× and the issue body listed
// "Vitest suite has zero console warnings" as the acceptance criterion.
//
// PR review catch (grumpy-senior): a naïve `() => undefined` for every
// method crashes any consumer that expects a returned object —
// `ctx.measureText('x').width` would TypeError. The Proxy here returns
// minimal stub objects for the chained-return methods consumers depend on:
//
//   measureText(...)           -> TextMetrics shape (width: 0, ...)
//   createLinearGradient(...)  -> { addColorStop: () => undefined } (proxy)
//   createRadialGradient(...)  -> { addColorStop: () => undefined } (proxy)
//   createPattern(...)         -> { setTransform: () => undefined } (proxy)
//   getImageData/putImageData  -> ImageData shape (data: Uint8ClampedArray)
//   createImageData(...)       -> ImageData shape
//
// Everything else is a void-returning no-op (`fillRect`, `drawImage`,
// `beginPath`, etc). We deliberately do NOT depend on the `canvas` npm
// package: it ships a native binary that breaks on Buildmesh's CI
// Linux/musl runners. Anything that actually wants bitmap assertions in a
// test can override `HTMLCanvasElement.prototype.getContext` locally.
type CanvasContext = Record<string | symbol, unknown>;

/**
 * A `CanvasGradient`-shaped proxy: factory methods like `createLinearGradient`
 * return objects whose only common method is `addColorStop(0, 'red')`. The
 * proxy returns `undefined` for property reads (e.g. `gradient.addColorStop`)
 * via the same no-op-fn fallback used by the context, so further chained calls
 * stay inert.
 */
function noopCanvasGradient(): CanvasContext {
  return new Proxy(
    {},
    {
      get() {
        return () => undefined;
      },
      set() {
        return true;
      },
    },
  );
}

/**
 * A `CanvasPattern`-shaped proxy. Same shape as a gradient from a consumer's
 * standpoint — `setTransform` is the documented method, the rest is inert.
 */
function noopCanvasPattern(): CanvasContext {
  return noopCanvasGradient();
}

/** `TextMetrics` minimum: `width` is the only field most consumers read. */
function noopTextMetrics(): TextMetrics {
  return {
    width: 0,
    actualBoundingBoxLeft: 0,
    actualBoundingBoxRight: 0,
    actualBoundingBoxAscent: 0,
    actualBoundingBoxDescent: 0,
    alphabeticBaseline: 0,
    emHeightAscent: 0,
    emHeightDescent: 0,
    fontBoundingBoxAscent: 0,
    fontBoundingBoxDescent: 0,
    hangingBaseline: 0,
    ideographicBaseline: 0,
  } as TextMetrics;
}

/** `ImageData` minimum: consumers usually read `.data` and `.width`. */
function noopImageData(width = 0, height = 0): ImageData {
  return {
    data: new Uint8ClampedArray(width * height * 4),
    width,
    height,
    colorSpace: 'srgb',
  } as ImageData;
}

/**
 * The main `CanvasRenderingContext2D` stub. Property reads return sensible
 * defaults; method calls return either `undefined` (for void methods like
 * `fillRect`) or a minimal stub object (for factory methods whose return
 * value is consumed downstream). The closure over the four stub factories
 * means `getImageData(...)`, `createLinearGradient(...)`, etc. all produce
 * fresh objects per call — no shared state between consumers.
 */
const noopCanvasContext = new Proxy(
  {} as CanvasContext,
  {
    get(_target, prop) {
      // Read-only-ish properties.
      if (prop === 'canvas') return null;
      if (prop === 'font') return '10px sans-serif';
      if (prop === 'fillStyle' || prop === 'strokeStyle') return '#000';
      if (prop === 'globalAlpha' || prop === 'globalCompositeOperation') return 1;
      if (prop === 'lineWidth' || prop === 'lineDashOffset') return 1;

      // Chained-return methods — return the right *shape* so consumers that
      // read properties or call further methods don't TypeError.
      if (prop === 'measureText') return () => noopTextMetrics();
      if (prop === 'createLinearGradient' || prop === 'createRadialGradient') {
        return () => noopCanvasGradient();
      }
      if (prop === 'createPattern') return () => noopCanvasPattern();
      if (prop === 'getImageData') {
        // CanvasRenderingContext2D.getImageData(sx, sy, sw, sh) — width
        // and height are the 3rd and 4th positional args, not the 1st/2nd.
        return (_sx: number, _sy: number, sw?: number, sh?: number) =>
          noopImageData(sw ?? 0, sh ?? 0);
      }
      if (prop === 'createImageData') {
        // CanvasRenderingContext2D.createImageData(width, height) — width
        // and height are the 1st and 2nd positional args.
        return (sw?: number, sh?: number) => noopImageData(sw ?? 0, sh ?? 0);
      }
      if (prop === 'putImageData') return () => undefined;
      if (prop === 'getLineDash') return () => [];
      if (prop === 'getTransform') return () => ({ a: 1, b: 0, c: 0, d: 1, e: 0, f: 0 });

      // Everything else — known methods (`fillRect`, `drawImage`, `arcTo`,
      // `ellipse`, ...) and properties we haven't enumerated — becomes a
      // callable no-op that returns `undefined`. A missing method should be
      // inert, never a TypeError.
      return () => undefined;
    },
    // Writes (`ctx.fillStyle = 'red'`) succeed silently — the stub doesn't
    // persist anything but accepting the assignment keeps consumer code
    // from seeing "Cannot set properties of undefined".
    set() {
      return true;
    },
  },
);

if (typeof HTMLCanvasElement !== 'undefined') {
  // jsdom's typings lie about where `getContext` lives — installing on the
  // prototype is the path every test render hits. Tests that need a richer
  // mock can override per-instance.
  (HTMLCanvasElement.prototype as { getContext: unknown }).getContext = function () {
    return noopCanvasContext;
  };
}

// ============================================================
// Tauri API Mocks
// ============================================================

// Mock invoke - used for write_to_agent, list_agent_nodes, etc.
vi.mock('@tauri-apps/api/core', async () => {
  const { MockChannel } = await import('./tauriChannel');
  return {
    invoke: vi.fn().mockResolvedValue({}),
    Channel: MockChannel,
  };
});

// Mock window API - used for focus tracking (onFocusChanged)
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: vi.fn(() => ({
    isFocused: vi.fn().mockResolvedValue(true),
    onFocusChanged: vi.fn().mockResolvedValue(() => {}),
  })),
}));

// Mock event system
const mockListeners = new Map<string, Set<(...args: unknown[]) => void>>();

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockImplementation(<T>(event: string, callback: (event: { payload: T }) => void) => {
    if (!mockListeners.has(event)) {
      mockListeners.set(event, new Set());
    }
    mockListeners.get(event)!.add(callback as (...args: unknown[]) => void);
    return Promise.resolve(() => mockListeners.get(event)?.delete(callback as (...args: unknown[]) => void));
  }),
  emit: vi.fn().mockImplementation((event: string, payload?: unknown) => {
    const listeners = mockListeners.get(event);
    if (listeners) {
      listeners.forEach(cb => cb({ payload }));
    }
    return Promise.resolve();
  }),
}));

// ============================================================
// xterm.js Mocks
// ============================================================

// Mock Terminal class - must work with 'new Terminal(options)'
vi.mock('@xterm/xterm', () => {
  // Mirror the real @xterm/xterm shape: the user-facing `unicode` is a thin
  // proxy whose `register` delegates to the internal `UnicodeService` and
  // which exposes `activeVersion` via getter/setter. The real provider map
  // (`_providers`) lives on the internal service behind `_core`, NOT on
  // the user-facing `unicode` — modelling it correctly here is what
  // prevented the loadUnicode11Widths regression from passing its tests.
  // The mock's `loadAddon` is a no-op because we can't synthesise the
  // addon's `activate` (other addons in the registry need real terminal
  // methods we don't provide) — the helper itself pre-seeds the slot it
  // expects to read from. Tests that care about the wrapper's behaviour
  // exercise it via a dedicated mock in load-unicode11-widths.*.test.ts.
  const mockProviders: Record<string, {
    version: string;
    wcwidth: (cp: number) => number;
    charProperties: (cp: number, preceding: number) => number;
  }> = {
    // Pre-seed the '11' provider so getInternalService(...)._providers['11']
    // exists when loadUnicode11Widths reads it (the real addon's activate
    // populates this slot; in the mock we skip activate and seed directly).
    // The helper replaces this entry with its own wrapper, so the seed
    // values are placeholders.
    '11': { version: '11', wcwidth: () => 1, charProperties: () => 0 },
  };
  let mockActive = '11';

  const internalService = {
    _providers: mockProviders,
    register: vi.fn((p: {
      version: string;
      wcwidth: (cp: number) => number;
      charProperties: (cp: number, preceding: number) => number;
    }) => {
      mockProviders[p.version] = p;
    }),
  };

  class MockTerminal {
    write = vi.fn();
    onData = vi.fn();
    onResize = vi.fn();
    onTitleChange = vi.fn();
    open = vi.fn();
    resize = vi.fn();
    dispose = vi.fn();
    focus = vi.fn();
    loadAddon = vi.fn();
    registerCharacterJoiner = vi.fn();
    attachCustomKeyEventHandler = vi.fn();
    scrollToBottom = vi.fn();
    refresh = vi.fn();
    onScroll = vi.fn(() => ({ dispose: vi.fn() }));
    buffer = { active: { getWindow: vi.fn(), viewportY: 0, baseY: 0 } };
    // User-facing proxy: NO `_providers`, has `register` (a passthrough to
    // the internal service) and `activeVersion` getter/setter.
    unicode = {
      register: vi.fn((p: {
        version: string;
        wcwidth: (cp: number) => number;
        charProperties: (cp: number, preceding: number) => number;
      }) => internalService.register(p)),
      get activeVersion() {
        return mockActive;
      },
      set activeVersion(v: string) {
        if (!mockProviders[v]) {
          throw new Error(`unknown Unicode version "${v}"`);
        }
        mockActive = v;
      },
    };
    // The internal service lives behind `_core.unicodeService` in real xterm.
    _core = { unicodeService: internalService };
    rows = 24;
    cols = 80;
    element: HTMLElement | null = null;
    // Issue #734: ThemeManager writes the active xterm.js palette into
    // `term.options.theme` on every theme flip. The real Terminal class
    // stores its constructor options here; the mock previously dropped
    // them. Without this, the theme-toggle tests crash with
    // "Cannot set properties of undefined (setting 'theme')". Defaulting
    // to {} keeps any pre-existing test that ignores options unaffected.
    options: Record<string, unknown> = {};

    constructor(options?: Record<string, unknown>) {
      if (options) this.options = options;
    }
  }

  return {
    Terminal: MockTerminal,
  };
});

vi.mock('@xterm/addon-unicode11', () => {
  class MockUnicode11Addon {
    dispose = vi.fn();
  }
  return { Unicode11Addon: MockUnicode11Addon };
});

// ============================================================
// xterm/addon-fit Mock
// ============================================================

vi.mock('@xterm/addon-fit', () => {
  class MockFitAddon {
    fit = vi.fn();
    dispose = vi.fn();
    proposeDimensions = vi.fn().mockReturnValue({ cols: 80, rows: 24 });
  }

  return {
    FitAddon: MockFitAddon,
  };
});

// ============================================================
// Global test utilities
// ============================================================

beforeEach(async () => {
  mockListeners.clear();
  vi.clearAllMocks();
  // The pathInvalidatedCache primitive installs ONE process-wide
  // GIT_CHANGED listener (intentionally — it lives for the whole process
  // in production). The setup's `mockListeners.clear()` above also wipes
  // that listener from the mock set, but the primitive's own
  // `listenerInstalled` flag stays true, so the next mount's `subscribe`
  // would short-circuit and the bus would silently stop firing.
  // Resetting the primitive alongside the mock listeners keeps the
  // "primitive is fresh between tests" contract automatic. See issues
  // #345, #354.
  //
  // Imported dynamically (rather than at the top of the file) so the
  // primitive — and its transitive `paths.ts` → `platform.ts` chain —
  // doesn't load before per-test `vi.mock` factories are hoisted. A
  // top-level import would cause `platform.ts` to be cached with the
  // real `navigator.platform` value, defeating the test files that
  // mock it to force `isWindows = true` (issue #354 follow-up).
  const [{ resetPathInvalidatedCacheForTests }, { resetProviderCachesForTests }] =
    await Promise.all([
      import('../../src/lib/pathInvalidatedCache'),
      import('../../src/lib/providerCache'),
    ]);
  resetPathInvalidatedCacheForTests();
  resetProviderCachesForTests();
});

// React Testing Library does not auto-unmount rendered components between
// tests by default. Without an explicit `cleanup()`, a component mounted
// in test N is still in the React fiber tree when test N+1 starts. Async
// store mutations fired in test N (e.g. `deleteAgentNode`'s Phase 2
// `setState` that removes a row from `nodesById` after the test's
// `waitFor` resolved on Phase 0 — see tests/unit/grid-node-header-
// responsive.test.tsx's "toggles Close from the kebab item") land during
// test N+1's setup and re-render the *orphaned* fiber with `node =
// undefined`. React then trips "Rendered fewer hooks than expected" on
// the next render attempt — and vitest reports the unhandled error
// against test N+1's file even though the actual cause was the leaked
// render from test N.
afterEach(() => {
  cleanup();
});
