// Regression: detach must remove term.element from its parent, otherwise the
// single-pane SessionView stacks the old terminal alongside the new one when
// switching between meshes that each have a single node (#fde8b61).
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { terminalManager } from '../../src/components/Terminal/Terminal';

const mockListeners = new Map<string, Set<(...args: unknown[]) => void>>();

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
  // See tests/setup/vitest.setup.ts for why the unicode service needs
  // _providers + a real register method (loadUnicode11Widths reads from it).
  // We pre-seed _providers['11'] so loadUnicode11Widths can read from it
  // after its (no-op) loadAddon() call. Both `wcwidth` AND
  // `charProperties` are read by the production helper.
  const mockProviders: Record<string, {
    version: string;
    wcwidth: (cp: number) => number;
    charProperties: (cp: number, preceding: number) => number;
  }> = {
    '11': { version: '11', wcwidth: () => 1, charProperties: () => 0 },
  };
  let mockActive = '11';

  class MockTerminal {
    write = vi.fn();
    onData = vi.fn();
    onTitleChange = vi.fn();
    onResize = vi.fn();
    dispose = vi.fn();
    focus = vi.fn();
    loadAddon = vi.fn();
    attachCustomKeyEventHandler = vi.fn();
    scrollToBottom = vi.fn();
    refresh = vi.fn();
    resize = vi.fn();
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

    open(container: HTMLElement) {
      this.element = document.createElement('div');
      this.element.className = 'xterm';
      container.appendChild(this.element);
    }

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

describe('terminal container reuse on sessionId change (single-pane mesh switch)', () => {
  beforeEach(() => {
    mockListeners.clear();
    vi.clearAllMocks();
    terminalManager.dispose(1);
    terminalManager.dispose(2);
  });

  afterEach(() => {
    terminalManager.dispose(1);
    terminalManager.dispose(2);
  });

  it('detach removes the terminal element from its parent container', async () => {
    const inst = await terminalManager.getOrCreate(1);
    expect(inst).not.toBeNull();

    const container = document.createElement('div');
    inst!.term.open(container);
    inst!.opened = true;
    expect(inst!.term.element!.parentElement).toBe(container);

    terminalManager.detach(1);

    expect(inst!.term.element!.parentElement).toBeNull();
  });

  it('reusing the same container with a new sessionId leaves only one terminal element', async () => {
    // Simulate single-pane SessionView: mesh A's only node attaches to container.
    const container = document.createElement('div');

    const instA = await terminalManager.getOrCreate(1);
    instA!.term.open(container);
    instA!.opened = true;
    expect(container.querySelectorAll('.xterm').length).toBe(1);

    // User clicks mesh B's only node in sidebar. React reuses the AgentTerminal
    // DOM container; effect cleanup runs detach(old), then attach(new) reopens
    // into the same container.
    terminalManager.detach(1);

    const instB = await terminalManager.getOrCreate(2);
    instB!.term.open(container);
    instB!.opened = true;

    // Container should now hold only the new terminal, not both stacked.
    expect(container.querySelectorAll('.xterm').length).toBe(1);
    expect(instB!.term.element!.parentElement).toBe(container);
    expect(instA!.term.element!.parentElement).toBeNull();
  });

  it('terminal instance is preserved through detach so it can be re-attached later', async () => {
    const inst = await terminalManager.getOrCreate(1);
    const containerA = document.createElement('div');
    inst!.term.open(containerA);
    inst!.opened = true;

    terminalManager.detach(1);

    // Instance must still be available (detach is not dispose).
    expect(terminalManager.getInstance(1)).toBe(inst);

    // Element reference is preserved and can be reparented into a new container.
    const containerB = document.createElement('div');
    containerB.appendChild(inst!.term.element!);
    expect(inst!.term.element!.parentElement).toBe(containerB);
  });
});

// Regression: TerminalRegistry.attachToDOM unconditionally scheduled
// scrollToBottom() in a requestAnimationFrame on every attach, which forced
// the terminal back to the tail whenever the React effect re-ran (e.g. on
// node.status transitions). That destroyed the user's scrollback context
// silently and also caused the new "jump to latest" pill to flash for one
// frame on every switch back to a scrolled-up terminal. Auto-scroll-to-tail
// is only meaningful on the *first* DOM open of an instance — once the user
// has a buffer they may have scrolled, re-attaches must preserve that.
describe('terminal attach preserves scroll position on re-attach', () => {
  beforeEach(() => {
    if (!('ResizeObserver' in globalThis)) {
      (globalThis as unknown as { ResizeObserver: unknown }).ResizeObserver = class {
        observe() {}
        unobserve() {}
        disconnect() {}
      };
    }
    vi.clearAllMocks();
    terminalManager.dispose(1);
  });

  afterEach(() => {
    terminalManager.dispose(1);
  });

  it('scrollToBottom is called on first open but NOT on re-attach', async () => {
    const containerA = document.createElement('div');
    const inst = await terminalManager.attach(1, containerA);
    expect(inst).not.toBeNull();

    // Flush the rAF that attachToDOM queues for measureAndFit/refresh.
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    const firstOpenCalls = (inst!.term.scrollToBottom as ReturnType<typeof vi.fn>).mock.calls.length;
    expect(firstOpenCalls).toBeGreaterThanOrEqual(1);

    // Simulate the effect re-running (e.g. node.status changed) — detach
    // followed by attach back into a fresh container, same instance.
    terminalManager.detach(1);
    const containerB = document.createElement('div');
    const inst2 = await terminalManager.attach(1, containerB);
    expect(inst2).toBe(inst);

    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    const afterReattachCalls = (inst!.term.scrollToBottom as ReturnType<typeof vi.fn>).mock.calls.length;

    // The re-attach must not have triggered another scrollToBottom.
    expect(afterReattachCalls).toBe(firstOpenCalls);
  });
});
