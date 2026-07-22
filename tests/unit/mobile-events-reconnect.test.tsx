/**
 * useWsEvents resumes on foreground/network return (issue #806 sister fix).
 *
 * The events WebSocket drives the "attention needed" red-dot indicator on
 * the NodeList. On mobile, backgrounding suspends timers and drops the WS,
 * so a phone left backgrounded for a few seconds stalls the live event
 * stream — the same root cause as the terminal WS fix #818 closed. The
 * events hook now resumes on `visibilitychange` / `online` instead of
 * stranding the user on the 5-second poll fallback.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { useWsEvents } from "../../src/mobile/useWsEvents";

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
    if (this.readyState !== FakeWebSocket.CLOSED) {
      this.readyState = FakeWebSocket.CLOSED;
    }
  }
  simulateOpen() {
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.();
  }
  simulateError() {
    this.onerror?.();
  }
  simulateDrop() {
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.();
  }
}

// `eventsWsUrl` is the only seam useWsEvents reads; stub the mint so
// `connect()` resolves a URL instead of hitting the network.
vi.mock("../../src/mobile/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../src/mobile/api")>();
  return {
    ...actual,
    eventsWsUrl: vi.fn().mockResolvedValue("ws://test/ws/events?ticket=x"),
  };
});

import { eventsWsUrl } from "../../src/mobile/api";

const noop = () => {};

describe("useWsEvents reconnect on foreground/online", () => {
  beforeEach(() => {
    sockets = [];
    vi.stubGlobal("WebSocket", FakeWebSocket);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  async function mountAndConnect() {
    const utils = renderHook(() => useWsEvents(noop, noop));
    await waitFor(() => expect(sockets.length).toBe(1));
    act(() => sockets[0].simulateOpen());
    return utils;
  }

  it("opens a single events socket on mount", async () => {
    const utils = await mountAndConnect();
    expect(eventsWsUrl).toHaveBeenCalledTimes(1);
    expect(sockets.length).toBe(1);
    utils.unmount();
  });

  it("reconnects a dropped socket when the document is foregrounded", async () => {
    const utils = await mountAndConnect();
    act(() => sockets[0].simulateDrop());

    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await waitFor(() => expect(sockets.length).toBe(2));
    expect(eventsWsUrl).toHaveBeenCalledTimes(2);
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

  it("leaves a healthy open socket alone on foreground", async () => {
    const utils = await mountAndConnect();
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await Promise.resolve();
    expect(sockets.length).toBe(1);
    expect(eventsWsUrl).toHaveBeenCalledTimes(1);
    utils.unmount();
  });

  // Same dedup contract as the terminal WS: a real browser fires BOTH
  // `error` and `close` for one failed connection. Without dedup the
  // `attempt` counter ticks twice per failure and two reconnect timers
  // race against each other (1s + 2s for the first failure). We wait
  // 2.1s — past the 1s timer (dedup case AND first-half of no-dedup
  // case both reach eventsWsUrl=2 by then) AND past the 2s timer that
  // ONLY the no-dedup path schedules. With dedup, eventsWsUrl stays at 2;
  // without dedup, it climbs to 3.
  it("advances the attempt counter once when onerror and onclose both fire for one failure", async () => {
    const utils = await mountAndConnect();
    // 1 connect so far (mount).
    expect(eventsWsUrl).toHaveBeenCalledTimes(1);

    // Fire both events on socket 0 — both reach scheduleReconnect
    // unless the dedup flag in connect() filters the second one.
    act(() => {
      sockets[0].simulateError();
      sockets[0].simulateDrop();
    });

    // Wait long enough that BOTH a 1s timer (would fire either way)
    // AND a 2s timer (only schedules in the no-dedup path) have run.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 2100));
    });

    // Dedup: only the 1s timer fired → eventsWsUrl = 2, one socket opened.
    // No dedup: the 2s leftover timer also fired → eventsWsUrl = 3,
    // a leaked parallel socket exists. The asserts below fail in that case.
    expect(eventsWsUrl).toHaveBeenCalledTimes(2);
    expect(sockets.length).toBe(2);

    utils.unmount();
  });

  it("removes the foreground/online listeners on unmount", async () => {
    const utils = await mountAndConnect();
    const initialCalls = (eventsWsUrl as ReturnType<typeof vi.fn>).mock
      .calls.length;

    utils.unmount();

    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    act(() => {
      window.dispatchEvent(new Event("online"));
    });
    await Promise.resolve();
    expect(eventsWsUrl).toHaveBeenCalledTimes(initialCalls);
  });
});
