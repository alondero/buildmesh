import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { openUrl } from '@tauri-apps/plugin-opener';
import { TerminalRegistry } from '../../src/components/Terminal/TerminalRegistry';

const terminalTestState = vi.hoisted(() => ({
  latestTerminalOptions: undefined as {
    linkHandler?: { activate: (event: MouseEvent, text: string, range: unknown) => void };
  } | undefined,
}));

globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockImplementation((_event: string, _callback: unknown) => {
    return Promise.resolve(() => {});
  }),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn().mockResolvedValue({}),
}));

vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: vi.fn().mockResolvedValue(undefined),
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
    clear = vi.fn();
    selectAll = vi.fn();
    hasSelection = vi.fn().mockReturnValue(false);
    getSelection = vi.fn().mockReturnValue('');
    paste = vi.fn();
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
    options = { fontSize: 10 };
    element: HTMLElement | null = null;
    constructor(options?: typeof terminalTestState.latestTerminalOptions) {
      terminalTestState.latestTerminalOptions = options;
    }
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

vi.mock('@xterm/addon-serialize', () => {
  class MockSerializeAddon {
    serialize = vi.fn().mockReturnValue('');
    dispose = vi.fn();
  }
  return { SerializeAddon: MockSerializeAddon };
});

vi.mock('@xterm/addon-search', () => {
  class MockSearchAddon {
    findNext = vi.fn();
    findPrevious = vi.fn();
    clearDecorations = vi.fn();
    dispose = vi.fn();
  }
  return { SearchAddon: MockSearchAddon };
});

vi.mock('@xterm/addon-web-links', () => {
  class MockWebLinksAddon {
    dispose = vi.fn();
    constructor(_handler?: unknown) {}
  }
  return { WebLinksAddon: MockWebLinksAddon };
});

vi.mock('@xterm/addon-unicode11', () => {
  class MockUnicode11Addon {
    dispose = vi.fn();
  }
  return { Unicode11Addon: MockUnicode11Addon };
});

vi.mock('@xterm/addon-webgl', () => {
  // Issue #1122: WebGL addon is loaded on every terminal. The mock has to
  // expose `onContextLoss` (an event listener registration) so the
  // production loader's fallback handler can subscribe without throwing.
  class MockWebglAddon {
    dispose = vi.fn();
    onContextLoss = vi.fn();
  }
  return { WebglAddon: MockWebglAddon };
});

describe('TerminalRegistry', () => {
  let registry: TerminalRegistry;

  beforeEach(() => {
    vi.clearAllMocks();
    terminalTestState.latestTerminalOptions = undefined;
    registry = new TerminalRegistry();
  });

  afterEach(() => {
    registry.destroy();
  });

  describe('getOrCreate', () => {
    it('creates a new instance', async () => {
      const inst = await registry.getOrCreate(1);
      expect(inst).not.toBeNull();
      expect(inst!.term).toBeDefined();
      expect(inst!.fitAddon).toBeDefined();
    });

    it('returns same instance on subsequent calls', async () => {
      const first = await registry.getOrCreate(1);
      const second = await registry.getOrCreate(1);
      expect(first).toBe(second);
    });

    it('deduplicates concurrent creation requests', async () => {
      const [r1, r2, r3] = await Promise.all([
        registry.getOrCreate(1),
        registry.getOrCreate(1),
        registry.getOrCreate(1),
      ]);
      expect(r1).toBe(r2);
      expect(r2).toBe(r3);
    });

    it('creates separate instances for different IDs', async () => {
      const inst1 = await registry.getOrCreate(1);
      const inst2 = await registry.getOrCreate(2);
      expect(inst1).not.toBe(inst2);
    });

    it('notifies subscribers on creation', async () => {
      const cb = vi.fn();
      registry.subscribe(cb);
      await registry.getOrCreate(1);
      expect(cb).toHaveBeenCalled();
    });

    // Regression: without the Unicode 11 width tables, xterm falls back to
    // Unicode 6 widths and emoji-bearing CLI output (Claude Code status tables,
    // gh, npm) shears its box-drawing borders on Windows.
    it('activates Unicode 11 glyph widths', async () => {
      const inst = await registry.getOrCreate(1);
      expect(inst!.term.unicode.activeVersion).toBe('11');
    });

    it('opens OSC 8 hyperlinks in the default browser', async () => {
      await registry.getOrCreate(1);

      terminalTestState.latestTerminalOptions!.linkHandler!.activate(
        new MouseEvent('click'),
        'https://example.com/codex-link',
        {},
      );

      expect(openUrl).toHaveBeenCalledWith('https://example.com/codex-link');
    });
  });

  describe('getInstance / getTerminal', () => {
    it('returns undefined for non-existent node', () => {
      expect(registry.getInstance(999)).toBeUndefined();
      expect(registry.getTerminal(999)).toBeUndefined();
    });

    it('returns instance after creation', async () => {
      const inst = await registry.getOrCreate(1);
      expect(registry.getInstance(1)).toBe(inst);
      expect(registry.getTerminal(1)).toBe(inst!.term);
    });
  });

  describe('attach / detach', () => {
    it('opens terminal on first attach', async () => {
      const container = document.createElement('div');
      const inst = await registry.attach(1, container);
      expect(inst).not.toBeNull();
      expect(inst!.opened).toBe(true);
      expect(inst!.term.open).toHaveBeenCalledWith(container);
    });

    it('returns null if creation fails', async () => {
      const container = document.createElement('div');
      const inst = await registry.attach(999, container);
      expect(inst).not.toBeNull();
    });

    it('detach clears attachedContainer', async () => {
      const container = document.createElement('div');
      const inst = await registry.attach(1, container);
      expect(inst!.attachedContainer).toBe(container);

      registry.detach(1);
      expect(inst!.attachedContainer).toBeNull();
    });

    it('detach is safe for non-existent node', () => {
      expect(() => registry.detach(999)).not.toThrow();
    });
  });

  describe('dispose', () => {
    it('removes instance', async () => {
      await registry.getOrCreate(1);
      registry.dispose(1);
      expect(registry.getInstance(1)).toBeUndefined();
    });

    it('calls term.dispose and unlisten', async () => {
      const inst = await registry.getOrCreate(1);
      const disposeSpy = vi.spyOn(inst!.term, 'dispose');
      const unlistenSpy = vi.spyOn(inst!, 'unlisten');

      registry.dispose(1);
      expect(disposeSpy).toHaveBeenCalled();
      expect(unlistenSpy).toHaveBeenCalled();
    });

    it('allows fresh creation after dispose', async () => {
      const inst1 = await registry.getOrCreate(1);
      registry.dispose(1);
      const inst2 = await registry.getOrCreate(1);
      expect(inst2).not.toBe(inst1);
    });

    it('notifies subscribers', async () => {
      const cb = vi.fn();
      registry.subscribe(cb);
      await registry.getOrCreate(1);
      cb.mockClear();

      registry.dispose(1);
      expect(cb).toHaveBeenCalled();
    });

    it('is idempotent', () => {
      expect(() => registry.dispose(999)).not.toThrow();
    });
  });

  describe('syncPtySize', () => {
    // Post-spawn PTY-size reconcile. The attach-fit fires resize_agent BEFORE
    // the agent process exists ("Agent not running", swallowed), and spawn
    // falls back to 80x24. Once the agent is up, nothing re-pushes the term's
    // real size — so the PTY stayed at 80 cols inside a wide pane and the
    // agent wrapped its output / input early. syncPtySize closes that gap.
    //
    // The constructor's `agent-spawned` listener (issue #332) is the
    // production caller; the dedicated wiring test lives in
    // terminal-registry-agent-spawned.test.ts. The unit tests below pin the
    // method's contract — re-push the term's current dimensions to the PTY,
    // no-op for detached / unknown nodes — independent of who calls it.
    it('re-sends the terminal\'s current dimensions to the PTY', async () => {
      const container = document.createElement('div');
      const inst = await registry.attach(1, container);
      // Simulate the term having been fit to a wide pane (e.g. a full-width
      // partial row in the fluid grid) while the PTY is still at the 80x24
      // spawn fallback.
      inst!.term.cols = 180;
      inst!.term.rows = 50;
      vi.mocked(invoke).mockClear();

      registry.syncPtySize(1);

      expect(invoke).toHaveBeenCalledWith('resize_agent', { sessionId: 1, rows: 50, cols: 180 });
    });

    it('does nothing for a detached terminal', async () => {
      await registry.getOrCreate(1);
      vi.mocked(invoke).mockClear();

      registry.syncPtySize(1);

      expect(invoke).not.toHaveBeenCalled();
    });

    it('is safe for a non-existent node', () => {
      vi.mocked(invoke).mockClear();
      expect(() => registry.syncPtySize(999)).not.toThrow();
      expect(invoke).not.toHaveBeenCalled();
    });
  });

  describe('subscribe', () => {
    it('notifies on creation', async () => {
      const cb = vi.fn();
      registry.subscribe(cb);
      await registry.getOrCreate(1);
      expect(cb).toHaveBeenCalled();
    });

    it('unsubscribe stops notifications', async () => {
      const cb = vi.fn();
      const unsub = registry.subscribe(cb);
      unsub();
      await registry.getOrCreate(1);
      expect(cb).not.toHaveBeenCalled();
    });
  });

});
