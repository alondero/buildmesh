/**
 * Creation / teardown race coverage for `TerminalRegistry` (issue #2016).
 *
 * Production shape of the bug: `getOrCreate` records a pending creation, but
 * `dispose(nodeId)` only cleaned an instance already in `instances`. Deleting
 * a node while the lazy xterm import or a `listen()` registration was in
 * flight let `doCreate` finish and register a terminal, a writer sink,
 * font-size / theme entries, Tauri listeners and a binary PTY Channel for a
 * node that no longer existed. A failure after those registrations kept every
 * one of them, and a module-level `listen()` that resolved after `destroy()`
 * parked its handle for good.
 *
 * These tests drive the real `TerminalRegistry` against controlled
 * `listen()` / IPC promises so each window is entered on purpose rather than
 * by luck:
 *
 *   - delete before the lazy chunk's first checkpoint,
 *   - delete after the terminal registrations but before the `agent-output`
 *     listener resolves,
 *   - delete while the binary PTY Channel is still registering,
 *   - reject each registration stage,
 *   - destroy before the module-level `agent-spawned` listen resolves,
 *   - concurrent callers sharing one pending creation.
 *
 * Two invariants are pinned alongside them: `detach` (React unmount / mesh
 * switch) must never cancel or dispose a surviving node, and a terminal that
 * survives those cancellations must still attach, re-attach, receive output,
 * serialize, paste and resize. The "lazy chunk itself fails to load" case
 * lives in `terminal-registry-import-failure.test.ts` — a mocked module
 * factory runs once per test file, so it needs its own file to observe the
 * rejection.
 *
 * Run with: npx vitest run tests/unit/terminal-registry-creation-race.test.ts
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { MockChannel } from '../setup/tauriChannel';

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason: unknown) => void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** One `listen()` call, with the unlisten handle it eventually hands back. */
interface ListenEntry {
  event: string;
  unlisten: ReturnType<typeof vi.fn>;
  gate: Deferred<() => void>;
}

interface IpcEntry {
  command: string;
  args?: Record<string, unknown>;
}

/**
 * Per-test switches read by the `doMock` factories below. The factories are
 * registered once and run when each test imports a fresh module graph, so
 * every switch is reset in `beforeEach`.
 */
const control = {
  /** Events whose `listen()` resolution is held open. */
  holdListen: (_event: string): boolean => false,
  /** Events whose `listen()` rejects instead of resolving. */
  rejectListen: new Set<string>(),
  listens: [] as ListenEntry[],
  /** Reject the binary `subscribe_agent_output` IPC. */
  rejectSubscribe: false,
  /** Held to keep `subscribe_agent_output` in flight. */
  subscribeGate: null as Deferred<void> | null,
  ipc: [] as IpcEntry[],
  /** The Channel object handed to `subscribe_agent_output`, per session id. */
  channels: new Map<number, MockChannel>(),
  terminals: [] as MockTerminal[],
};

/** Callbacks still registered per event, so a test can emit a real payload. */
const liveCallbacks = new Map<string, Set<(event: { payload: unknown }) => void>>();

class MockTerminal {
  write = vi.fn((_data?: unknown, callback?: () => void) => {
    callback?.();
  });
  onData = vi.fn();
  onTitleChange = vi.fn();
  resizeCallback: ((size: { cols: number; rows: number }) => void) | undefined;
  onResize = vi.fn((callback: (size: { cols: number; rows: number }) => void) => {
    this.resizeCallback = callback;
  });
  open = vi.fn();
  resize = vi.fn();
  dispose = vi.fn();
  focus = vi.fn();
  // Real xterm hands every addon the terminal on load; the FitAddon uses it
  // to fire `onResize` from `fit()`, which is how a pane resize reaches
  // `resize_agent`.
  loadAddon = vi.fn((addon: { attachTerminal?: (terminal: unknown) => void }) => {
    addon.attachTerminal?.(this);
  });
  attachCustomKeyEventHandler = vi.fn();
  scrollToBottom = vi.fn();
  refresh = vi.fn();
  clear = vi.fn();
  selectAll = vi.fn();
  hasSelection = vi.fn().mockReturnValue(false);
  getSelection = vi.fn().mockReturnValue('');
  paste = vi.fn();
  buffer = { active: { getWindow: vi.fn(), viewportY: 0, baseY: 0 } };
  unicode = {
    register: vi.fn(),
    get activeVersion() { return '11'; },
    set activeVersion(_v: string) { /* noop */ },
  };
  _core = {
    unicodeService: {
      _providers: { '11': { version: '11', wcwidth: () => 1, charProperties: () => 0 } },
      register: vi.fn(),
    },
  };
  rows = 24;
  cols = 80;
  options: Record<string, unknown> = {};
  element: HTMLElement | null = null;

  constructor(options?: Record<string, unknown>) {
    if (options) this.options = options;
    control.terminals.push(this);
  }
}

vi.doMock('@xterm/xterm', () => ({ Terminal: MockTerminal }));
vi.doMock('@xterm/addon-unicode11', () => ({
  Unicode11Addon: class { dispose = vi.fn(); },
}));

vi.doMock('@xterm/addon-fit', () => ({
  FitAddon: class {
    private terminal: { resizeCallback?: (size: { cols: number; rows: number }) => void } | null = null;
    attachTerminal(terminal: { resizeCallback?: (size: { cols: number; rows: number }) => void }): void {
      this.terminal = terminal;
    }
    fit = vi.fn(() => {
      // Real FitAddon.fit resizes the terminal when the proposed dimensions
      // change, which fires Terminal.onResize and reaches resize_agent.
      this.terminal?.resizeCallback?.({ cols: 80, rows: 24 });
    });
    proposeDimensions = vi.fn().mockReturnValue({ cols: 80, rows: 24 });
    dispose = vi.fn();
  },
}));
vi.doMock('@xterm/addon-serialize', () => ({
  SerializeAddon: class { serialize = vi.fn().mockReturnValue('snapshot'); dispose = vi.fn(); },
}));
vi.doMock('@xterm/addon-search', () => ({
  SearchAddon: class { findNext = vi.fn(); findPrevious = vi.fn(); clearDecorations = vi.fn(); dispose = vi.fn(); },
}));
vi.doMock('@xterm/addon-web-links', () => ({
  WebLinksAddon: class { dispose = vi.fn(); constructor(_handler?: unknown) {} },
}));
vi.doMock('@xterm/addon-webgl', () => ({
  // Issue #1122: the WebGL addon exposes `onContextLoss`, which the
  // production loader's fallback handler subscribes to.
  WebglAddon: class { dispose = vi.fn(); onContextLoss = vi.fn(); },
}));

vi.doMock('@tauri-apps/plugin-opener', () => ({
  openUrl: vi.fn().mockResolvedValue(undefined),
}));

vi.doMock('@tauri-apps/api/event', () => ({
  listen: vi.fn((event: string, callback: (event: { payload: unknown }) => void) => {
    if (!liveCallbacks.has(event)) liveCallbacks.set(event, new Set());
    liveCallbacks.get(event)!.add(callback);
    const entry: ListenEntry = {
      event,
      unlisten: vi.fn(),
      gate: deferred<() => void>(),
    };
    entry.unlisten.mockImplementation(() => {
      liveCallbacks.get(event)?.delete(callback);
    });
    control.listens.push(entry);
    if (control.rejectListen.has(event)) {
      entry.gate.reject(new Error(`listen failed: ${event}`));
    } else if (!control.holdListen(event)) {
      entry.gate.resolve(entry.unlisten);
    }
    return entry.gate.promise;
  }),
  emit: vi.fn().mockResolvedValue(undefined),
}));

vi.doMock('@tauri-apps/api/core', () => ({
  invoke: vi.fn((command: string, args?: Record<string, unknown>) => {
    control.ipc.push({ command, args });
    if (command === 'write_to_agent') {
      // Issue #1530: the transport reads a typed InputOutcome off this reply.
      return Promise.resolve({ disposition: 'accepted', activity: { user_input: true, submitted: false } });
    }
    if (command === 'subscribe_agent_output') {
      control.channels.set(args!.sessionId as number, args!.onChunk as MockChannel);
      if (control.rejectSubscribe) return Promise.reject(new Error('subscribe failed'));
      if (control.subscribeGate) return control.subscribeGate.promise;
    }
    return Promise.resolve(undefined);
  }),
  Channel: MockChannel,
}));

type Registry = InstanceType<typeof import('../../src/components/Terminal/TerminalRegistry').TerminalRegistry>;

/** Fresh module graph per test so no mocked state leaks between cases. */
async function loadRegistry(): Promise<Registry> {
  vi.resetModules();
  const module = await import('../../src/components/Terminal/TerminalRegistry');
  return new module.TerminalRegistry();
}

/** Yield to the macrotask queue so pending module / promise work settles. */
function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

async function waitFor(predicate: () => boolean, label: string): Promise<void> {
  for (let attempt = 0; attempt < 50; attempt++) {
    if (predicate()) return;
    await tick();
  }
  throw new Error(`timed out waiting for ${label}`);
}

function listensFor(event: string): ListenEntry[] {
  return control.listens.filter((entry) => entry.event === event);
}

function ipcFor(command: string): IpcEntry[] {
  return control.ipc.filter((entry) => entry.command === command);
}

/** Deliver a JSON `agent-output` event to whatever is still registered. */
function emitAgentOutput(nodeId: number, payload: Record<string, unknown>): void {
  for (const callback of liveCallbacks.get('agent-output') ?? []) {
    callback({ payload: { session_id: nodeId, ...payload } });
  }
}

/** Deliver raw PTY bytes through the Channel the registry subscribed with. */
function emitChannelBytes(nodeId: number, bytes: Uint8Array): void {
  control.channels.get(nodeId)?.onmessage(bytes);
}

describe('TerminalRegistry creation/teardown races (issue #2016)', () => {
  let registry: Registry | null = null;

  beforeEach(() => {
    control.holdListen = () => false;
    control.rejectListen = new Set<string>();
    control.listens = [];
    control.rejectSubscribe = false;
    control.subscribeGate = null;
    control.ipc = [];
    control.channels = new Map<number, MockChannel>();
    control.terminals = [];
    liveCallbacks.clear();
    globalThis.ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    } as unknown as typeof ResizeObserver;
  });

  afterEach(() => {
    registry?.destroy();
    registry = null;
  });

  describe('deletion racing an in-flight creation', () => {
    it('registers no terminal, listener or Channel when the node is deleted before the lazy chunk settles', async () => {
      registry = await loadRegistry();

      // `doCreate` reaches its first checkpoint only after the dynamic
      // `import()`s settle, so a delete in the same synchronous block lands
      // squarely in the chunk-fetch window production hits on first attach.
      const created = registry.getOrCreate(1);
      registry.dispose(1);

      expect(await created).toBeNull();
      expect(registry.getInstance(1)).toBeUndefined();
      expect(control.terminals).toHaveLength(0);
      expect(listensFor('agent-output')).toHaveLength(0);
      expect(listensFor('serialize-terminal-request')).toHaveLength(0);
      expect(ipcFor('subscribe_agent_output')).toHaveLength(0);
    });

    it('releases the terminal and its keyed entries when the node is deleted before the agent-output listener resolves', async () => {
      // Hold `agent-output`: by the time this resolves, `doCreate` owns the
      // xterm, its resize scheduler, the writer sink and the font-size and
      // theme entries.
      control.holdListen = (event) => event === 'agent-output';
      registry = await loadRegistry();

      const created = registry.getOrCreate(1);
      await waitFor(() => listensFor('agent-output').length === 1, 'the agent-output registration');
      expect(control.terminals).toHaveLength(1);

      registry.dispose(1);
      const held = listensFor('agent-output')[0];
      held.gate.resolve(held.unlisten);
      await created;

      expect(registry.getInstance(1)).toBeUndefined();
      expect(control.terminals[0].dispose).toHaveBeenCalledTimes(1);
      expect(held.unlisten).toHaveBeenCalledTimes(1);
      // The writer sink and the JSON listener are both gone: a late event
      // reaches nothing.
      emitAgentOutput(1, { line: 'hello' });
      expect(control.terminals[0].write).not.toHaveBeenCalled();
      // Nothing subscribed to the binary Channel on this path, so there is
      // no unsubscribe to send.
      expect(ipcFor('unsubscribe_agent_output')).toHaveLength(0);
    });

    it('unsubscribes only after the in-flight binary subscription settles, so the Channel cannot outlive the node', async () => {
      // Hold the serialize registration (the last await in `doCreate`) while
      // the binary Channel is still registering.
      control.holdListen = (event) => event === 'serialize-terminal-request';
      control.subscribeGate = deferred<void>();
      registry = await loadRegistry();

      const created = registry.getOrCreate(1);
      await waitFor(() => ipcFor('subscribe_agent_output').length === 1, 'the binary subscription');
      await waitFor(() => listensFor('serialize-terminal-request').length === 1, 'the serialize registration');
      const channel = control.channels.get(1)!;

      registry.dispose(1);
      // Still pending: unsubscribing before the subscribe lands would let
      // the backend register the Channel afterwards, with nothing left to
      // drop it.
      expect(ipcFor('unsubscribe_agent_output')).toHaveLength(0);
      expect(control.terminals[0].dispose).toHaveBeenCalledTimes(1);

      control.subscribeGate.resolve();
      await waitFor(() => ipcFor('unsubscribe_agent_output').length === 1, 'the output unsubscribe');
      expect(ipcFor('unsubscribe_agent_output')[0].args).toEqual({ sessionId: 1 });

      const held = listensFor('serialize-terminal-request')[0];
      held.gate.resolve(held.unlisten);
      expect(await created).toBeNull();
      expect(registry.getInstance(1)).toBeUndefined();
      // The listener registered after the release is released on arrival.
      expect(held.unlisten).toHaveBeenCalledTimes(1);
      emitChannelBytes(1, new Uint8Array([0x68]));
      expect(control.terminals[0].write).not.toHaveBeenCalled();
      expect(channel).toBeDefined();
    });

    it('releases the rest of the resources even when one disposer throws', async () => {
      // `agent-output` resolves first, so its unlisten is owned ahead of the
      // serialize listener and the xterm; a throwing handle must not skip
      // the terminal teardown that follows it.
      control.holdListen = (event) => event === 'serialize-terminal-request';
      registry = await loadRegistry();

      const created = registry.getOrCreate(1);
      await waitFor(() => listensFor('agent-output').length === 1, 'the agent-output registration');
      const agentOutput = listensFor('agent-output')[0];
      agentOutput.unlisten.mockImplementation(() => {
        throw new Error('unlisten failed');
      });

      registry.dispose(1);
      const held = listensFor('serialize-terminal-request')[0];
      held.gate.resolve(held.unlisten);
      expect(await created).toBeNull();
      expect(registry.getInstance(1)).toBeUndefined();
      expect(held.unlisten).toHaveBeenCalledTimes(1);
      expect(control.terminals[0].dispose).toHaveBeenCalledTimes(1);
    });

    it('shares one pending creation between concurrent callers', async () => {
      registry = await loadRegistry();

      const [first, second, third] = await Promise.all([
        registry.getOrCreate(1),
        registry.getOrCreate(1),
        registry.getOrCreate(1),
      ]);

      expect(first).toBe(second);
      expect(second).toBe(third);
      expect(first).not.toBeNull();
      // One coalesced creation, so exactly one xterm — the dedup the pending
      // map exists for.
      expect(control.terminals).toHaveLength(1);
      expect(listensFor('agent-output')).toHaveLength(1);
      expect(ipcFor('subscribe_agent_output')).toHaveLength(1);
    });

    it('gives every concurrent caller of one pending creation the same deleted-node answer', async () => {
      registry = await loadRegistry();

      const first = registry.getOrCreate(1);
      const second = registry.getOrCreate(1);
      const third = registry.getOrCreate(1);
      registry.dispose(1);

      expect(await Promise.all([first, second, third])).toEqual([null, null, null]);
      expect(control.terminals).toHaveLength(0);
      expect(listensFor('agent-output')).toHaveLength(0);
      expect(ipcFor('subscribe_agent_output')).toHaveLength(0);
    });

    it('leaves the node reusable after a cancelled creation', async () => {
      registry = await loadRegistry();

      const cancelled = registry.getOrCreate(1);
      registry.dispose(1);
      expect(await cancelled).toBeNull();

      // No tombstone: the next attach for the same id builds a live terminal.
      const inst = await registry.getOrCreate(1);
      expect(inst).not.toBeNull();
      expect(registry.getInstance(1)).toBe(inst);
      expect(control.terminals).toHaveLength(1);
    });
  });

  describe('registration-stage failures', () => {
    it('releases the terminal and its keyed entries when the agent-output registration rejects', async () => {
      control.rejectListen = new Set(['agent-output']);
      registry = await loadRegistry();
      const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

      try {
        expect(await registry.getOrCreate(1)).toBeNull();
        expect(registry.getInstance(1)).toBeUndefined();
        expect(control.terminals).toHaveLength(1);
        expect(control.terminals[0].dispose).toHaveBeenCalledTimes(1);
        // The failed listen never yielded a handle, so there is nothing to
        // unlisten — but it must not leave the writer sink behind either.
        emitAgentOutput(1, { line: 'hello' });
        expect(control.terminals[0].write).not.toHaveBeenCalled();
      } finally {
        errorSpy.mockRestore();
      }
    });

    it('releases the JSON listener and the binary Channel when the serialize registration rejects', async () => {
      control.rejectListen = new Set(['serialize-terminal-request']);
      registry = await loadRegistry();
      const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

      try {
        expect(await registry.getOrCreate(1)).toBeNull();
        expect(registry.getInstance(1)).toBeUndefined();
        expect(control.terminals[0].dispose).toHaveBeenCalledTimes(1);
        expect(listensFor('agent-output')[0].unlisten).toHaveBeenCalledTimes(1);
        await waitFor(() => ipcFor('unsubscribe_agent_output').length === 1, 'the output unsubscribe');
        expect(ipcFor('unsubscribe_agent_output')[0].args).toEqual({ sessionId: 1 });
        emitChannelBytes(1, new Uint8Array([0x68]));
        expect(control.terminals[0].write).not.toHaveBeenCalled();
      } finally {
        errorSpy.mockRestore();
      }
    });

    it('keeps the documented JSON-event fallback when only the binary subscription fails', async () => {
      // The binary path is fire-and-forget by design (pre-subscribe bytes are
      // buffered on the backend OutputSink), so a rejected Channel must not
      // sink the whole creation.
      control.rejectSubscribe = true;
      registry = await loadRegistry();
      const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

      try {
        const inst = await registry.getOrCreate(1);
        expect(inst).not.toBeNull();
        expect(registry.getInstance(1)).toBe(inst);

        emitAgentOutput(1, { line: 'still-wired' });
        await waitFor(() => control.terminals[0].write.mock.calls.length > 0, 'the JSON fallback write');
      } finally {
        errorSpy.mockRestore();
      }
    });
  });

  describe('destroy', () => {
    it('releases a module-level listener that resolves after teardown', async () => {
      control.holdListen = (event) => event === 'agent-spawned';
      registry = await loadRegistry();

      const pending = listensFor('agent-spawned')[0];
      registry.destroy();
      expect(pending.unlisten).not.toHaveBeenCalled();

      pending.gate.resolve(pending.unlisten);
      await tick();

      // The handle was released instead of parked in `unlistenFns` for the
      // rest of the process.
      expect(pending.unlisten).toHaveBeenCalledTimes(1);
    });

    it('releases a creation that is still in flight', async () => {
      registry = await loadRegistry();

      const created = registry.getOrCreate(1);
      registry.destroy();

      expect(await created).toBeNull();
      expect(registry.getInstance(1)).toBeUndefined();
      expect(control.terminals).toHaveLength(0);
    });

    it('releases a creation parked on a held registration', async () => {
      control.holdListen = (event) => event === 'agent-output';
      registry = await loadRegistry();

      const created = registry.getOrCreate(1);
      await waitFor(() => listensFor('agent-output').length === 1, 'the agent-output registration');

      registry.destroy();
      const held = listensFor('agent-output')[0];
      held.gate.resolve(held.unlisten);

      expect(await created).toBeNull();
      expect(registry.getInstance(1)).toBeUndefined();
      expect(control.terminals[0].dispose).toHaveBeenCalledTimes(1);
      expect(held.unlisten).toHaveBeenCalledTimes(1);
    });
  });

  describe('persistent-terminal contract (unchanged by #2016)', () => {
    it('detach neither cancels an in-flight creation nor disposes a live terminal', async () => {
      control.holdListen = (event) => event === 'agent-output';
      registry = await loadRegistry();

      // A mesh switch detaches while the first attach is still registering.
      const container = document.createElement('div');
      const attaching = registry.attach(1, container);
      await waitFor(() => listensFor('agent-output').length === 1, 'the agent-output registration');
      registry.detach(1);
      const held = listensFor('agent-output')[0];
      held.gate.resolve(held.unlisten);

      const inst = await attaching;
      expect(inst).not.toBeNull();
      expect(registry.getInstance(1)).toBe(inst);
      expect(control.terminals[0].dispose).not.toHaveBeenCalled();

      // Detach detaches; it never disposes the surviving node.
      registry.detach(1);
      expect(registry.getInstance(1)).toBe(inst);
      expect(control.terminals[0].dispose).not.toHaveBeenCalled();
      expect(ipcFor('unsubscribe_agent_output')).toHaveLength(0);
    });

    it('attaches, re-attaches, renders output, serializes, pastes and resizes after a cancelled creation', async () => {
      registry = await loadRegistry();

      const cancelled = registry.getOrCreate(1);
      registry.dispose(1);
      expect(await cancelled).toBeNull();

      const first = document.createElement('div');
      const inst = (await registry.attach(1, first))!;
      expect(inst).not.toBeNull();
      expect(inst.term.open).toHaveBeenCalledExactlyOnceWith(first);

      // Re-attach (fluid-grid pane swap) re-parents the same xterm.
      const second = document.createElement('div');
      const reattached = (await registry.attach(1, second))!;
      expect(reattached).toBe(inst);
      expect(inst.term.open).toHaveBeenCalledTimes(1);
      expect(control.terminals).toHaveLength(1);

      // Output over the binary Channel.
      emitChannelBytes(1, new Uint8Array([0x6f, 0x6b]));
      await waitFor(() => control.terminals[0].write.mock.calls.length > 0, 'the output write');

      // A snapshot request round-trips through the live instance.
      for (const callback of liveCallbacks.get('serialize-terminal-request') ?? []) {
        callback({ payload: { node_id: 1, request_id: 'req-1' } });
      }
      await waitFor(() => ipcFor('submit_terminal_snapshot').length === 1, 'the snapshot IPC');
      expect(ipcFor('submit_terminal_snapshot')[0].args).toEqual({ requestId: 'req-1', data: 'snapshot' });

      // Paste reaches the PTY through the shared clipboard action.
      inst.useNativeClipboardPaste = true;
      await registry.pasteClipboard(1);
      expect(ipcFor('write_to_agent').map((entry) => entry.args)).toContainEqual({
        sessionId: 1,
        data: '\x16',
      });

      // Resize reaches the PTY.
      registry.fit(1);
      expect(ipcFor('resize_agent').length).toBeGreaterThan(0);

      // Only an explicit deletion tears the terminal down.
      registry.dispose(1);
      expect(control.terminals[0].dispose).toHaveBeenCalledTimes(1);
      expect(registry.getInstance(1)).toBeUndefined();
    });
  });
});