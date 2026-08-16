// Issue #1122 — the DOM renderer in xterm.js degrades from <2ms to 30-60ms
// per keystroke once the scrollback spans thousands of lines, because TUI
// cursor positioning forces style recalcs across the HTML <span> tree.
// TerminalRegistry must load `@xterm/addon-webgl` to keep keystroke
// latency flat, with a clean fallback to the DOM renderer when WebGL2 is
// unavailable (Safari < 16, headless harness, OS-disabled hardware accel).

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { TerminalRegistry } from '../../src/components/Terminal/TerminalRegistry';

// Mock xterm + addons the same way terminal-registry.test.ts does, so
// `new Terminal(...)` and the existing addons are no-ops.
globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockImplementation(() => Promise.resolve(() => {})),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn().mockResolvedValue({}),
}));

vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('@xterm/xterm', () => {
  // Mirror the real @xterm/xterm shape that loadUnicode11Widths depends
  // on: an internal `_core.unicodeService._providers` map keyed by
  // version string, plus a user-facing `term.unicode.register` proxy.
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
    register: vi.fn((p: { version: string }) => {
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
    loadAddon = vi.fn((addon: { activate?: (term: unknown) => void }) => {
      // Mirror real xterm: `loadAddon` delegates to the addon's `activate`
      // method. Our WebGL mock uses this to throw when `activateThrow` is
      // set, exercising the registry's catch path.
      addon.activate?.(undefined);
    });
    attachCustomKeyEventHandler = vi.fn();
    scrollToBottom = vi.fn();
    refresh = vi.fn();
    clear = vi.fn();
    selectAll = vi.fn();
    hasSelection = vi.fn().mockReturnValue(false);
    getSelection = vi.fn().mockReturnValue('');
    paste = vi.fn();
    buffer = { active: { getWindow: vi.fn() } };
    unicode = {
      register: vi.fn((p: { version: string }) => internalService.register(p)),
      get activeVersion() { return mockActive; },
      set activeVersion(v: string) {
        if (!mockProviders[v]) {
          throw new Error(`unknown Unicode version "${v}"`);
        }
        mockActive = v;
      },
    };
    _core = { unicodeService: internalService };
    rows = 24;
    cols = 80;
    options = { fontSize: 10 };
    element: HTMLElement | null = null;
    constructor(_options?: unknown) {}
  }
  return { Terminal: MockTerminal };
});

vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class { fit = vi.fn(); proposeDimensions = vi.fn().mockReturnValue({ cols: 80, rows: 24 }); dispose = vi.fn(); },
}));
vi.mock('@xterm/addon-serialize', () => ({
  SerializeAddon: class { serialize = vi.fn().mockReturnValue(''); dispose = vi.fn(); },
}));
vi.mock('@xterm/addon-search', () => ({
  SearchAddon: class { findNext = vi.fn(); findPrevious = vi.fn(); clearDecorations = vi.fn(); dispose = vi.fn(); },
}));
vi.mock('@xterm/addon-web-links', () => ({
  WebLinksAddon: class { dispose = vi.fn(); },
}));
vi.mock('@xterm/addon-unicode11', () => ({
  Unicode11Addon: class { dispose = vi.fn(); },
}));

// Mock the WebGL addon so we can drive every branch the loader takes —
// constructor throw (Safari < 16), activate throw (no WebGL2 context),
// success (load + subscribe), and context-loss recovery.
const listeners: Array<() => void> = [];
let constructorThrow = false;
let activateThrow = false;
let constructorCalls = 0;

vi.mock('@xterm/addon-webgl', () => {
  class MockWebglAddon {
    dispose = vi.fn();
    onContextLoss = vi.fn((handler: () => void) => {
      listeners.push(handler);
      return { dispose: () => {
        const i = listeners.indexOf(handler);
        if (i >= 0) listeners.splice(i, 1);
      } };
    });
    constructor() {
      constructorCalls++;
      if (constructorThrow) {
        throw new Error('Webgl2 is only supported on Safari 16 and above');
      }
    }
    activate(_terminal: unknown): void {
      if (activateThrow) {
        throw new Error('Failed to create WebGL2 context');
      }
    }
  }
  return { WebglAddon: MockWebglAddon };
});

function resetWebglState(): void {
  listeners.length = 0;
  constructorThrow = false;
  activateThrow = false;
  constructorCalls = 0;
}

describe('TerminalRegistry WebGL renderer (issue #1122)', () => {
  let registry: TerminalRegistry;
  // Quiet the expected console.warn noise from the fallback paths.
  let warnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    resetWebglState();
    vi.clearAllMocks();
    registry = new TerminalRegistry();
    warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
  });

  afterEach(() => {
    registry.destroy();
    warnSpy.mockRestore();
  });

  it('attaches the WebGL addon when WebGL2 is available', async () => {
    const inst = await registry.getOrCreate(1);
    expect(inst!.webglHandle.kind).toBe('webgl');
    expect(constructorCalls).toBe(1);
  });

  it('falls back to the DOM renderer when the WebGL constructor throws', async () => {
    constructorThrow = true;
    const inst = await registry.getOrCreate(1);
    expect(inst!.webglHandle.kind).toBe('dom');
    // The user-visible "renderer is the default" log line gives a Triage
    // reader a smoking gun on a host where WebGL was just disabled —
    // assert the log fires, and the message names the node.
    expect(warnSpy).toHaveBeenCalledWith(
      expect.stringContaining('WebGL unavailable for node 1'),
      expect.anything(),
    );
  });

  it('falls back to the DOM renderer when WebGL activate throws', async () => {
    activateThrow = true;
    const inst = await registry.getOrCreate(1);
    expect(inst!.webglHandle.kind).toBe('dom');
    expect(warnSpy).toHaveBeenCalledWith(
      expect.stringContaining('WebGL activate failed for node 1'),
      expect.anything(),
    );
  });

  it('disposes the addon when the terminal is disposed', async () => {
    const inst = await registry.getOrCreate(1);
    expect(inst!.webglHandle.kind).toBe('webgl');
    const addon = inst!.webglHandle.kind === 'webgl' ? inst!.webglHandle.addon : null;
    registry.dispose(1);
    // Disposing twice (the registry calls dispose + a potential re-dispose
    // from the context-loss path) must not throw — the recovery path
    // catches double-dispose.
    expect(() => registry.dispose(1)).not.toThrow();
    expect(addon!.dispose).toHaveBeenCalled();
    // The context-loss listener must be released too — otherwise it pins
    // the disposed terminal in a closure until the registry itself dies.
    expect(listeners).toHaveLength(0);
  });

  it('recovers from a WebGL context loss by re-attaching the addon', async () => {
    const inst = await registry.getOrCreate(1);
    expect(inst!.webglHandle.kind).toBe('webgl');
    const originalAddon = inst!.webglHandle.kind === 'webgl' ? inst!.webglHandle.addon : null;
    const beforeCalls = constructorCalls;
    const beforeListeners = listeners.length;

    // Simulate the GPU dropping the context (driver crash, tab backgrounded
    // past the timeout, hardware acceleration toggled off).
    expect(listeners).toHaveLength(1);
    listeners[0]();

    // The recovery path constructs a fresh addon, so the total count
    // ticks up by one. The original addon's dispose is also called (it
    // restored the DOM renderer so the new addon can take over).
    expect(constructorCalls).toBe(beforeCalls + 1);
    expect(originalAddon!.dispose).toHaveBeenCalled();
    // The OLD listener must be released so its closure (capturing the
    // disposed addon) doesn't pin the registry, and the new listener
    // must be registered in its place. Net effect: 1 listener
    // registered (the new one), same as before the recovery.
    expect(listeners).toHaveLength(beforeListeners);

    // Regression for the bug where the registry kept the original
    // (disposed) handle on recovery: the recovered addon was leaked
    // (its listener never released) and a second context-loss event
    // would operate on a torn-down handle. The fix swaps the stored
    // handle in place, so `inst.webglHandle` is now the NEW live
    // addon's handle — not the disposed one.
    const fresh = inst!.webglHandle;
    expect(fresh.kind).toBe('webgl');
    if (fresh.kind === 'webgl' && originalAddon) {
      expect(fresh.addon).not.toBe(originalAddon);
      // The new handle's listener must also be live, so a SECOND
      // context-loss event can still trigger recovery.
      expect(fresh.addon.dispose).not.toHaveBeenCalled();
    }
  });

  it('survives a second context-loss event after the first recovery', async () => {
    // End-to-end check of the recovery fix: after the first context
    // loss, a SECOND loss must also recover cleanly. Pre-fix, the
    // registry held the torn-down handle and the second listener was
    // a no-op (or threw on the disposed addon's emitter), so a flaky
    // GPU would freeze the terminal instead of recovering again.
    const inst = await registry.getOrCreate(1);
    const firstAddon = inst!.webglHandle.kind === 'webgl' ? inst!.webglHandle.addon : null;

    // First context loss.
    listeners[0]();
    const secondAddon = inst!.webglHandle.kind === 'webgl' ? inst!.webglHandle.addon : null;
    expect(secondAddon).not.toBe(firstAddon);

    // Second context loss — the recovered handle has its own live
    // listener, so this still fires a fresh recovery.
    listeners[0]();
    const thirdAddon = inst!.webglHandle.kind === 'webgl' ? inst!.webglHandle.addon : null;
    expect(thirdAddon).not.toBe(secondAddon);
    expect(secondAddon!.dispose).toHaveBeenCalled();
  });

  it('stays on the DOM renderer when context-loss re-attach fails', async () => {
    const inst = await registry.getOrCreate(1);
    expect(inst!.webglHandle.kind).toBe('webgl');

    // Flip the constructor to fail BEFORE the context loss fires so the
    // recovery path lands on the DOM branch.
    constructorThrow = true;
    warnSpy.mockClear();
    listeners[0]();

    // The re-attach logs a warning that names the node so a Triage reader
    // can see "we lost WebGL and the recovery failed" without diffing
    // buildmesh.log against the device tree.
    expect(warnSpy).toHaveBeenCalledWith(
      expect.stringContaining('WebGL re-attach failed for node 1'),
    );
  });
});
