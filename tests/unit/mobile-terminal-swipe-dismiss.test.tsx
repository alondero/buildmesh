/**
 * TerminalScreen: swipe-down on the app bar dismisses back to the list
 * (issue #1377). The gesture is anchored to the app bar only — the xterm
 * surface belongs to pan-scrolling (see attachTouchPan) — and only fires
 * for mostly-vertical strokes past the 72px threshold, so scrollback
 * drags and diagonal chrome taps never close the screen.
 *
 * Post-review (#1377):
 *   * `onTouchCancel` resets the swipe anchor (the previous build left
 *     `swipeStartRef` stuck if an OS gesture interrupted)
 *   * real-time visual feedback during the drag — the app bar wrapper's
 *     `--appbar-translate` / `--appbar-opacity` CSS variables are
 *     mirrored directly from the handler, with no React re-render
 *   * horizontal axis lock: if a horizontal stroke takes over mid-drag,
 *     the anchor drops so a later vertical stroke from the same touch
 *     isn't mis-attributed
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

  // Drag with an explicit move sequence so the handler can drive the
  // CSS-variable visual feedback. The existing `swipe()` helper only fires
  // touchstart + touchend, which is enough to test the dismiss decision
  // but not the visual state.
  const drag = (
    from: { x: number; y: number },
    moves: { x: number; y: number }[],
    to: { x: number; y: number },
  ) => {
    const bar = screen.getByTestId("terminal-appbar");
    fireEvent.touchStart(bar, { touches: [{ clientX: from.x, clientY: from.y }] });
    for (const m of moves) {
      fireEvent.touchMove(bar, { touches: [{ clientX: m.x, clientY: m.y }] });
    }
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

  it("translates the app bar wrapper during a downward drag (visual feedback)", async () => {
    // The previous build had zero visual feedback during the drag — the
    // screen would just abruptly unmount on release. Post-review fix
    // mirrors the dy into `--appbar-translate` so the wrapper slides down
    // as the user drags, and resets to 0 if the gesture is cancelled.
    const onBack = vi.fn();
    const utils = await mount(onBack);
    const bar = screen.getByTestId("terminal-appbar");

    drag(
      { x: 100, y: 40 },
      [{ x: 100, y: 80 }, { x: 100, y: 120 }, { x: 100, y: 160 }],
      { x: 100, y: 180 },
    );

    // Pull mid-drag: the bar wrapper carries the latest dy as its
    // CSS-variable translate. The exact value isn't important — just
    // that it tracked the gesture.
    expect(bar.style.getPropertyValue("--appbar-translate")).not.toBe("0px");
    // Past the threshold + dismiss fires.
    expect(onBack).toHaveBeenCalledTimes(1);
    utils.unmount();
  });

  it("resets the translate when the gesture is cancelled below threshold", async () => {
    // Drag 50px down (below the 72px dismiss threshold) and release — the
    // CSS variable must snap back to 0px so the app bar doesn't stay
    // shifted when the user changed their mind.
    const onBack = vi.fn();
    const utils = await mount(onBack);
    const bar = screen.getByTestId("terminal-appbar");

    drag(
      { x: 100, y: 40 },
      [{ x: 100, y: 60 }, { x: 100, y: 80 }],
      { x: 100, y: 80 },
    );

    expect(bar.style.getPropertyValue("--appbar-translate")).toBe("0px");
    expect(bar.style.getPropertyValue("--appbar-opacity")).toBe("1");
    expect(onBack).not.toHaveBeenCalled();
    utils.unmount();
  });

  it("onTouchCancel resets the swipe anchor and visual state (issue #1377)", async () => {
    // The previous build left `swipeStartRef.current` stuck if an OS
    // gesture interrupted (system swipe-back, notification banner drag).
    // Post-review fix routes `touchcancel` to the same `resetSwipeVisual`
    // path so a subsequent touch starts from a clean slate.
    const onBack = vi.fn();
    const utils = await mount(onBack);
    const bar = screen.getByTestId("terminal-appbar");

    fireEvent.touchStart(bar, { touches: [{ clientX: 100, clientY: 40 }] });
    fireEvent.touchMove(bar, { touches: [{ clientX: 100, clientY: 120 }] });
    expect(bar.style.getPropertyValue("--appbar-translate")).not.toBe("0px");
    fireEvent.touchCancel(bar);
    expect(bar.style.getPropertyValue("--appbar-translate")).toBe("0px");
    expect(bar.style.getPropertyValue("--appbar-opacity")).toBe("1");
    expect(onBack).not.toHaveBeenCalled();
    utils.unmount();
  });
});
