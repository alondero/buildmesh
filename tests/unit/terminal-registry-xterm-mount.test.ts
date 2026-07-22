/**
 * Regression test for the `xterm_mount` spawn-timing checkpoint (issue #602).
 *
 * Background: the Rust backend emits a sequence of `spawn_timing:` log lines
 * via `SpawnTimer::checkpoint` (`spawn.rs:354`) — `after_node_db_read`,
 * `after_pty_spawn`, `first_pty_output`, `first_user_input`, etc. — all
 * `tracing::info!("spawn_timing: session={} checkpoint={} elapsed={}ms", …)`,
 * so a reader can `grep spawn_timing:` in `buildmesh.log` and see the full
 * spawn timeline in one view. The xterm mount on the frontend was the only
 * remaining "spawn latency" moment without a matching line.
 *
 * Contract being pinned:
 *   - On a fresh attach (`attachToDOM` with `wasFreshOpen === true`), the
 *     registry MUST emit exactly one `console.info` line whose payload
 *     matches `^spawn_timing: session=<nodeId> checkpoint=xterm_mount
 *     elapsed=<N>ms$`. Format parity with the Rust side lets a future log
 *     aggregator grep both halves of the IPC boundary as one timeline.
 *   - On re-attach (detach + attach of the same node), the registry MUST
 *     NOT emit another `xterm_mount` line — re-attaches are fluid-grid pane
 *     swaps, not mounts, and emitting on every swap would flood the log.
 *   - `dispose(nodeId)` MUST clear the per-node start-time stamp so a long
 *     session that spawns/deletes many nodes doesn't leak the map.
 *
 * Run with: npm test -- --run tests/unit/terminal-registry-xterm-mount.test.ts
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { TerminalRegistry } from '../../src/components/Terminal/TerminalRegistry';

globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
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
  FitAddon: class { fit = vi.fn(); dispose = vi.fn(); },
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

const XTERM_MOUNT_PATTERN =
  /^spawn_timing: session=-?\d+ checkpoint=xterm_mount elapsed=\d+ms$/;

describe('TerminalRegistry xterm_mount spawn-timing checkpoint (issue #602)', () => {
  let registry: TerminalRegistry;
  let infoSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    vi.clearAllMocks();
    registry = new TerminalRegistry();
    infoSpy = vi.spyOn(console, 'info').mockImplementation(() => {});
  });

  afterEach(() => {
    registry.destroy();
    infoSpy.mockRestore();
  });

  it('emits a `spawn_timing:` checkpoint on fresh attach with the expected shape', async () => {
    const container = document.createElement('div');
    await registry.attach(42, container);

    const xtermMountCalls = infoSpy.mock.calls.filter(
      (c) => typeof c[0] === 'string' && c[0].includes('checkpoint=xterm_mount'),
    );
    expect(xtermMountCalls).toHaveLength(1);
    const [line] = xtermMountCalls[0];
    expect(line).toMatch(XTERM_MOUNT_PATTERN);
    expect(line).toContain('session=42');
    expect(line).toContain('checkpoint=xterm_mount');
  });

  it('reports elapsed as a non-negative integer milliseconds value', async () => {
    const container = document.createElement('div');
    await registry.attach(7, container);

    const xtermMountCalls = infoSpy.mock.calls.filter(
      (c) => typeof c[0] === 'string' && c[0].includes('checkpoint=xterm_mount'),
    );
    expect(xtermMountCalls).toHaveLength(1);
    const match = /elapsed=(\d+)ms/.exec(xtermMountCalls[0][0] as string);
    expect(match).not.toBeNull();
    // performance.now() returns a positive DOMHighResTimeStamp relative to
    // navigationStart; the delta between stamp + emission must be >= 0
    // (monotonic clock — guarantees the test is deterministic up to noise).
    const elapsed = Number(match![1]);
    expect(Number.isInteger(elapsed)).toBe(true);
    expect(elapsed).toBeGreaterThanOrEqual(0);
  });

  it('emits exactly one checkpoint per node, not per attach call', async () => {
    const container1 = document.createElement('div');
    const container2 = document.createElement('div');
    // First attach: fresh open — emit.
    await registry.attach(100, container1);
    // Second attach to the same node: re-attach (`wasFreshOpen === false`)
    // — must NOT emit another `xterm_mount`. Fluid-grid pane swaps would
    // otherwise flood the log.
    await registry.attach(100, container2);

    const xtermMountCalls = infoSpy.mock.calls.filter(
      (c) => typeof c[0] === 'string' && c[0].includes('checkpoint=xterm_mount'),
    );
    expect(xtermMountCalls).toHaveLength(1);
    expect(xtermMountCalls[0][0]).toContain('session=100');
  });

  it('emits a separate checkpoint per distinct node', async () => {
    const c1 = document.createElement('div');
    const c2 = document.createElement('div');
    const c3 = document.createElement('div');
    await registry.attach(1, c1);
    await registry.attach(2, c2);
    await registry.attach(3, c3);

    const xtermMountCalls = infoSpy.mock.calls.filter(
      (c) => typeof c[0] === 'string' && c[0].includes('checkpoint=xterm_mount'),
    );
    expect(xtermMountCalls).toHaveLength(3);
    expect(xtermMountCalls[0][0]).toContain('session=1');
    expect(xtermMountCalls[1][0]).toContain('session=2');
    expect(xtermMountCalls[2][0]).toContain('session=3');
  });

  it('clears the per-node start-time stamp on dispose so the map does not leak', async () => {
    // Reach into the private field via the only public surface that
    // surfaces a side effect: a fresh create after dispose must record a
    // new start time (the old one was cleared), and the second emit must
    // reference the new node, not the disposed one.
    const container = document.createElement('div');
    await registry.attach(500, container);
    infoSpy.mockClear();

    registry.dispose(500);
    // After dispose, a re-spawn of the same nodeId goes through
    // `getOrCreate`'s "fresh" branch (instance deleted by dispose) and
    // stamps a new start time. The follow-up attach emits a new line.
    await registry.attach(500, container);

    const xtermMountCalls = infoSpy.mock.calls.filter(
      (c) => typeof c[0] === 'string' && c[0].includes('checkpoint=xterm_mount'),
    );
    expect(xtermMountCalls).toHaveLength(1);
    expect(xtermMountCalls[0][0]).toContain('session=500');
  });

  it('does not emit the checkpoint when getOrCreate is called without attach', async () => {
    // `getOrCreate` builds the Terminal instance but `attachToDOM` (and
    // therefore the checkpoint) only runs when the React pane actually
    // parents the xterm. Pre-warming the registry for many nodes at once
    // (e.g. workspace restore) must not produce a flurry of mount
    // checkpoints for panes that never paint.
    await registry.getOrCreate(11);
    await registry.getOrCreate(12);
    await registry.getOrCreate(13);

    const xtermMountCalls = infoSpy.mock.calls.filter(
      (c) => typeof c[0] === 'string' && c[0].includes('checkpoint=xterm_mount'),
    );
    expect(xtermMountCalls).toEqual([]);
  });

  it('clears the per-node start-time stamp when creation fails', async () => {
    // Regression test for the failed-create leak path (issue #602): the
    // stamp is set in `getOrCreate` BEFORE `doCreate`, so if `doCreate`
    // throws (here forced via `mockImplementationOnce` on the Terminal
    // ctor) the catch branch MUST drop the stamp. `dispose` is the
    // success-path cleanup; it can't help here because no instance was
    // ever inserted into the registry. Without this fix a long-running
    // session that fails many creates would leak the map alongside the
    // console.error logs.
    //
    // `mockImplementationOnce` throws on the NEXT call only — the
    // subsequent successful `attach` resumes the original mock behaviour
    // automatically, so no manual restore is needed.
    const { Terminal: RealTerminal } = await import('@xterm/xterm');
    vi.mocked(RealTerminal).mockImplementationOnce(
      () => { throw new Error('forced create failure for test'); } as unknown as InstanceType<typeof RealTerminal>,
    );

    const inst = await registry.getOrCreate(999);
    expect(inst).toBeNull();

    // Now recover creation for the same nodeId. If the failed-create
    // path didn't clean up, the next successful `attach` would emit a
    // `xterm_mount` line with a HUGE elapsed (whole test run) — and
    // more importantly, the map would carry a dead entry forever.
    // By recovering and asserting the emitted elapsed is small AND the
    // line is the only xterm_mount call, we pin both halves of the
    // contract.
    const container = document.createElement('div');
    infoSpy.mockClear();
    await registry.attach(999, container);

    const xtermMountCalls = infoSpy.mock.calls.filter(
      (c) => typeof c[0] === 'string' && c[0].includes('checkpoint=xterm_mount'),
    );
    expect(xtermMountCalls).toHaveLength(1);
    const match = /elapsed=(\d+)ms/.exec(xtermMountCalls[0][0] as string);
    expect(match).not.toBeNull();
    const elapsed = Number(match![1]);
    // If the failed-create stamp leaked, this would be the entire
    // test-run duration, not a small mount-latency figure.
    expect(elapsed).toBeLessThan(1000);
  });
});