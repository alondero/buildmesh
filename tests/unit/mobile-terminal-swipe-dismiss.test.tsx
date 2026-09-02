/**
 * TerminalScreen: swipe-down on the app bar dismisses back to the list
 * (issue #1377). The gesture is anchored to the app bar only — the xterm
 * surface belongs to pan-scrolling (see attachTouchPan) — and only fires
 * for mostly-vertical strokes past the 72px threshold, so scrollback
 * drags and diagonal chrome taps never close the screen.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import TerminalScreen from "../../src/mobile/screens/TerminalScreen";
import type { AgentNode } from "../../src/mobile/api";

let sockets: FakeWebSocket[] = [];

class FakeWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;

  readyState = FakeWebSocket.CONNECTING;
  onopen: (() => void) | null = null;
  onmessage: ((e: unknown) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(public url: string) {
    sockets.push(this);
  }
  send() {}
  close() {
    this.readyState = FakeWebSocket.OPEN;
  }
  simulateOpen() {
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.();
  }
}

const node: AgentNode = {
  id: 1,
  mesh_id: 1,
  name: "node-1",
  path: "/tmp/wt",
  branch: "main",
  provider: "anthropic",
  status: "running",
  cli_session_id: null,
  created_at: "2026-06-11T00:00:00Z",
};

describe("TerminalScreen swipe-down dismiss (issue #1377)", () => {
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

  async function mount(onBack: () => void) {
    const utils = render(<TerminalScreen node={node} onBack={onBack} />);
    await waitFor(() => expect(screen.getByTestId("terminal-screen")));
    await waitFor(() => expect(sockets.length).toBe(1));
    sockets[0].simulateOpen();
    return utils;
  }

  const swipe = (
    from: { x: number; y: number },
    to: { x: number; y: number },
  ) => {
    const bar = screen.getByTestId("terminal-appbar");
    fireEvent.touchStart(bar, { touches: [{ clientX: from.x, clientY: from.y }] });
    fireEvent.touchEnd(bar, {
      changedTouches: [{ clientX: to.x, clientY: to.y }],
    });
  };

  it("dismisses on a mostly-vertical downward drag past the threshold", async () => {
    const onBack = vi.fn();
    const utils = await mount(onBack);

    swipe({ x: 100, y: 40 }, { x: 108, y: 160 }); // dy 120, dx 8

    expect(onBack).toHaveBeenCalledTimes(1);
    utils.unmount();
  });

  it("ignores short drags and horizontal swipes", async () => {
    const onBack = vi.fn();
    const utils = await mount(onBack);

    swipe({ x: 100, y: 40 }, { x: 102, y: 90 }); // dy 50 — below threshold
    swipe({ x: 20, y: 40 }, { x: 160, y: 70 }); // horizontal — dy must dominate
    swipe({ x: 100, y: 160 }, { x: 100, y: 60 }); // upward — never dismisses

    expect(onBack).not.toHaveBeenCalled();
    utils.unmount();
  });
});
