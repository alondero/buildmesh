/**
 * TerminalScreen resumes on foreground/network return (issue #806).
 *
 * On mobile, backgrounding suspends timers and drops the WebSocket, so a
 * terminal can silently exhaust its retries while hidden. The fix adds
 * `visibilitychange`/`online` handlers that reset the backoff and reconnect
 * a dead socket — but leave a healthy one alone.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, waitFor } from "@testing-library/react";
import { act } from "react";
import TerminalScreen from "../../src/mobile/screens/TerminalScreen";
import type { AgentNode } from "../../src/mobile/api";

let sockets: FakeWebSocket[] = [];

class FakeWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  readyState = FakeWebSocket.CONNECTING;
  binaryType = "";
  onopen: (() => void) | null = null;
  onmessage: ((e: unknown) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor(public url: string) {
    sockets.push(this);
  }
  send() {}
  close() {
    this.readyState = FakeWebSocket.CLOSED;
  }

  // Test drivers for the socket lifecycle.
  simulateOpen() {
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.();
  }
  simulateDrop() {
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.();
  }
}

// Keep isAuthError real; stub only the ticket mint so connect() resolves a URL.
vi.mock("../../src/mobile/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../src/mobile/api")>();
  return {
    ...actual,
    terminalWsUrl: vi.fn().mockResolvedValue("ws://test/ws/terminal/1?ticket=x"),
  };
});

import { terminalWsUrl } from "../../src/mobile/api";

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

describe("TerminalScreen reconnect on foreground/online", () => {
  beforeEach(() => {
    sockets = [];
    vi.stubGlobal("WebSocket", FakeWebSocket);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  async function mountAndConnect() {
    const utils = render(
      <TerminalScreen node={node} onBack={noop} onAuthFailed={noop} />,
    );
    await waitFor(() => expect(sockets.length).toBe(1));
    act(() => sockets[0].simulateOpen());
    return utils;
  }

  it("reconnects a dropped socket when the document is foregrounded", async () => {
    const utils = await mountAndConnect();
    expect(terminalWsUrl).toHaveBeenCalledTimes(1);

    // Socket dies (as it would while backgrounded).
    act(() => sockets[0].simulateDrop());

    // Foregrounding resets backoff and reconnects immediately.
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await waitFor(() => expect(sockets.length).toBe(2));
    expect(terminalWsUrl).toHaveBeenCalledTimes(2);

    utils.unmount();
  });

  it("reconnects a dropped socket when the network returns (online event)", async () => {
    const utils = await mountAndConnect();
    act(() => sockets[0].simulateDrop());

    act(() => {
      window.dispatchEvent(new Event("online"));
    });
    await waitFor(() => expect(sockets.length).toBe(2));

    utils.unmount();
  });

  it("leaves a healthy open socket alone on foreground (no redundant reconnect)", async () => {
    const utils = await mountAndConnect();
    expect(terminalWsUrl).toHaveBeenCalledTimes(1);

    // Still OPEN — a tab switch back must not tear down the live connection.
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    // Give any stray async reconnect a chance to fire before asserting.
    await Promise.resolve();
    expect(sockets.length).toBe(1);
    expect(terminalWsUrl).toHaveBeenCalledTimes(1);

    utils.unmount();
  });
});
