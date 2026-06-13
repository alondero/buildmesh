/**
 * Regression test for issue #303.
 *
 * BuildRunTerminal used to write every `build-run-output-<nodeId>` event
 * straight through `term.write`, which during a verbose build (e.g.
 * `cargo build -v`) turned each compiler line into a separate xterm render
 * pass and pushed Buildmesh's CPU into the red. The agent path had already
 * solved this through a `TerminalWriter` whose `requestAnimationFrame`
 * scheduler coalesces same-frame chunks into one write — BuildRunTerminal
 * just never picked it up (PR #260 landed the listener without the
 * buffering hop).
 *
 * The fix is to route the listener through a TerminalWriter the same way
 * `TerminalRegistry` does for the agent terminal. This test asserts the
 * wire-up at the component boundary: N synchronous events in flight ⇒ at
 * most ONE term.write per animation frame.
 *
 * We don't reach into `TerminalWriter` itself (its own unit tests already
 * cover register/unregister/coalesce). We drive the component, stub
 * `requestAnimationFrame` so we own the flush moment, and assert on the
 * `write` mock of the locally-mocked xterm Terminal.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, cleanup, waitFor } from '@testing-library/react';
import { emit } from '@tauri-apps/api/event';
import { BuildRunTerminal } from '../../src/components/Terminal/BuildRunTerminal';

// The global vitest setup mocks @xterm/xterm with an inline class, which
// gives every `new Terminal()` its own `write` mock but hides the instance.
// Re-mock here so the test can grab the instance and spy on `write` —
// file-level vi.mock takes precedence over the setup-level one.
const terminalInstances: Array<{ write: ReturnType<typeof vi.fn> }> = [];
vi.mock('@xterm/xterm', () => {
  class MockTerminal {
    write = vi.fn();
    onData = vi.fn();
    onResize = vi.fn();
    open = vi.fn();
    dispose = vi.fn();
    loadAddon = vi.fn();
    refresh = vi.fn();
    scrollToBottom = vi.fn();
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
    rows = 24;
    cols = 80;
    element: HTMLElement | null = null;
    constructor(_opts?: unknown) {
      terminalInstances.push(this as unknown as { write: ReturnType<typeof vi.fn> });
    }
  }
  return { Terminal: MockTerminal };
});

// jsdom doesn't ship ResizeObserver and the component instantiates one
// unconditionally inside the effect; this no-op keeps render() from throwing.
globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

describe('BuildRunTerminal RAF batching (issue #303)', () => {
  let rafQueue: Array<() => void>;

  beforeEach(() => {
    terminalInstances.length = 0;
    rafQueue = [];
    // Capture every requestAnimationFrame callback so the test owns the
    // flush moment. The TerminalWriter's default scheduler is
    // `(cb) => requestAnimationFrame(cb)` (wrapped to dodge the WebView2
    // "Illegal invocation" trap — see terminal-writer.test.ts), so stubbing
    // the global here is what reaches the writer that BuildRunTerminal owns
    // privately.
    vi.stubGlobal('requestAnimationFrame', (cb: () => void) => {
      rafQueue.push(cb);
      return 0;
    });
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it('coalesces a flood of build-run-output events into one term.write per frame', async () => {
    render(<BuildRunTerminal sessionId={7} />);

    // The component's `listen(...).then(...)` first registers the handler,
    // then writes an opening banner. Wait until the banner appears so we
    // know the listener is live before we emit. Without this barrier, the
    // emitted events would no-op against an empty mockListeners map.
    await waitFor(() => {
      expect(terminalInstances).toHaveLength(1);
      expect(terminalInstances[0].write).toHaveBeenCalled();
    });

    const term = terminalInstances[0];
    // Discard banner writes and any incidental resize-observer RAFs so the
    // assertions below see only storm-induced activity.
    term.write.mockClear();
    rafQueue.length = 0;

    // 100 lines is the shape of `cargo build -v` mid-flight — one event per
    // compiler line. Each event arrives synchronously through the mock
    // emitter, so without batching this would be 100 writes and 0 RAFs;
    // with batching it must be 0 writes and exactly 1 RAF.
    for (let i = 0; i < 100; i++) {
      await emit('build-run-output-7', `line ${i}\n`);
    }

    expect(rafQueue).toHaveLength(1);
    expect(term.write).not.toHaveBeenCalled();

    // Run the single scheduled frame; the writer should flush a coalesced
    // string carrying every line in order.
    rafQueue.shift()!();

    expect(term.write).toHaveBeenCalledTimes(1);
    const written = term.write.mock.calls[0][0];
    expect(typeof written).toBe('string');
    expect(written).toContain('line 0\n');
    expect(written).toContain('line 99\n');
    // Order preservation: the merged string should start with line 0 and
    // end with line 99\n — guards against a future "Set/dedupe" coalescer.
    expect(written.indexOf('line 0\n')).toBeLessThan(written.indexOf('line 99\n'));
  });

  it('coalesces base64 byte payloads without UTF-8 corruption across chunk boundaries', async () => {
    render(<BuildRunTerminal sessionId={8} />);
    await waitFor(() => {
      expect(terminalInstances).toHaveLength(1);
      expect(terminalInstances[0].write).toHaveBeenCalled();
    });

    const term = terminalInstances[0];
    term.write.mockClear();
    rafQueue.length = 0;

    // Split a single UTF-8 codepoint across two events: ▀ U+2580 = 0xE2 0x96 0x80.
    // Asserts that the writer's `coalesceChunks` merges adjacent byte chunks
    // into one Uint8Array (preserving byte order). This is the writer-level
    // invariant that makes split-codepoint reassembly work downstream —
    // xterm-side UTF-8 decoding isn't exercised here because xterm is mocked
    // (a real xterm would happily reassemble the merged bytes; the unit of
    // work this PR owns is "don't drop or reorder bytes between RAFs").
    const b64 = (bytes: number[]) =>
      globalThis.btoa(String.fromCharCode(...bytes));

    await emit('build-run-output-8', { data: b64([0xe2, 0x96]) });
    await emit('build-run-output-8', { data: b64([0x80]) });

    expect(rafQueue).toHaveLength(1);
    expect(term.write).not.toHaveBeenCalled();

    rafQueue.shift()!();

    expect(term.write).toHaveBeenCalledTimes(1);
    const written = term.write.mock.calls[0][0];
    expect(written).toBeInstanceOf(Uint8Array);
    expect(Array.from(written as Uint8Array)).toEqual([0xe2, 0x96, 0x80]);
  });
});
