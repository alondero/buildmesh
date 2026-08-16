/**
 * Integration tests for TerminalContainer and session management.
 *
 * Run with: npm test -- --run tests/integration/terminal-container.test.tsx
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { terminalManager } from '../../src/components/Terminal/Terminal';
import { useSessionStore } from '../../src/stores/sessionStore';

// ============================================================
// Mocks for Tauri API
// ============================================================
// `vi.hoisted` runs before the imports — necessary because the SUT import
// above triggers the TerminalRegistry constructor, which now eagerly
// subscribes to Tauri events (see issue #332 / agent-spawned reconcile).
// Without hoisting, the factory's closure over `mockListeners` would hit
// TDZ when the constructor calls listen() during module load.
const mockListeners = vi.hoisted(() => new Map<string, Set<(...args: unknown[]) => void>>());

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockImplementation((event: string, callback: (event: { payload: unknown }) => void) => {
    if (!mockListeners.has(event)) mockListeners.set(event, new Set());
    mockListeners.get(event)!.add(callback as (...args: unknown[]) => void);
    return Promise.resolve(() => mockListeners.get(event)?.delete(callback as (...args: unknown[]) => void));
  }),
  emit: vi.fn().mockImplementation((event: string, payload?: unknown) => {
    mockListeners.get(event)?.forEach(cb => cb({ payload }));
    return Promise.resolve();
  }),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn().mockResolvedValue({}),
}));

vi.mock('@xterm/xterm', () => {
  // Mirror the real @xterm/xterm shape: the user-facing `unicode` is a
  // thin proxy whose `register` delegates to the internal UnicodeService
  // on `_core.unicodeService` (which holds the `_providers` map). The
  // pre-seeded `mockProviders['11']` stands in for what the addon's
  // `activate` populates in production. See
  // tests/unit/load-unicode11-widths.runtime-shape.test.ts for the full
  // contract this mock is satisfying.
  const mockProviders: Record<string, {
    version: string;
    wcwidth: (cp: number) => number;
    charProperties: (cp: number, preceding: number) => number;
  }> = {
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
    onTitleChange = vi.fn();
    onResize = vi.fn();
    open = vi.fn();
    dispose = vi.fn();
    focus = vi.fn();
    loadAddon = vi.fn();
    attachCustomKeyEventHandler = vi.fn();
    scrollToBottom = vi.fn();
    refresh = vi.fn();
    buffer = { active: { getWindow: vi.fn() } };
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
    constructor(_options?: unknown) {}
  }
  return { Terminal: MockTerminal };
});

vi.mock('@xterm/addon-fit', () => {
  class MockFitAddon {
    fit = vi.fn();
    dispose = vi.fn();
    proposeDimensions = vi.fn().mockReturnValue({ cols: 80, rows: 24 });
  }
  return { FitAddon: MockFitAddon };
});

// ============================================================
// TerminalManager Tests
// ============================================================
describe('TerminalManager', () => {
  beforeEach(() => {
    mockListeners.clear();
    vi.clearAllMocks();
    terminalManager.dispose(1);
    terminalManager.dispose(2);
    terminalManager.dispose(3);
  });

  afterEach(() => {
    terminalManager.dispose(1);
    terminalManager.dispose(2);
    terminalManager.dispose(3);
  });

  it('getOrCreate returns instance for same nodeId', async () => {
    const instance = await terminalManager.getOrCreate(1);
    expect(instance).not.toBeNull();
    const instance2 = await terminalManager.getOrCreate(1);
    expect(instance2).toBe(instance);
  });

  it('concurrent getOrCreate calls return same instance (pending map prevents duplicates)', async () => {
    terminalManager.dispose(1);
    const [r1, r2, r3] = await Promise.all([
      terminalManager.getOrCreate(1),
      terminalManager.getOrCreate(1),
      terminalManager.getOrCreate(1),
    ]);
    expect(r1).toBe(r2);
    expect(r2).toBe(r3);
  });

  it('dispose removes instance from manager', async () => {
    const instance = await terminalManager.getOrCreate(1);
    expect(instance).not.toBeNull();
    terminalManager.dispose(1);
    // After dispose, a new getOrCreate should create a fresh instance
    const fresh = await terminalManager.getOrCreate(1);
    expect(fresh).not.toBeNull();
    // The fresh instance should be a different object reference
    expect(fresh).not.toBe(instance);
  });

  it('write after open() works correctly', async () => {
    const instance = await terminalManager.getOrCreate(2);
    expect(instance).not.toBeNull();
    const container = document.createElement('div');
    instance!.term.open(container);
    instance!.term.write('test content');
    expect(instance!.term.write).toHaveBeenCalledWith('test content');
  });
});

// ============================================================
// Event Listener Tests
// ============================================================
describe('Event Listener Integration', () => {
  // Issue #1122: TerminalWriter's default flush scheduler changed from
  // `requestAnimationFrame` to `queueMicrotask` (lower first-flush
  // latency). rAF is driven by `vi.runAllTimers()`, but microtasks
  // need an explicit microtask-checkpoint flush. Awaiting a resolved
  // promise drains the microtask queue without also advancing other
  // timers, so we can call it after `vi.runAllTimers()` to flush
  // whichever scheduler the writer is using.
  const flushScheduledWork = async (): Promise<void> => {
    vi.runAllTimers();
    await Promise.resolve();
    await Promise.resolve();
  };

  beforeEach(() => {
    mockListeners.clear();
    vi.clearAllMocks();
    vi.useFakeTimers();
    terminalManager.dispose(1);
  });

  afterEach(() => {
    vi.useRealTimers();
    terminalManager.dispose(1);
  });

  it('agent-output events are received and written to terminal', async () => {
    const instance = await terminalManager.getOrCreate(1);
    expect(instance).not.toBeNull();

    const writeSpy = vi.spyOn(instance!.term, 'write');

    // Simulate agent-output event
    const listeners = mockListeners.get('agent-output');
    listeners?.forEach(cb => cb({ payload: { session_id: 1, line: 'Hello\n' } }));

    await flushScheduledWork();

    expect(writeSpy).toHaveBeenCalledWith('Hello\n');
  });

  it('agent-output byte payloads are decoded and written to terminal as bytes', async () => {
    const instance = await terminalManager.getOrCreate(1);
    expect(instance).not.toBeNull();

    const writeSpy = vi.spyOn(instance!.term, 'write');

    mockListeners.get('agent-output')?.forEach(cb =>
      cb({ payload: { session_id: 1, data: '4paI' } })
    );

    await flushScheduledWork();

    expect(writeSpy).toHaveBeenCalledWith(new Uint8Array([0xe2, 0x96, 0x88]));
  });

  it('events for different sessions are not cross-written', async () => {
    const instance1 = await terminalManager.getOrCreate(1);
    const instance2 = await terminalManager.getOrCreate(2);
    expect(instance1).not.toBeNull();
    expect(instance2).not.toBeNull();

    const write1Spy = vi.spyOn(instance1!.term, 'write');
    const write2Spy = vi.spyOn(instance2!.term, 'write');

    // Event for session 1 only
    mockListeners.get('agent-output')?.forEach(cb =>
      cb({ payload: { session_id: 1, line: 'From session 1\n' } })
    );

    await flushScheduledWork();

    expect(write1Spy).toHaveBeenCalledWith('From session 1\n');
    expect(write2Spy).not.toHaveBeenCalled();
  });

  it('open() is called before subsequent writes', async () => {
    const instance = await terminalManager.getOrCreate(1);
    expect(instance).not.toBeNull();

    const container = document.createElement('div');
    instance!.term.open(container);

    instance!.term.write('Content after open');
    expect(instance!.term.write).toHaveBeenCalledWith('Content after open');
  });

  it('visibility effect calls scrollToBottom and refresh when terminal becomes visible', async () => {
    const instance = await terminalManager.getOrCreate(1);
    expect(instance).not.toBeNull();

    const container = document.createElement('div');
    instance!.term.open(container);
    instance!.term.write('Some content');

    // Simulate what the visibility effect does when isVisible becomes true
    instance!.fitAddon.fit();
    instance!.term.focus();
    instance!.term.scrollToBottom();
    instance!.term.refresh(0, instance!.term.rows - 1);

    // Verify all methods were called
    expect(instance!.fitAddon.fit).toHaveBeenCalled();
    expect(instance!.term.focus).toHaveBeenCalled();
    expect(instance!.term.scrollToBottom).toHaveBeenCalled();
    expect(instance!.term.refresh).toHaveBeenCalledWith(0, instance!.term.rows - 1);
  });
});
