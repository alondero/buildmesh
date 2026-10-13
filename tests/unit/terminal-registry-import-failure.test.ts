/**
 * Creation failure at the lazy-import stage (issue #2016).
 *
 * `doCreate` awaits six dynamic `import()`s before it owns anything, so a
 * failing chunk load is the one registration stage with nothing yet
 * acquired. It still has to resolve to "no terminal" rather than a dangling
 * half-creation: no xterm, no writer sink, no Tauri listener, no binary PTY
 * Channel, and no registry entry a later delete could find.
 *
 * Its own file because a mocked module factory runs once per test file: to
 * observe the rejection, this has to be the first `@xterm/xterm` import in
 * the module registry. Everything the factory DOES own on the later stages is
 * covered by `terminal-registry-creation-race.test.ts`.
 *
 * Run with: npx vitest run tests/unit/terminal-registry-import-failure.test.ts
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const control = {
  listens: [] as string[],
  ipc: [] as string[],
};

// The chunk fetch fails — a corrupted/missing Vite chunk, or a stale cached
// index that 404s. The factory itself rejects, so the module runner owns the
// rejected promise and vitest's unhandled-error gate stays meaningful.
vi.doMock('@xterm/xterm', () => {
  throw new Error('Failed to fetch dynamically imported module: /assets/xterm.js');
});
vi.doMock('@xterm/addon-unicode11', () => ({
  Unicode11Addon: class { dispose = vi.fn(); },
}));
vi.doMock('@xterm/addon-fit', () => ({ FitAddon: class { dispose = vi.fn(); } }));
vi.doMock('@xterm/addon-serialize', () => ({ SerializeAddon: class { dispose = vi.fn(); } }));
vi.doMock('@xterm/addon-search', () => ({ SearchAddon: class { dispose = vi.fn(); } }));
vi.doMock('@xterm/addon-web-links', () => ({ WebLinksAddon: class { dispose = vi.fn(); } }));
vi.doMock('@xterm/addon-webgl', () => ({ WebglAddon: class { dispose = vi.fn(); onContextLoss = vi.fn(); } }));
vi.doMock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn().mockResolvedValue(undefined) }));

vi.doMock('@tauri-apps/api/event', () => ({
  listen: vi.fn((event: string) => {
    control.listens.push(event);
    return Promise.resolve(() => {});
  }),
  emit: vi.fn().mockResolvedValue(undefined),
}));

vi.doMock('@tauri-apps/api/core', () => ({
  invoke: vi.fn((command: string) => {
    control.ipc.push(command);
    return Promise.resolve(undefined);
  }),
  Channel: class { onmessage: (message: unknown) => void = () => {}; },
}));

type Registry = InstanceType<typeof import('../../src/components/Terminal/TerminalRegistry').TerminalRegistry>;

describe('TerminalRegistry creation fails at the lazy import (issue #2016)', () => {
  let registry: Registry;

  beforeEach(async () => {
    control.listens = [];
    control.ipc = [];
    globalThis.ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    } as unknown as typeof ResizeObserver;
    const module = await import('../../src/components/Terminal/TerminalRegistry');
    registry = new module.TerminalRegistry();
  });

  it('resolves to null and registers nothing', async () => {
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    try {
      expect(await registry.getOrCreate(1)).toBeNull();
      expect(registry.getInstance(1)).toBeUndefined();
      // Never reached, so nothing was registered for them.
      expect(control.listens).not.toContain('agent-output');
      expect(control.listens).not.toContain('serialize-terminal-request');
      expect(control.ipc).not.toContain('subscribe_agent_output');
      // The failure is reported once, with the node it belongs to.
      expect(errorSpy).toHaveBeenCalledWith(
        '[TerminalRegistry] Failed to create terminal for 1',
        expect.any(Error),
      );
    } finally {
      errorSpy.mockRestore();
      registry.destroy();
    }
  });

  it('stays disposable and retryable — the failed node leaves no entry behind', async () => {
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    try {
      await registry.getOrCreate(1);
      await registry.getOrCreate(1);
      // A deletion of a node that never materialised is a no-op, and a
      // repeated attempt still fails cleanly rather than throwing.
      expect(() => registry.dispose(1)).not.toThrow();
      expect(await registry.attach(1, document.createElement('div'))).toBeNull();
    } finally {
      errorSpy.mockRestore();
      registry.destroy();
    }
  });
});