/**
 * Android WebView terminal write backlog (issue #2765).
 *
 * Kotlin pushes each PTY frame into the page as base64 and the page used to
 * call `terminal.write` immediately. That is the same uncapped xterm queue as
 * the browser client. This bundle now feeds the shared `TerminalWriter`.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act } from "react";
import {
  MAX_INFLIGHT_WRITES,
  MAX_PENDING_BYTES,
  MAX_PENDING_CHUNKS,
} from "../../src/components/Terminal/TerminalWriter";

const FRAME_BYTES = 32 * 1024;

const { terminalInstances, pendingParses, rafQueue } = vi.hoisted(() => ({
  terminalInstances: [] as Array<{
    write: ReturnType<typeof vi.fn>;
    modes: { applicationCursorKeysMode: boolean; bracketedPasteMode: boolean };
  }>,
  pendingParses: [] as Array<() => void>,
  rafQueue: [] as Array<() => void>,
}));

vi.mock("@xterm/xterm", () => {
  class MockTerminal {
    write = vi.fn((_data?: unknown, callback?: () => void) => {
      if (callback) pendingParses.push(callback);
    });
    onResize = vi.fn();
    onScroll = vi.fn();
    open = vi.fn();
    loadAddon = vi.fn();
    scrollToBottom = vi.fn();
    buffer = { active: { viewportY: 0, baseY: 0 } };
    modes = { applicationCursorKeysMode: false, bracketedPasteMode: false };
    rows = 24;
    cols = 80;
    element: HTMLElement | null = null;
    textarea: HTMLTextAreaElement | null = null;
    options: Record<string, unknown> = {};

    constructor() {
      terminalInstances.push(this);
    }
  }

  return { Terminal: MockTerminal };
});

type AndroidTerminal = typeof import("../../android/terminal/terminal.js");

function flushRaf() {
  const queued = rafQueue.splice(0, rafQueue.length);
  for (const callback of queued) callback();
}

function frame(marker: number): string {
  const bytes = new Uint8Array(FRAME_BYTES);
  bytes.fill(marker & 0xff);
  return Buffer.from(bytes).toString("base64");
}

describe("Android terminal xterm write backlog (issue #2765)", () => {
  let page: AndroidTerminal;
  const bridge = {
    ready: vi.fn(),
    modes: vi.fn(),
    scrollState: vi.fn(),
    resize: vi.fn(),
    openUrl: vi.fn(),
    focusInput: vi.fn(),
  };

  beforeEach(async () => {
    terminalInstances.length = 0;
    pendingParses.length = 0;
    rafQueue.length = 0;
    document.body.innerHTML = '<div id="terminal"></div>';
    vi.stubGlobal("BuildmeshTerminal", bridge);
    vi.stubGlobal("requestAnimationFrame", (callback: () => void) => {
      rafQueue.push(callback);
      return 0;
    });
    vi.stubGlobal("ResizeObserver", class {
      observe() {}
      unobserve() {}
      disconnect() {}
    });
    vi.resetModules();
    page = await import("../../android/terminal/terminal.js");
  });

  afterEach(() => {
    page?.androidTerminalWriter.unregister(page.ANDROID_TERMINAL_WRITER_ID);
    document.body.innerHTML = "";
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  it("keeps queued chunks and in-flight parses inside the writer caps while the parser is held", () => {
    const term = terminalInstances[0];
    const writer = page.androidTerminalWriter;
    const id = page.ANDROID_TERMINAL_WRITER_ID;
    const frames = MAX_INFLIGHT_WRITES + MAX_PENDING_BYTES / FRAME_BYTES + 8;
    expect(frames * FRAME_BYTES).toBeGreaterThan(MAX_PENDING_BYTES);

    for (let i = 0; i < frames; i++) {
      act(() => {
        window.buildmeshOutput(frame(i));
      });
      act(() => flushRaf());
    }

    expect(term.write).toHaveBeenCalledTimes(MAX_INFLIGHT_WRITES);
    expect(writer.inFlightWrites(id)).toBe(MAX_INFLIGHT_WRITES);
    expect(writer.queuedChunks(id)).toBeGreaterThan(0);
    expect(writer.queuedChunks(id)).toBeLessThanOrEqual(MAX_PENDING_CHUNKS);
    expect(writer.pendingBytes(id)).toBe(MAX_PENDING_BYTES);

    const heldBytes = writer.pendingBytes(id);
    act(() => {
      pendingParses.shift()?.();
    });
    act(() => flushRaf());

    expect(term.write).toHaveBeenCalledTimes(MAX_INFLIGHT_WRITES + 1);
    const drained = term.write.mock.calls.at(-1)?.[0] as Uint8Array;
    expect(drained.byteLength).toBe(heldBytes);
    const queuedFrames = MAX_PENDING_BYTES / FRAME_BYTES;
    expect(drained[0]).toBe((frames - queuedFrames) & 0xff);
    expect(drained[drained.byteLength - 1]).toBe((frames - 1) & 0xff);
  });

  it("still writes a decoded frame and reports modes when that parse finishes", () => {
    const term = terminalInstances[0];
    const bytes = Uint8Array.from([27, 91, 63, 49, 104, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76]);
    const callsBefore = bridge.modes.mock.calls.length;
    act(() => {
      window.buildmeshOutput(Buffer.from(bytes).toString("base64"));
    });
    act(() => flushRaf());

    const written = term.write.mock.calls.at(-1)?.[0] as Uint8Array;
    expect(Array.from(written)).toEqual(Array.from(bytes));
    expect(bridge.modes.mock.calls.length).toBe(callsBefore);

    term.modes.applicationCursorKeysMode = true;
    act(() => {
      pendingParses.shift()?.();
    });
    expect(bridge.modes).toHaveBeenCalledWith(true, false);
  });
});