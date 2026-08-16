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
  /// Standalone `onerror` — does NOT flip readyState, mirroring how a real
  /// browser fires an error event for an opening or live socket before the
  /// accompanying close. The dedup tests use this to confirm the reconnect
  /// counter only ticks once when both events arrive for one failure.
  simulateError() {
    this.onerror?.();
  }
  simulateDrop() {
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.();
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

describe("TerminalScreen reconnect on foreground/online", () => {
  beforeEach(() => {
    sockets = [];
    vi.stubGlobal("WebSocket", FakeWebSocket);
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
    expect((fetch as ReturnType<typeof vi.fn>).mock.calls).toHaveLength(1);

    // Socket dies (as it would while backgrounded).
    act(() => sockets[0].simulateDrop());

    // Foregrounding resets backoff and reconnects immediately.
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await waitFor(() => expect(sockets.length).toBe(2));
    expect((fetch as ReturnType<typeof vi.fn>).mock.calls).toHaveLength(2);

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
    expect((fetch as ReturnType<typeof vi.fn>).mock.calls).toHaveLength(1);

    // Still OPEN — a tab switch back must not tear down the live connection.
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    // Give any stray async reconnect a chance to fire before asserting.
    await Promise.resolve();
    expect(sockets.length).toBe(1);
    expect((fetch as ReturnType<typeof vi.fn>).mock.calls).toHaveLength(1);

    utils.unmount();
  });

  // Regression coverage: a real browser fires BOTH `error` AND `close` for a
  // single socket failure (in that order). The current code subscribes to
  // both independently, so without dedupe the counter ticks twice per
  // failure and the budget is exhausted 2x as fast as the user expects —
  // which is exactly how a phone left backgrounded for a minute could
  // land on the "Connection lost." sentinel.
  it("ticks the counter once when onerror and onclose both fire for one failure", async () => {
    const utils = await mountAndConnect();

    // Error first (typical browser ordering), then close.
    act(() => sockets[0].simulateError());
    act(() => sockets[0].simulateDrop());

    // The overlay should show the 1st-attempt delay (1s), not the 2nd
    // (2s) — proves only one scheduleReconnect fired.
    await waitFor(() => {
      const overlay = document.querySelector(
        '[data-testid="reconnect-overlay"]',
      );
      expect(overlay).toBeTruthy();
      expect(overlay!.textContent).toContain("reconnecting in 1s");
    });

    utils.unmount();
  });

  it("survives multiple back-to-back background→foreground cycles", async () => {
    const utils = await mountAndConnect();
    expect(sockets.length).toBe(1);

    // Three cycles, each: drop the current socket, then foreground.
    // The listener must keep firing fresh reconnects, not bleed across
    // cycles or get stuck after the first round-trip.
    for (let i = 0; i < 3; i++) {
      act(() => {
        sockets[sockets.length - 1].simulateDrop();
        document.dispatchEvent(new Event("visibilitychange"));
      });
      await waitFor(() => expect(sockets.length).toBe(i + 2));
    }
    // Three new sockets; the original is sockets[0].
    expect(sockets.length).toBe(4);

    utils.unmount();
  });

  it("resets the backoff from the 'gave up' state on foreground", async () => {
    // Mount with real timers, then switch to fake so we can drive the
    // 1/2/4/8/16s backoff forward without losing 31s of wall-clock.
    const utils = await mountAndConnect();
    vi.useFakeTimers();
    try {
      // Drive 5 reconnect cycles (no intermediate opens), so the counter
      // ticks 0 → 5. Each drop is on the still-CONNECTING socket that the
      // previous timer fired. act() flushes the scheduleReconnect state
      // update synchronously, so no waitFor is needed for assertions.
      const delays = [1000, 2000, 4000, 8000, 16000];
      for (const ms of delays) {
        await act(async () => {
          sockets[sockets.length - 1].simulateDrop();
          await vi.advanceTimersByTimeAsync(ms);
        });
      }
      expect(sockets.length).toBe(6);

      // One more drop pushes the counter to 5 → scheduleReconnect's >= check
      // fires and we land on the sentinel "gave up" UI state.
      act(() => sockets[sockets.length - 1].simulateDrop());
      const overlay = document.querySelector(
        '[data-testid="reconnect-overlay"]',
      );
      expect(overlay).toBeTruthy();
      expect(overlay!.textContent).toContain("Connection lost.");

      // Foreground must reset + reconnect immediately.
      const before = sockets.length;
      await act(async () => {
        document.dispatchEvent(new Event("visibilitychange"));
      });
      expect(sockets.length).toBe(before + 1);
    } finally {
      vi.useRealTimers();
    }
    utils.unmount();
  });

  it("removes the foreground/online listeners on unmount", async () => {
    const utils = await mountAndConnect();
    const initialCalls = (fetch as ReturnType<typeof vi.fn>).mock.calls.length;

    utils.unmount();

    // Listeners removed by the cleanup — neither event should drive a
    // reconnect on an unmounted screen.
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    act(() => {
      window.dispatchEvent(new Event("online"));
    });
    // Allow any stray microtask to flush.
    await Promise.resolve();
    expect(fetch).toHaveBeenCalledTimes(initialCalls);
  });

  it.each(["close", "error"] as const)(
    "recovers auth when an unopened ticket socket fires %s",
    async (event) => {
      const fetch = vi
        .fn()
        .mockResolvedValueOnce({
          ok: true,
          status: 200,
          json: async () => ({ ticket: "initial" }),
        })
        .mockResolvedValueOnce({
          ok: false,
          status: 401,
          json: async () => ({ error: "expired" }),
        });
      vi.stubGlobal("fetch", fetch);
      const onAuthFailed = vi.fn();
      const utils = render(
        <TerminalScreen node={node} onBack={noop} onAuthFailed={onAuthFailed} />,
      );
      await waitFor(() => expect(sockets.length).toBe(1));

      act(() => {
        if (event === "close") sockets[0].simulateDrop();
        else sockets[0].simulateError();
      });

      await act(async () => {
        await Promise.resolve();
      });
      expect(onAuthFailed).toHaveBeenCalledTimes(1);
      expect(document.querySelector('[data-testid="reconnect-overlay"]')).toBeNull();
      utils.unmount();
    },
  );

  it("keeps the reconnect error state for a network failure", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("network")));
    const onAuthFailed = vi.fn();
    const utils = render(
      <TerminalScreen node={node} onBack={noop} onAuthFailed={onAuthFailed} />,
    );

    await waitFor(() => {
      expect(document.querySelector('[data-testid="reconnect-overlay"]')).toBeTruthy();
    });
    expect(onAuthFailed).not.toHaveBeenCalled();
    utils.unmount();
  });

  it.each([404, 500])(
    "keeps the reconnect error state for a non-auth %s ticket response",
    async (status) => {
      vi.stubGlobal(
        "fetch",
        vi.fn().mockResolvedValue({
          ok: false,
          status,
          json: async () => ({ error: `ticket ${status}` }),
        }),
      );
      const onAuthFailed = vi.fn();
      const utils = render(
        <TerminalScreen node={node} onBack={noop} onAuthFailed={onAuthFailed} />,
      );

      await waitFor(() => {
        expect(document.querySelector('[data-testid="reconnect-overlay"]')).toBeTruthy();
      });
      expect(onAuthFailed).not.toHaveBeenCalled();
      utils.unmount();
    },
  );
});
