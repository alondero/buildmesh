import { vi } from 'vitest';

// ============================================================
// Tauri API Mocks
// ============================================================

// Mock invoke - used for write_to_agent, list_sessions, etc.
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
  // Fake unicode service: the real xterm has `term.unicode.register(p)` which
  // writes into `term.unicode._providers` keyed by `p.version`, plus a
  // setter on `activeVersion` that activates one of those providers. The
  // loadUnicode11Widths helper reads `_providers['11'].wcwidth` AND
  // `_providers['11'].charProperties` after loadAddon, so the mock has to
  // honour that contract. Tests that don't care about widths (most of
  // them) just check `activeVersion === '11'`.
  // We can't make loadAddon() invoke addon.activate() because the other
  // addons the registry loads (SearchAddon, SerializeAddon, WebLinksAddon)
  // need real terminal methods our mock doesn't provide — so we pre-seed
  // the '11' provider in the constructor instead, which is the only state
  // loadUnicode11Widths actually reads. The helper immediately replaces
  // this entry with its own wrapper, so the seed values are placeholders.
  const mockProviders: Record<string, {
    version: string;
    wcwidth: (cp: number) => number;
    charProperties: (cp: number, preceding: number) => number;
  }> = {
    // The addon's own wcwidth/charProperties would normally populate this
    // key, but the mock short-circuits loadAddon. The helper immediately
    // replaces this entry with its own wrapper, so its identity is
    // irrelevant — what matters is that the keys exist.
    '11': { version: '11', wcwidth: () => 1, charProperties: () => 0 },
  };
  let mockActive = '11';

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
    buffer = { active: { getWindow: vi.fn() } };
    unicode = {
      _providers: mockProviders,
      register: vi.fn((p: {
        version: string;
        wcwidth: (cp: number) => number;
        charProperties: (cp: number, preceding: number) => number;
      }) => {
        mockProviders[p.version] = p;
      }),
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

beforeEach(() => {
  mockListeners.clear();
  vi.clearAllMocks();
});
