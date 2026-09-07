/**
 * Regression test for the BuildRun terminal "resets on mesh navigation" bug.
 *
 * BuildRunTerminal used to dispose its xterm + kill its Rust PTY on every
 * React unmount. The user-visible symptom: open "BuildRun → Terminal in
 * worktree" on an agent node, switch to a different mesh in the sidebar,
 * switch back — terminal is empty, shell prompt is fresh, everything the
 * user was doing is gone. The agent terminal does NOT have this bug
 * because it uses a singleton TerminalRegistry whose attach/detach moves
 * the existing xterm element between DOM containers without touching the
 * Terminal object or the underlying PTY.
 *
 * The fix mirrors that singleton pattern in a sibling
 * `BuildRunTerminalRegistry`. These tests pin the singleton contract at
 * the registry boundary (Tests A/B/D) and the React lifecycle boundary
 * (Tests C/E/F) so future regressions on the same axis surface locally
 * instead of as a "the terminal wiped again" report.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { emit, listen } from '@tauri-apps/api/event';

// jsdom doesn't ship ResizeObserver. Keep each observer instance so the
// resize scheduler can be driven without a module-level callback singleton.
const resizeObservers: Array<{
  callback: ResizeObserverCallback;
  trigger: () => void;
}> = [];
const originalResizeObserver = globalThis.ResizeObserver;

class MockResizeObserver {
  private readonly callback: ResizeObserverCallback;

  constructor(callback: ResizeObserverCallback) {
    this.callback = callback;
    resizeObservers.push({
      callback,
      trigger: () => this.callback([], this as unknown as ResizeObserver),
    });
  }

  observe(): void {}
  disconnect(): void {}
}

// Capture every `new Terminal()` instance the registry creates so we can
// assert "the same Terminal object survived remount" at the lifecycle
// boundary. The global setup already mocks @xterm/xterm but the
// constructor there is anonymous; this local mock pushes the instance
// into an array we control. Mirrors the override pattern in
// `build-run-terminal-raf-batching.test.tsx:33-65`.
const terminalInstances: Array<{
  write: ReturnType<typeof vi.fn>;
  open: ReturnType<typeof vi.fn>;
  dispose: ReturnType<typeof vi.fn>;
  refresh: ReturnType<typeof vi.fn>;
  scrollToBottom: ReturnType<typeof vi.fn>;
  rows: number;
  cols: number;
  element: HTMLElement | null;
}> = [];

vi.mock('@xterm/xterm', () => {
  class TrackedTerminal {
    write = vi.fn();
    onData = vi.fn();
    resizeCallback: ((size: { cols: number; rows: number }) => void) | undefined;
    onResize = vi.fn((callback: (size: { cols: number; rows: number }) => void) => {
      this.resizeCallback = callback;
    });
    open = vi.fn((container: HTMLElement) => {
      this.element = container as unknown as HTMLElement;
    });
    dispose = vi.fn();
    focus = vi.fn();
    loadAddon = vi.fn((addon: unknown) => {
      if (
        addon !== null &&
        typeof addon === 'object' &&
        'attachTerminal' in addon &&
        typeof addon.attachTerminal === 'function'
      ) {
        addon.attachTerminal(this);
      }
    });
    attachCustomKeyEventHandler = vi.fn();
    scrollToBottom = vi.fn();
    refresh = vi.fn();
    resize = vi.fn();
    registerCharacterJoiner = vi.fn();
    // Minimal `unicode`/`_core` shape — loadUnicode11Widths writes through it
    // during render; without these stubs the helper throws and the effect
    // never registers a listener.
    unicode = {
      register: vi.fn(),
      get activeVersion() { return '11'; },
      set activeVersion(_v: string) {},
    };
    _core = {
      unicodeService: {
        _providers: { '11': { version: '11', wcwidth: () => 1, charProperties: () => 0 } },
        register: vi.fn(),
      },
    };
    buffer = { active: { getWindow: vi.fn() } };
    rows = 24;
    cols = 80;
    element: HTMLElement | null = null;
    constructor(_opts?: unknown) {
      terminalInstances.push(this as unknown as typeof terminalInstances[number]);
    }
  }
  return { Terminal: TrackedTerminal };
});

vi.mock('@xterm/addon-fit', () => {
  class TrackedFitAddon {
    private terminal: { resizeCallback?: (size: { cols: number; rows: number }) => void } | null = null;

    attachTerminal(terminal: { resizeCallback?: (size: { cols: number; rows: number }) => void }): void {
      this.terminal = terminal;
    }

    fit = vi.fn(() => {
      this.terminal?.resizeCallback?.({ cols: 80, rows: 24 });
    });
    dispose = vi.fn();
    proposeDimensions = vi.fn().mockReturnValue({ cols: 80, rows: 24 });
  }

  return { FitAddon: TrackedFitAddon };
});

vi.mock('@xterm/addon-webgl', () => ({
  // Issue #1122: WebGL addon is loaded on every terminal. The mock exposes
  // `onContextLoss` so the production loader's fallback handler can subscribe
  // without throwing.
  WebglAddon: class { dispose = vi.fn(); onContextLoss = vi.fn(); },
}));

// Imports AFTER the mock override above so the registry picks up the
// tracked Terminal constructor.
import { buildRunTerminalManager } from '../../src/components/Terminal/BuildRunTerminalRegistry';

beforeEach(() => {
  globalThis.ResizeObserver = MockResizeObserver as unknown as typeof ResizeObserver;
});

afterEach(() => {
  globalThis.ResizeObserver = originalResizeObserver;
});

describe('BuildRunTerminalRegistry — persistence across remount (issue: build-run terminal resets on mesh navigation)', () => {
  beforeEach(() => {
    terminalInstances.length = 0;
    resizeObservers.length = 0;
    // The setup's `beforeEach` already calls `vi.clearAllMocks()` which
    // wipes `invoke.mock.calls` between tests, but call it explicitly so
    // each test's assertions are obvious.
    vi.mocked(invoke).mockClear();
  });

  afterEach(() => {
    // Tear down any lingering instances so the registry is clean for
    // the next test (otherwise getInstance would still return them).
    buildRunTerminalManager.destroy();
  });

  it('A: reuses the same Terminal instance across attach/detach/attach cycles', async () => {
    const containerA = document.createElement('div');
    await buildRunTerminalManager.attach(7, 'terminal', true, containerA);
    expect(terminalInstances).toHaveLength(1);
    const firstTerm = terminalInstances[0];

    // Simulate mesh switch: detach (DOM-only, instance survives).
    buildRunTerminalManager.detach(7, 'terminal', true);
    expect(terminalInstances).toHaveLength(1);

    // Simulate returning to the mesh: re-attach to a different container.
    const containerB = document.createElement('div');
    await buildRunTerminalManager.attach(7, 'terminal', true, containerB);

    // CRITICAL ASSERTION: same Terminal instance, not a fresh one. If
    // the registry were re-creating on re-attach the xterm scrollback
    // would be empty and the user would see a "reset" terminal.
    expect(terminalInstances).toHaveLength(1);
    expect(terminalInstances[0]).toBe(firstTerm);

    // CRITICAL ASSERTION: build_run invoked exactly once across the full
    // cycle. A second invocation would mean a second PTY was spawned,
    // which would overwrite the first in BUILD_RUN_REGISTRY and orphan
    // the original shell process (its master is leaked under the old Arc
    // until GC). More importantly for the user, the new PTY would have
    // an empty prompt — the visible "reset".
    const buildRunCalls = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'build_run',
    );
    expect(buildRunCalls).toHaveLength(1);
    expect(buildRunCalls[0]).toEqual(['build_run', { nodeId: 7, mode: 'terminal' }]);

    // CRITICAL ASSERTION: close_build_run NEVER invoked during the cycle.
    // It must only fire on explicit close (X button), never on React
    // unmount. The user explicitly clicked the X in step 2 of the manual
    // verification, but never in the remount flow.
    const closeCalls = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'close_build_run',
    );
    expect(closeCalls).toHaveLength(0);
  });

  it('coalesces interactive terminal resize observations', async () => {
    vi.useFakeTimers();
    const originalRequestAnimationFrame = globalThis.requestAnimationFrame;
    const rafQueue: Array<() => void> = [];
    vi.stubGlobal('requestAnimationFrame', (callback: () => void) => {
      rafQueue.push(callback);
      return 0;
    });

    try {
      const container = document.createElement('div');
      const inst = await buildRunTerminalManager.attach(75, 'terminal', true, container);
      expect(resizeObservers).toHaveLength(1);
      while (rafQueue.length > 0) rafQueue.shift()!(); // initial fit + repaint
      vi.mocked(inst!.fitAddon.fit).mockClear();
      vi.mocked(invoke).mockClear();

      for (let i = 0; i < 4; i++) {
        resizeObservers[0].trigger();
        vi.advanceTimersByTime(25);
      }

      expect(inst!.fitAddon.fit).not.toHaveBeenCalled();
      vi.advanceTimersByTime(25); // max wait flushes at 100 ms
      expect(rafQueue).toHaveLength(1);
      rafQueue.shift()!();
      expect(inst!.fitAddon.fit).toHaveBeenCalledTimes(1);
      expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === 'resize_build_run'))
        .toEqual([['resize_build_run', { nodeId: 75, rows: 24, cols: 80 }]]);
    } finally {
      vi.stubGlobal('requestAnimationFrame', originalRequestAnimationFrame);
      vi.useRealTimers();
    }
  });

  it('B: dispose() tears down the instance AND kills the Rust PTY (X-button path)', async () => {
    const container = document.createElement('div');
    await buildRunTerminalManager.attach(8, 'terminal', true, container);
    expect(terminalInstances).toHaveLength(1);

    // Explicit close — the X button path. Must kill the PTY (otherwise
    // a leak accumulates every time the user opens then closes a
    // terminal). Flush microtasks so the api.buildRun().then() callback
    // that flips ptyAlive=true has fired — in real usage the user takes
    // hundreds of ms between open and X-click, but the test goes straight
    // from `await attach()` to `dispose()` and would otherwise race.
    await Promise.resolve();
    buildRunTerminalManager.dispose(8, 'terminal', true);

    const closeCalls = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'close_build_run',
    );
    expect(closeCalls).toHaveLength(1);
    expect(closeCalls[0]).toEqual(['close_build_run', { nodeId: 8 }]);

    // The instance is fully removed — a subsequent attach on the same
    // sessionId should respawn.
    terminalInstances.length = 0;
    vi.mocked(invoke).mockClear();
    await buildRunTerminalManager.attach(8, 'terminal', true, container);
    expect(terminalInstances).toHaveLength(1);
    const buildRunCalls = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'build_run',
    );
    expect(buildRunCalls).toHaveLength(1);
    expect(buildRunCalls[0]).toEqual(['build_run', { nodeId: 8, mode: 'terminal' }]);
  });

  it('closes a PTY when explicit dispose races the build_run resolution', async () => {
    let resolveBuildRun!: () => void;
    const buildRunPending = new Promise<void>((resolve) => { resolveBuildRun = resolve; });
    vi.mocked(invoke).mockImplementation((command: string) =>
      command === 'build_run' ? buildRunPending : Promise.resolve({}),
    );

    const container = document.createElement('div');
    const attachPromise = buildRunTerminalManager.attach(81, 'terminal', true, container);
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(vi.mocked(invoke).mock.calls.some(([command]) => command === 'build_run')).toBe(true);

    // The instance is removed while the Rust command is still in flight.
    // The eventual resolution must close the PTY instead of reviving the
    // removed instance or leaving an unreachable child process behind.
    buildRunTerminalManager.dispose(81, 'terminal', true);
    expect(vi.mocked(invoke).mock.calls.some(([command]) => command === 'close_build_run')).toBe(false);

    resolveBuildRun();
    expect(await attachPromise).toBeNull();
    expect(vi.mocked(invoke).mock.calls).toEqual(expect.arrayContaining([
      ['close_build_run', { nodeId: 81 }],
    ]));
    expect(buildRunTerminalManager.getInstance(81, 'terminal', true)).toBeUndefined();
  });

  it('awaits deferred PTY close and output unsubscribe before a replacement spawn', async () => {
    let resolveClose!: () => void;
    let resolveUnsubscribe!: () => void;
    const closePending = new Promise<void>((resolve) => { resolveClose = resolve; });
    const unsubscribePending = new Promise<void>((resolve) => { resolveUnsubscribe = resolve; });
    vi.mocked(invoke).mockImplementation((command: string) => {
      if (command === 'close_build_run') return closePending;
      if (command === 'unsubscribe_build_run_output') return unsubscribePending;
      return Promise.resolve({});
    });

    const container = document.createElement('div');
    await buildRunTerminalManager.attach(83, 'build', true, container);

    // The mode switch must wait for both teardown IPC calls. If it spawned
    // immediately, the replacement's build_run/subscribe calls could be
    // removed by the old close/unsubscribe resolutions.
    const replacement = buildRunTerminalManager.attach(83, 'run', true, container);
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === 'build_run')).toHaveLength(1);
    expect(vi.mocked(invoke).mock.calls).toEqual(expect.arrayContaining([
      ['close_build_run', { nodeId: 83 }],
      ['unsubscribe_build_run_output', { sessionId: 83 }],
    ]));

    resolveClose();
    resolveUnsubscribe();
    await expect(replacement).resolves.toBeDefined();

    const calls = vi.mocked(invoke).mock.calls.map(([command]) => command);
    const closeIndex = calls.indexOf('close_build_run');
    const unsubscribeIndex = calls.indexOf('unsubscribe_build_run_output');
    const replacementBuildIndex = calls.lastIndexOf('build_run');
    expect(replacementBuildIndex).toBeGreaterThan(closeIndex);
    expect(replacementBuildIndex).toBeGreaterThan(unsubscribeIndex);
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === 'build_run')).toHaveLength(2);
  });

  it('does not retain a registry instance when attach is aborted before lazy creation finishes', async () => {
    let resolveOutputListener!: (unlisten: () => void) => void;
    const outputListenerPending = new Promise<() => void>((resolve) => { resolveOutputListener = resolve; });
    vi.mocked(listen).mockImplementationOnce(() => outputListenerPending as never);

    const controller = new AbortController();
    const container = document.createElement('div');
    const attachPromise = buildRunTerminalManager.attach(82, 'terminal', true, container, controller.signal);
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(vi.mocked(listen)).toHaveBeenCalled();

    controller.abort();
    buildRunTerminalManager.dispose(82, 'terminal', true);
    resolveOutputListener(() => {});

    expect(await attachPromise).toBeNull();
    expect(buildRunTerminalManager.getInstance(82, 'terminal', true)).toBeUndefined();
    expect(terminalInstances[0].dispose).toHaveBeenCalled();
    expect(vi.mocked(invoke).mock.calls.some(([command]) => command === 'build_run')).toBe(false);
  });

  it('D: deduplicates concurrent attach() calls into one doCreate + one build_run', async () => {
    // Three concurrent attaches on the same sessionId — e.g. a fast
    // React Strict-Mode double-render, or two NodeCards mounting at the
    // same tick. Must collapse to one doCreate, one Terminal, one PTY.
    //
    // Use three distinct containers: real React remounts give a fresh
    // <div>, and a single container would make the mock's `term.element
    // = container` self-cycle on the second appendChild.
    const c1 = document.createElement('div');
    const c2 = document.createElement('div');
    const c3 = document.createElement('div');
    const [a, b, c] = await Promise.all([
      buildRunTerminalManager.attach(10, 'terminal', true, c1),
      buildRunTerminalManager.attach(10, 'terminal', true, c2),
      buildRunTerminalManager.attach(10, 'terminal', true, c3),
    ]);

    expect(a).toBe(b);
    expect(b).toBe(c);
    expect(terminalInstances).toHaveLength(1);

    const buildRunCalls = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'build_run',
    );
    expect(buildRunCalls).toHaveLength(1);
  });

  it('C: build-run-output events flow into the xterm while detached', async () => {
    // The listener is registered once at doCreate and writes to the xterm
    // via TerminalWriter regardless of whether the xterm is currently
    // attached to a DOM container. This is what lets the scrollback
    // accumulate while the user is on another mesh.
    const container = document.createElement('div');
    await buildRunTerminalManager.attach(20, 'build', true, container);
    const term = terminalInstances[0];
    term.write.mockClear();

    // Detach (simulate user navigating to another mesh).
    buildRunTerminalManager.detach(20, 'build', true);

    // Emit a build line while detached. The xterm's scrollback should
    // accumulate it; on re-attach, the refresh() repaints it.
    await emit('build-run-output-20', 'first line\n');
    // TerminalWriter coalesces via RAF; flush manually.
    await new Promise((r) => requestAnimationFrame(() => r(null)));

    expect(term.write).toHaveBeenCalled();
    const written = term.write.mock.calls.map((c) => c[0]).join('');
    expect(written).toContain('first line');

    // Re-attach to a different container; scrollback should be repainted.
    const newContainer = document.createElement('div');
    await buildRunTerminalManager.attach(20, 'build', true, newContainer);
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    expect(term.refresh).toHaveBeenCalled();
  });

  it('E: switching mode for the same sessionId disposes the old PTY before spawning the new one', async () => {
    // A user opens Build, then without closing opens Terminal on the same
    // node. Only one PTY can exist per node_id in BUILD_RUN_REGISTRY, so
    // we must dispose the old one (which calls api.closeBuildRun) before
    // api.buildRun can spawn the new one.
    const container = document.createElement('div');
    await buildRunTerminalManager.attach(30, 'build', true, container);
    const firstTerm = terminalInstances[0];
    expect(firstTerm).toBeDefined();

    // Switch mode to terminal — must dispose first then respawn.
    await buildRunTerminalManager.attach(30, 'terminal', true, container);
    expect(terminalInstances).toHaveLength(2);

    const closeCalls = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'close_build_run',
    );
    expect(closeCalls).toHaveLength(1);
    expect(closeCalls[0]).toEqual(['close_build_run', { nodeId: 30 }]);

    const buildRunCalls = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'build_run',
    );
    expect(buildRunCalls).toHaveLength(2);
    expect(buildRunCalls[0]).toEqual(['build_run', { nodeId: 30, mode: 'build' }]);
    expect(buildRunCalls[1]).toEqual(['build_run', { nodeId: 30, mode: 'terminal' }]);
  });

  it('F: writes the opening banner only on first attach, not on re-attach', async () => {
    // Re-attaching an existing terminal must NOT clobber the scrollback
    // with a second banner, and must NOT respawn the PTY.
    const c1 = document.createElement('div');
    await buildRunTerminalManager.attach(40, 'terminal', true, c1);
    const term = terminalInstances[0];

    const bannerWrite = term.write.mock.calls
      .map((c) => c[0])
      .join('');
    expect(bannerWrite).toContain('Opening terminal');

    term.write.mockClear();
    const c2 = document.createElement('div');
    await buildRunTerminalManager.attach(40, 'terminal', true, c2);

    // The RAF inside attachToDOM hasn't fired yet — flush it so refresh()
    // and fit() actually run before we assert.
    await new Promise<void>((r) => requestAnimationFrame(() => r()));

    // No second banner.
    const reattachWrite = term.write.mock.calls
      .map((c) => c[0])
      .join('');
    expect(reattachWrite).not.toContain('Opening terminal');
    // But refresh() ran, which is what repaints the accumulated scrollback.
    expect(term.refresh).toHaveBeenCalled();
  });
});

describe('BuildRunTerminal component — survival of the user-reported bug', () => {
  // Component-level regression for the original report: open the terminal
  // pane via the BuildRun dropdown, simulate a mesh switch (NodeCard
  // unmount → BuildRunTerminal unmount → React effect cleanup runs `detach`),
  // then simulate the user returning (NodeCard remount → BuildRunTerminal
  // remount → `attach` into a fresh container). The xterm scrollback, the
  // Terminal object, and the Rust PTY must all survive the cycle.

  beforeEach(() => {
    terminalInstances.length = 0;
    vi.mocked(invoke).mockClear();
  });

  afterEach(() => {
    buildRunTerminalManager.destroy();
  });

  it('survives unmount/remount (the user-reported mesh-navigation scenario)', async () => {
    const { BuildRunTerminal } = await import('../../src/components/Terminal/BuildRunTerminal');
    const { render, unmount, waitFor } = await import('@testing-library/react');

    // Initial mount — simulates the user clicking BuildRun → "Terminal in worktree".
    const { unmount: unmount1 } = render(
      <BuildRunTerminal sessionId={50} mode="terminal" useWorktree={true} />,
    );
    await waitFor(() => expect(terminalInstances).toHaveLength(1));
    const firstTerm = terminalInstances[0];

    // Simulate the user navigating to a different mesh (NodeCard unmounts,
    // BuildRunTerminal's effect cleanup runs `detach`).
    unmount1();
    expect(terminalInstances).toHaveLength(1); // xterm NOT disposed

    // Simulate the user navigating back. A fresh NodeCard mounts, with a
    // fresh container <div>. BuildRunTerminal mounts, calls `attach` into
    // the new container.
    const { unmount: unmount2 } = render(
      <BuildRunTerminal sessionId={50} mode="terminal" useWorktree={true} />,
    );
    await waitFor(() => expect(terminalInstances).toHaveLength(1));

    // CRITICAL: same Terminal instance (same scrollback).
    expect(terminalInstances[0]).toBe(firstTerm);
    // CRITICAL: build_run was called exactly ONCE — no PTY respawn.
    const buildRunCalls = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'build_run',
    );
    expect(buildRunCalls).toHaveLength(1);
    // CRITICAL: close_build_run NEVER called during navigation.
    const closeCalls = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'close_build_run',
    );
    expect(closeCalls).toHaveLength(0);

    unmount2();
  });

  it('G: build-run-exited-{sessionId} sentinel marks ptyAlive=false and writes banner if attached', async () => {
    // After the singleton fix, a shell that exits naturally while the user
    // is on another mesh would leave a zombie PTY in Rust. The frontend
    // listener for `build-run-exited-{sessionId}` must flip ptyAlive=false
    // (so subsequent writeToBuildRun calls cleanly hit the "not running"
    // path) and, if the xterm is currently attached, surface a visible
    // "[process exited]" banner so the user understands the dead state.
    const container = document.createElement('div');
    await buildRunTerminalManager.attach(60, 'terminal', true, container);
    const term = terminalInstances[0];
    term.write.mockClear();

    await emit('build-run-exited-60', {});

    // ptyAlive should now be false. Assert indirectly: a writeToBuildRun
    // call after the sentinel should NOT be a successful IPC round-trip
    // — the registry's `ptyAlive` flag drives a `catch` swallow on
    // "Build run not running" (which our mock resolves with {}), so the
    // observable behavior is that subsequent write attempts still call
    // invoke (we don't gate them on ptyAlive at the JS level — Rust does
    // the gating via "not running" errors). The visible assertion is the
    // banner: if attached, the xterm received "\r\n[process exited]\r\n".
    await new Promise<void>((r) => requestAnimationFrame(() => r()));
    const written = term.write.mock.calls.map((c) => c[0]).join('');
    expect(written).toContain('[process exited]');
  });

  it('ignores a delayed exit from a disposed PTY after reopening the same session', async () => {
    const delayedExitHandlers: Array<(event: { payload: unknown }) => void> = [];
    const defaultListen = vi.mocked(listen).getMockImplementation();
    vi.mocked(listen).mockImplementation((eventName: string, handler: (event: { payload: unknown }) => void) => {
      if (eventName === 'build-run-exited-84') delayedExitHandlers.push(handler);
      // Keep the old exit callback registered so this test models a late
      // backend event whose frontend unlisten has not taken effect yet.
      return Promise.resolve(() => {});
    });

    try {
      const container = document.createElement('div');
      const first = await buildRunTerminalManager.attach(84, 'terminal', true, container);
      const firstGeneration = first!.generation;
      await buildRunTerminalManager.dispose(84, 'terminal', true);

      const replacement = await buildRunTerminalManager.attach(84, 'terminal', true, container);
      expect(replacement!.generation).toBeGreaterThan(firstGeneration);
      expect(delayedExitHandlers).toHaveLength(2);
      replacement!.term.write.mockClear();

      // The old PTY's event must be rejected by both the tombstoned
      // generation and the current-instance identity check. The replacement
      // remains alive and does not show a false process-exited banner.
      delayedExitHandlers[0]({ payload: {} });
      expect(replacement!.ptyAlive).toBe(true);
      expect(replacement!.term.write).not.toHaveBeenCalledWith(
        expect.stringContaining('[process exited]'),
      );
    } finally {
      if (defaultListen) vi.mocked(listen).mockImplementation(defaultListen);
    }
  });

  it('H: subscribes to the binary Channel on create and unsubscribes only on dispose', async () => {
    const container = document.createElement('div');
    await buildRunTerminalManager.attach(70, 'build', true, container);

    expect(vi.mocked(invoke).mock.calls).toEqual(
      expect.arrayContaining([
        ['subscribe_build_run_output', { sessionId: 70, onChunk: expect.any(Object) }],
      ]),
    );

    vi.mocked(invoke).mockClear();
    buildRunTerminalManager.detach(70, 'build', true);
    expect(
      vi.mocked(invoke).mock.calls.some(([cmd]) => cmd === 'unsubscribe_build_run_output'),
      'detach (mesh switch) must keep the session-scoped Channel',
    ).toBe(false);

    const containerB = document.createElement('div');
    await buildRunTerminalManager.attach(70, 'build', true, containerB);
    expect(
      vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'subscribe_build_run_output'),
    ).toHaveLength(0);

    await Promise.resolve();
    buildRunTerminalManager.dispose(70, 'build', true);
    expect(vi.mocked(invoke).mock.calls).toEqual(
      expect.arrayContaining([
        ['unsubscribe_build_run_output', { sessionId: 70 }],
      ]),
    );
  });

  it('I: Channel frames write through to xterm, including the fetch-path Response', async () => {
    const container = document.createElement('div');
    await buildRunTerminalManager.attach(71, 'run', true, container);

    const subscribeCall = vi.mocked(invoke).mock.calls.find(
      ([cmd]) => cmd === 'subscribe_build_run_output',
    );
    expect(subscribeCall).toBeDefined();
    const { onChunk } = subscribeCall![1] as {
      onChunk: { onmessage: (message: unknown) => void };
    };

    const term = terminalInstances[0];
    term.write.mockClear();

    const startupFrame = new Uint8Array(1024).fill('B'.charCodeAt(0));
    onChunk.onmessage(new Response(startupFrame.buffer));
    // Response.arrayBuffer() is async; give the Channel decode queue a
    // macrotask, then flush TerminalWriter's rAF coalesce.
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise<void>((r) => requestAnimationFrame(() => r()));

    expect(term.write).toHaveBeenCalledWith(startupFrame);
  });
});
