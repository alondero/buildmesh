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
import { emit } from '@tauri-apps/api/event';

// jsdom doesn't ship ResizeObserver and the registry instantiates one in
// `attachToDOM`. Stub it out before the registry import.
globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

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
    onResize = vi.fn();
    open = vi.fn((container: HTMLElement) => {
      this.element = container as unknown as HTMLElement;
    });
    dispose = vi.fn();
    focus = vi.fn();
    loadAddon = vi.fn();
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

vi.mock('@xterm/addon-webgl', () => ({
  // Issue #1122: WebGL addon is loaded on every terminal. The mock exposes
  // `onContextLoss` so the production loader's fallback handler can subscribe
  // without throwing.
  WebglAddon: class { dispose = vi.fn(); onContextLoss = vi.fn(); },
}));

// Imports AFTER the mock override above so the registry picks up the
// tracked Terminal constructor.
import { buildRunTerminalManager } from '../../src/components/Terminal/BuildRunTerminalRegistry';

describe('BuildRunTerminalRegistry — persistence across remount (issue: build-run terminal resets on mesh navigation)', () => {
  beforeEach(() => {
    terminalInstances.length = 0;
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
});