import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { TerminalRegistry } from '../../src/components/Terminal/TerminalRegistry';

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
    unicode = { activeVersion: '6' };
    rows = 24;
    cols = 80;
    options = { fontSize: 10 };
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

describe('TerminalRegistry', () => {
  let registry: TerminalRegistry;

  beforeEach(() => {
    vi.clearAllMocks();
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
