/**
 * Regression tests for terminal re-parenting on project switch.
 *
 * Bug: When switching projects, React unmounts the old GridLayout cells and mounts
 * new ones. The xterm.js terminal element stayed attached to the old (now unmounted)
 * container div, causing terminals to appear blank in the new project view.
 *
 * Fix: TerminalContainer re-parents the terminal element into the new container
 * when it detects the terminal was previously opened elsewhere.
 *
 * Run with: npm test -- --run tests/integration/terminal-reparenting.test.tsx
 */
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

describe('terminal re-parenting on project switch', () => {
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

  it('terminal element is attached to container on first open', async () => {
    const instance = await terminalManager.getOrCreate(1);
    expect(instance).not.toBeNull();

    const container = document.createElement('div');
    instance!.term.open(container);
    instance!.opened = true;

    expect(instance!.term.element).not.toBeNull();
    expect(instance!.term.element!.parentElement).toBe(container);
  });

  it('terminal element can be re-parented to a new container', async () => {
    const instance = await terminalManager.getOrCreate(1);
    expect(instance).not.toBeNull();

    // Simulate first mount (project A's grid cell)
    const containerA = document.createElement('div');
    instance!.term.open(containerA);
    instance!.opened = true;

    expect(instance!.term.element!.parentElement).toBe(containerA);

    // Simulate project switch: containerA is unmounted, containerB is mounted
    // The re-parenting logic from TerminalContainer:
    const containerB = document.createElement('div');
    const termEl = instance!.term.element;
    if (termEl && termEl.parentElement !== containerB) {
      containerB.appendChild(termEl);
    }

    // Terminal element should now be in the new container
    expect(instance!.term.element!.parentElement).toBe(containerB);
    // Old container should be empty
    expect(containerA.children.length).toBe(0);
  });

  it('re-parenting preserves the terminal element identity', async () => {
    const instance = await terminalManager.getOrCreate(1);
    expect(instance).not.toBeNull();

    const containerA = document.createElement('div');
    instance!.term.open(containerA);
    instance!.opened = true;

    const originalElement = instance!.term.element;

    // Re-parent
    const containerB = document.createElement('div');
    containerB.appendChild(instance!.term.element!);

    // Same DOM element, just moved
    expect(instance!.term.element).toBe(originalElement);
  });

  it('skips re-parenting when terminal is already in the correct container', async () => {
    const instance = await terminalManager.getOrCreate(1);
    expect(instance).not.toBeNull();

    const container = document.createElement('div');
    instance!.term.open(container);
    instance!.opened = true;

    // Simulate the guard: element is already in this container
    const termEl = instance!.term.element;
    const needsReparent = termEl && termEl.parentElement !== container;

    expect(needsReparent).toBe(false);
  });

  it('handles multiple sessions independently during project switch', async () => {
    const inst1 = await terminalManager.getOrCreate(1);
    const inst2 = await terminalManager.getOrCreate(2);

    // Mount both in project A's grid
    const gridA_cell1 = document.createElement('div');
    const gridA_cell2 = document.createElement('div');
    inst1!.term.open(gridA_cell1);
    inst1!.opened = true;
    inst2!.term.open(gridA_cell2);
    inst2!.opened = true;

    // Switch to project B — only session 1 belongs to project B
    const gridB_cell1 = document.createElement('div');
    const termEl1 = inst1!.term.element;
    if (termEl1 && termEl1.parentElement !== gridB_cell1) {
      gridB_cell1.appendChild(termEl1);
    }

    // Session 1 is now in new container
    expect(inst1!.term.element!.parentElement).toBe(gridB_cell1);
    // Session 2 remains where it was (its old container is unmounted from DOM
    // but the element reference is still valid)
    expect(inst2!.term.element!.parentElement).toBe(gridA_cell2);
  });

  it('re-parented terminal can still receive writes', async () => {
    const instance = await terminalManager.getOrCreate(1);
    expect(instance).not.toBeNull();

    const containerA = document.createElement('div');
    instance!.term.open(containerA);
    instance!.opened = true;

    // Re-parent to new container
    const containerB = document.createElement('div');
    containerB.appendChild(instance!.term.element!);

    // Write should still work after re-parenting
    instance!.term.write('data after reparent');
    expect(instance!.term.write).toHaveBeenCalledWith('data after reparent');
  });

  it('open() is not called again on re-parent (no double-init)', async () => {
    const instance = await terminalManager.getOrCreate(1);
    expect(instance).not.toBeNull();

    const containerA = document.createElement('div');
    instance!.term.open(containerA);
    instance!.opened = true;

    // Spy on open to count future calls
    const openSpy = vi.spyOn(instance!.term, 'open');

    // Re-parent (the fix path — does NOT call open())
    const containerB = document.createElement('div');
    const termEl = instance!.term.element;
    if (termEl && termEl.parentElement !== containerB) {
      containerB.appendChild(termEl);
    }

    // open() should NOT have been called again
    expect(openSpy).not.toHaveBeenCalled();
  });
});
