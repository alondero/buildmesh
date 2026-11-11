/**
 * Mobile terminal xterm write backlog (issue #2765).
 *
 * The socket handed each WebSocket frame to `term.write`. xterm parses
 * asynchronously and does not cap that queue, so a PTY that outruns the parser
 * retains one unparsed payload per frame. `TerminalWriter` is the cap the
 * desktop already uses: at most `MAX_INFLIGHT_WRITES` bulk payloads sit in
 * xterm, and the rest stay in a queue bounded by bytes and chunk count.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, waitFor } from "@testing-library/react";
import { act } from "react";
import TerminalScreen, { mobileTerminalWriter } from "../../src/mobile/screens/TerminalScreen";
import type { AgentNode } from "../../src/mobile/api";
import {
  MAX_INFLIGHT_WRITES,
  MAX_PENDING_BYTES,
  MAX_PENDING_CHUNKS,
} from "../../src/components/Terminal/TerminalWriter";

const FRAME_BYTES = 32 * 1024;

const { terminalInstances, pendingParses, scrollDisposables, rafQueue, parserHold } = vi.hoisted(() => ({
  terminalInstances: [] as Array<{
    write: ReturnType<typeof vi.fn>;
    dispose: ReturnType<typeof vi.fn>;
  }>,
  pendingParses: [] as Array<() => void>,
  scrollDisposables: [] as Array<{ dispose: ReturnType<typeof vi.fn> }>,
  rafQueue: [] as Array<() => void>,
  parserHold: { current: false },
}));

vi.mock("@xterm/xterm", () => {
  const mockProviders: Record<string, {
    version: string;
    wcwidth: (cp: number) => number;
    charProperties: (cp: number, preceding: number) => number;
  }> = {
    "11": { version: "11", wcwidth: () => 1, charProperties: () => 0 },
  };
  let mockActive = "11";
  const internalService = {
    _providers: mockProviders,
    register: vi.fn((provider: {
      version: string;
      wcwidth: (cp: number) => number;
      charProperties: (cp: number, preceding: number) => number;
    }) => {
      mockProviders[provider.version] = provider;
    }),
  };

  class MockTerminal {
    write = vi.fn((_data?: unknown, callback?: () => void) => {
      if (!callback) return;
      if (parserHold.current) pendingParses.push(callback);
      else callback();
    });
    onData = vi.fn();
    onResize = vi.fn();
    open = vi.fn();
    loadAddon = vi.fn();
    dispose = vi.fn();
    focus = vi.fn();
    scrollToBottom = vi.fn();
    onScroll = vi.fn(() => {
      const disposable = { dispose: vi.fn() };
      scrollDisposables.push(disposable);
      return disposable;
    });
    buffer = { active: { viewportY: 0, baseY: 0 } };
    unicode = {
      register: vi.fn((provider: {
        version: string;
        wcwidth: (cp: number) => number;
        charProperties: (cp: number, preceding: number) => number;
      }) => internalService.register(provider)),
      get activeVersion() {
        return mockActive;
      },
      set activeVersion(version: string) {
        if (!mockProviders[version]) throw new Error(`unknown Unicode version "${version}"`);
        mockActive = version;
      },
    };
    _core = { unicodeService: internalService };
    rows = 24;
    cols = 80;
    element: HTMLElement | null = null;
    options: Record<string, unknown> = {};

    constructor(options?: Record<string, unknown>) {
      if (options) this.options = options;
      terminalInstances.push(this);
    }
  }

  return { Terminal: MockTerminal };
});

let sockets: FakeWebSocket[] = [];

class FakeWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  readyState = FakeWebSocket.CONNECTING;
  binaryType = "";
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor(public url: string) {
    sockets.push(this);
  }
  send() {}
  close() {
    this.readyState = FakeWebSocket.CLOSED;
  }
}

const node: AgentNode = {
  id: 1,
  mesh_id: 1,
  name: "node-1",
  path: "/tmp/wt",
  branch: null,
  provider: "anthropic",
  status: "running",
  cli_session_id: null,
  created_at: "2026-06-11T00:00:00Z",
};

const noop = () => {};

function flushRaf() {
  const queued = rafQueue.splice(0, rafQueue.length);
  for (const callback of queued) callback();
}

function frame(marker: number): ArrayBuffer {
  const bytes = new Uint8Array(FRAME_BYTES);
  bytes.fill(marker & 0xff);
  return bytes.buffer;
}

describe("TerminalScreen xterm write backlog (issue #2765)", () => {
  beforeEach(() => {
    sockets = [];
    terminalInstances.length = 0;
    pendingParses.length = 0;
    scrollDisposables.length = 0;
    rafQueue.length = 0;
    parserHold.current = false;
    mobileTerminalWriter.unregister(node.id);
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("requestAnimationFrame", (callback: () => void) => {
      rafQueue.push(callback);
      return 0;
    });
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: true,
        status: 200,
        json: async () => ({ ticket: "x" }),
      }),
    );
  });

  afterEach(() => {
    mobileTerminalWriter.unregister(node.id);
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  async function mount() {
    const utils = render(<TerminalScreen node={node} onBack={noop} />);
    await waitFor(() => expect(sockets).toHaveLength(1));
    expect(terminalInstances).toHaveLength(1);
    return utils;
  }

  it("keeps queued chunks and in-flight parses inside the writer caps while the parser is held", async () => {
    parserHold.current = true;
    await mount();
    const term = terminalInstances[0];
    // One more window than the byte cap, after the payloads already sitting in
    // xterm, so a writer that forgets to evict is over the cap.
    const frames = MAX_INFLIGHT_WRITES + MAX_PENDING_BYTES / FRAME_BYTES + 8;
    expect(frames * FRAME_BYTES).toBeGreaterThan(MAX_PENDING_BYTES);

    for (let i = 0; i < frames; i++) {
      act(() => {
        sockets[0].onmessage?.({ data: frame(i) });
      });
      act(() => flushRaf());
    }

    expect(term.write).toHaveBeenCalledTimes(MAX_INFLIGHT_WRITES);
    expect(mobileTerminalWriter.inFlightWrites(node.id)).toBe(MAX_INFLIGHT_WRITES);
    expect(mobileTerminalWriter.inFlightWrites(node.id)).toBeLessThanOrEqual(MAX_INFLIGHT_WRITES);
    expect(mobileTerminalWriter.queuedChunks(node.id)).toBeGreaterThan(0);
    expect(mobileTerminalWriter.queuedChunks(node.id)).toBeLessThanOrEqual(MAX_PENDING_CHUNKS);
    expect(mobileTerminalWriter.pendingBytes(node.id)).toBeLessThanOrEqual(MAX_PENDING_BYTES);
    expect(mobileTerminalWriter.pendingBytes(node.id)).toBe(MAX_PENDING_BYTES);

    const heldBytes = mobileTerminalWriter.pendingBytes(node.id);
    act(() => {
      pendingParses.shift()?.();
    });
    act(() => flushRaf());

    expect(term.write).toHaveBeenCalledTimes(MAX_INFLIGHT_WRITES + 1);
    const drained = term.write.mock.calls.at(-1)?.[0] as Uint8Array;
    expect(drained).toBeInstanceOf(Uint8Array);
    expect(drained.byteLength).toBe(heldBytes);
    // The oldest queued frame was evicted; the surviving window starts later
    // and ends on the newest frame.
    const queuedFrames = MAX_PENDING_BYTES / FRAME_BYTES;
    expect(drained[0]).toBe((frames - queuedFrames) & 0xff);
    expect(drained[drained.byteLength - 1]).toBe((frames - 1) & 0xff);
  });

  it("still writes the text snapshot, a binary frame, and a blob frame", async () => {
    await mount();
    const term = terminalInstances[0];

    act(() => {
      sockets[0].onmessage?.({ data: "ok" });
    });
    expect(term.write).toHaveBeenCalledTimes(1);
    // A keystroke-sized frame skips the queue and does not take an in-flight slot.
    expect(term.write.mock.calls[0][0]).toBe("ok");
    expect(term.write.mock.calls[0][1]).toBeUndefined();

    const snapshot = "snapshot of the terminal\r\n";
    act(() => {
      sockets[0].onmessage?.({ data: snapshot });
    });
    expect(term.write).toHaveBeenCalledTimes(1);
    act(() => flushRaf());
    expect(term.write).toHaveBeenCalledTimes(2);
    expect(term.write).toHaveBeenLastCalledWith(snapshot, expect.any(Function));

    const binary = new Uint8Array(32);
    binary.set([1, 2, 3, 4]);
    act(() => {
      sockets[0].onmessage?.({ data: binary.buffer });
    });
    act(() => flushRaf());
    const writtenBinary = term.write.mock.calls.at(-1)?.[0] as Uint8Array;
    expect(Array.from(writtenBinary)).toEqual(Array.from(binary));

    const blobBytes = Uint8Array.from([9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 1, 2, 3, 4, 5, 6, 7]);
    act(() => {
      sockets[0].onmessage?.({ data: new Blob([blobBytes]) });
    });
    await waitFor(() => {
      act(() => flushRaf());
      const match = term.write.mock.calls.some((call) => {
        const data = call[0];
        return data instanceof Uint8Array && data.length === blobBytes.length && data[0] === 9;
      });
      expect(match).toBe(true);
    });
  });

  it("releases the scroll listener and the writer on unmount, and still disposes the per-mount terminal", async () => {
    // The screen creates a new xterm per visit. There is no TerminalManager
    // to blank, so unmount still disposes that visit's terminal. The writer
    // and the scroll listener are released on their own: a blob that resolves
    // after unmount must not write into the disposed terminal.
    const utils = await mount();
    const term = terminalInstances[0];
    expect(mobileTerminalWriter.has(node.id)).toBe(true);
    expect(scrollDisposables).toHaveLength(1);

    const payload = new Uint8Array(32);
    payload.fill(7);
    const blob = new Blob([payload]);
    let release: (value: ArrayBuffer) => void = () => {};
    const gated = new Promise<ArrayBuffer>((resolve) => {
      release = resolve;
    });
    vi.spyOn(blob, "arrayBuffer").mockReturnValue(gated);

    const writesBefore = term.write.mock.calls.length;
    act(() => {
      sockets[0].onmessage?.({ data: blob });
    });
    utils.unmount();

    await act(async () => {
      release(payload.buffer.slice(0));
      await Promise.resolve();
    });
    act(() => flushRaf());

    expect(term.write.mock.calls.length).toBe(writesBefore);
    expect(scrollDisposables[0].dispose).toHaveBeenCalledTimes(1);
    expect(mobileTerminalWriter.has(node.id)).toBe(false);
    expect(term.dispose).toHaveBeenCalledTimes(1);
  });
});
