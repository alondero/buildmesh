import { vi } from 'vitest';

// ============================================================
// Tauri API Mocks
// ============================================================

// Mock invoke - used for write_to_agent, list_agent_nodes, etc.
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn().mockResolvedValue({}),
}));

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

    constructor(_options?: unknown) {
      // Accept options but don't use them in mock
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
  const { resetPathInvalidatedCacheForTests } = await import(
    '../../src/lib/pathInvalidatedCache'
  );
  resetPathInvalidatedCacheForTests();
});
