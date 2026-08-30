/**
 * Regression test for the auto-resume PTY/term size desync (issue #332).
 *
 * Symptom: at app startup, an auto-resumed agent node wraps its first lines
 * of PTY output at FitAddon's 80x24 fallback regardless of how wide the
 * pane actually is. The backend's `auto_resume_agent_nodes` hardcodes 80x24
 * when calling `open_pty_pair`, and the frontend's attach-effect fires
 * `resize_agent(real cols)` before the agent process is up — so the
 * resize is silently rejected with "Agent not running" and never re-tries.
 * Term cols is already the real width (no further onResize), so the PTY
 * stays at 80 cols and the resumed session's first banner is permanently
 * wrapped.
 *
 * Same root cause as #302 (fresh auto-spawn) but on a different trigger:
 * the agent is async-spawned AFTER the term attaches, so the proper fix
 * in #302 (call `attach()` before `proposeDimensions`) doesn't apply —
 * the term is already parented and measured correctly; the PTY was just
 * created at the wrong size before the agent existed.
 *
 * The right contract is a backend "agent is up" event the frontend
 * reconciles PTY size against. This test asserts that when the registry
 * receives such an event (`agent-spawned`) for a session, it pushes the
 * terminal's current dimensions to the PTY via `resize_agent` — closing
 * the race for all three async-spawn paths uniformly:
 *   1. Auto-resume on startup (this regression)
 *   2. Fresh auto-spawn (#302 — already fixed via attach-first)
 *   3. Any future async-spawn
 *
 * Run with: npm test -- --run tests/unit/terminal-registry-agent-spawned.test.ts
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { TerminalRegistry } from '../../src/components/Terminal/TerminalRegistry';

globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

// Capture the callbacks `listen()` registers on the module, so the test can
// trigger Tauri events synchronously without needing a real WebView. The
// production path is: backend `app.emit("agent-spawned", ...)` → Tauri
// delivers to all `listen("agent-spawned", ...)` callbacks → the registry's
// handler calls `syncPtySize` → `invoke("resize_agent", ...)` reaches the
// backend. This mock short-circuits the IPC and replays the same call chain
// from the test's side.
const eventCallbacks = new Map<string, Set<(event: { payload: unknown }) => void>>();

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockImplementation((event: string, callback: (event: { payload: unknown }) => void) => {
    if (!eventCallbacks.has(event)) eventCallbacks.set(event, new Set());
    eventCallbacks.get(event)!.add(callback);
    return Promise.resolve(() => {
      eventCallbacks.get(event)?.delete(callback);
    });
  }),
}));

vi.mock('@tauri-apps/api/core', async () => {
  const { MockChannel } = await import('../setup/tauriChannel');
  return {
    invoke: vi.fn().mockResolvedValue({}),
    Channel: MockChannel,
  };
});

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
    unicode = {
      register: vi.fn(),
      get activeVersion() { return '11'; },
      set activeVersion(_v: string) { /* noop */ },
    };
    _core = { unicodeService: { _providers: { '11': { version: '11', wcwidth: () => 1, charProperties: () => 0 } }, register: vi.fn() } };
    rows = 24;
    cols = 80;
    options = { fontSize: 10 };
    element: HTMLElement | null = null;
    constructor(_options?: unknown) {}
  }
  return { Terminal: MockTerminal };
});

vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class {
    fit = vi.fn();
    dispose = vi.fn();
    proposeDimensions = vi.fn().mockReturnValue({ cols: 80, rows: 24 });
  },
}));
vi.mock('@xterm/addon-serialize', () => ({
  SerializeAddon: class { serialize = vi.fn().mockReturnValue(''); dispose = vi.fn(); },
}));
vi.mock('@xterm/addon-search', () => ({
  SearchAddon: class { findNext = vi.fn(); findPrevious = vi.fn(); clearDecorations = vi.fn(); dispose = vi.fn(); },
}));
vi.mock('@xterm/addon-web-links', () => ({
  WebLinksAddon: class { dispose = vi.fn(); constructor(_h?: unknown) {} },
}));

vi.mock('@xterm/addon-webgl', () => ({
  // Issue #1122: WebGL addon is loaded on every terminal. The mock exposes
  // `onContextLoss` so the production loader's fallback handler can subscribe
  // without throwing.
  WebglAddon: class { dispose = vi.fn(); onContextLoss = vi.fn(); },
}));

function emitAgentSpawned(nodeId: number, rows: number, cols: number) {
  const callbacks = eventCallbacks.get('agent-spawned');
  if (!callbacks) throw new Error('no listener registered for agent-spawned');
  for (const cb of callbacks) {
    cb({ payload: { session_id: nodeId, rows, cols } });
  }
}

describe('TerminalRegistry agent-spawned reconcile (issue #332)', () => {
  let registry: TerminalRegistry;

  beforeEach(() => {
    eventCallbacks.clear();
    vi.clearAllMocks();
    registry = new TerminalRegistry();
  });

  afterEach(() => {
    registry.destroy();
  });

  it('registers a listener for agent-spawned at construction', async () => {
    // Yield to let the async listen() resolve.
    await new Promise((r) => setTimeout(r, 0));
    expect(eventCallbacks.has('agent-spawned')).toBe(true);
  });

  it('pushes the terminal\'s current dimensions to the PTY on agent-spawned', async () => {
    const container = document.createElement('div');
    const inst = await registry.attach(1, container);
    // Simulate the term having been measured to a wide pane (e.g. a full-
    // width partial row in the fluid grid) while the PTY is still at the
    // 80x24 spawn fallback. This is the exact state the auto-resume race
    // leaves the term in: term cols=180, PTY cols=80, no further onResize
    // fires because term.cols is already the fitted value.
    inst!.term.cols = 180;
    inst!.term.rows = 50;
    vi.mocked(invoke).mockClear();

    // Yield so the constructor's async listen() resolves and the listener
    // is registered by the time we emit.
    await new Promise((r) => setTimeout(r, 0));
    expect(eventCallbacks.has('agent-spawned')).toBe(true);

    // Backend has just registered the agent in PROCESS_REGISTRY. Emit the
    // post-spawn reconcile trigger.
    emitAgentSpawned(1, /* backend-spawned rows, ignored */ 24, /* cols */ 80);

    // The listener must push the term's real dimensions — the ones the
    // attach-fit already measured — to the PTY, so the agent stops
    // wrapping at 80 cols.
    expect(invoke).toHaveBeenCalledWith('resize_agent', { sessionId: 1, rows: 50, cols: 180 });
  });

  it('reconciles multiple resumed sessions independently', async () => {
    const container1 = document.createElement('div');
    const container2 = document.createElement('div');
    const inst1 = await registry.attach(10, container1);
    const inst2 = await registry.attach(20, container2);
    inst1!.term.cols = 200;
    inst1!.term.rows = 56;
    inst2!.term.cols = 120;
    inst2!.term.rows = 40;
    vi.mocked(invoke).mockClear();

    await new Promise((r) => setTimeout(r, 0));

    emitAgentSpawned(10, 24, 80);
    emitAgentSpawned(20, 24, 80);

    const calls = vi.mocked(invoke).mock.calls.filter(
      (c) => c[0] === 'resize_agent',
    );
    expect(calls).toContainEqual(['resize_agent', { sessionId: 10, rows: 56, cols: 200 }]);
    expect(calls).toContainEqual(['resize_agent', { sessionId: 20, rows: 40, cols: 120 }]);
  });

  it('is a no-op for a session whose terminal is not yet attached', async () => {
    // The backend may finish resuming a node before the user has navigated
    // to its pane. The reconcile must not throw, log a warning, or
    // resize_agent a non-existent session.
    await new Promise((r) => setTimeout(r, 0));
    vi.mocked(invoke).mockClear();

    expect(() => emitAgentSpawned(999, 24, 80)).not.toThrow();

    const resizeCalls = vi.mocked(invoke).mock.calls.filter(
      (c) => c[0] === 'resize_agent',
    );
    expect(resizeCalls).toEqual([]);
  });

  it('is a no-op for a session whose terminal exists but is detached', async () => {
    // The user may have switched panes away before the resume completes.
    // The listener must not crash and must not invoke resize_agent
    // against a PTY whose terminal is no longer in the DOM (syncPtySize
    // already self-guards on this).
    await registry.getOrCreate(7);
    registry.detach(7);
    await new Promise((r) => setTimeout(r, 0));
    vi.mocked(invoke).mockClear();

    expect(() => emitAgentSpawned(7, 24, 80)).not.toThrow();

    const resizeCalls = vi.mocked(invoke).mock.calls.filter(
      (c) => c[0] === 'resize_agent',
    );
    expect(resizeCalls).toEqual([]);
  });
});
