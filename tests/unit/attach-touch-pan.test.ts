/**
 * Regression: mobile terminal swipe-to-scroll.
 *
 * xterm.js v6 replaced its native overflow-scroll viewport with VS Code's
 * SmoothScrollableElement. The old approach of mutating `.xterm-viewport`'s
 * `scrollTop` became a no-op, so `attachTouchPan` MUST drive scrolling
 * through `term.scrollLines()` — the public API that works regardless of
 * which renderer/scroller xterm uses internally.
 *
 * Run: npm test -- --run tests/unit/attach-touch-pan.test.ts
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { attachTouchPan } from "../../src/mobile/screens/attachTouchPan";

type FakeTerm = { scrollLines: ReturnType<typeof vi.fn> };

function buildHost(rowHeightPx = 16): { host: HTMLElement; xtermEl: HTMLElement } {
  const host = document.createElement("div");
  const xtermEl = document.createElement("div");
  xtermEl.className = "xterm";

  const rows = document.createElement("div");
  rows.className = "xterm-rows";
  const row = document.createElement("div");
  // jsdom returns 0 from getBoundingClientRect; stub it on this element.
  row.getBoundingClientRect = () =>
    ({ height: rowHeightPx, width: 0, top: 0, left: 0, right: 0, bottom: 0, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;
  rows.appendChild(row);

  xtermEl.appendChild(rows);
  host.appendChild(xtermEl);
  document.body.appendChild(host);
  return { host, xtermEl };
}

function touchEvent(type: string, clientYs: number[]): TouchEvent {
  const touches = clientYs.map(
    (clientY, i) =>
      ({
        identifier: i,
        clientX: 0,
        clientY,
        pageX: 0,
        pageY: clientY,
        screenX: 0,
        screenY: clientY,
        target: document.body,
        radiusX: 0,
        radiusY: 0,
        rotationAngle: 0,
        force: 1,
      }) as unknown as Touch,
  );
  const e = new Event(type, { bubbles: true, cancelable: true }) as TouchEvent;
  Object.defineProperty(e, "touches", { value: touches });
  Object.defineProperty(e, "changedTouches", { value: touches });
  Object.defineProperty(e, "targetTouches", { value: touches });
  return e;
}

describe("attachTouchPan", () => {
  let term: FakeTerm;

  beforeEach(() => {
    document.body.innerHTML = "";
    term = { scrollLines: vi.fn() };
  });

  it("converts an upward swipe into a positive scrollLines call (toward newest output)", () => {
    const { host, xtermEl } = buildHost(16);
    attachTouchPan(host, term as unknown as Parameters<typeof attachTouchPan>[1]);

    // Finger lands at y=200, drags up to y=152 → 48px upward swipe → 3 rows at 16px each.
    xtermEl.dispatchEvent(touchEvent("touchstart", [200]));
    xtermEl.dispatchEvent(touchEvent("touchmove", [152]));

    expect(term.scrollLines).toHaveBeenCalledTimes(1);
    expect(term.scrollLines).toHaveBeenCalledWith(3);
  });

  it("a downward swipe scrolls back into history", () => {
    const { host, xtermEl } = buildHost(16);
    attachTouchPan(host, term as unknown as Parameters<typeof attachTouchPan>[1]);

    // Finger at y=100, drags down to y=164 → 64px downward → -4 rows.
    xtermEl.dispatchEvent(touchEvent("touchstart", [100]));
    xtermEl.dispatchEvent(touchEvent("touchmove", [164]));

    expect(term.scrollLines).toHaveBeenCalledWith(-4);
  });

  it("ignores multi-touch (e.g. pinch) so it doesn't fight the OS", () => {
    const { host, xtermEl } = buildHost(16);
    attachTouchPan(host, term as unknown as Parameters<typeof attachTouchPan>[1]);

    xtermEl.dispatchEvent(touchEvent("touchstart", [200, 300]));
    xtermEl.dispatchEvent(touchEvent("touchmove", [100, 200]));

    expect(term.scrollLines).not.toHaveBeenCalled();
  });

  it("accumulates sub-row pixel deltas instead of dropping them", () => {
    const { host, xtermEl } = buildHost(16);
    attachTouchPan(host, term as unknown as Parameters<typeof attachTouchPan>[1]);

    xtermEl.dispatchEvent(touchEvent("touchstart", [200]));
    // Two 10px swipes — each is < 1 row (16px), but together cross the threshold.
    xtermEl.dispatchEvent(touchEvent("touchmove", [190]));
    expect(term.scrollLines).not.toHaveBeenCalled();
    xtermEl.dispatchEvent(touchEvent("touchmove", [180]));
    expect(term.scrollLines).toHaveBeenCalledWith(1);
  });

  it("preventDefault is called on touchmove so the WebView doesn't pull-to-refresh", () => {
    const { host, xtermEl } = buildHost(16);
    attachTouchPan(host, term as unknown as Parameters<typeof attachTouchPan>[1]);

    xtermEl.dispatchEvent(touchEvent("touchstart", [200]));
    const move = touchEvent("touchmove", [180]);
    xtermEl.dispatchEvent(move);
    expect(move.defaultPrevented).toBe(true);
  });

  it("detach removes the listeners so further touches are inert", () => {
    const { host, xtermEl } = buildHost(16);
    const detach = attachTouchPan(host, term as unknown as Parameters<typeof attachTouchPan>[1]);

    detach();
    xtermEl.dispatchEvent(touchEvent("touchstart", [200]));
    xtermEl.dispatchEvent(touchEvent("touchmove", [100]));
    expect(term.scrollLines).not.toHaveBeenCalled();
  });

  it("returns a no-op when the host has no .xterm child (defensive against race with term.open)", () => {
    const host = document.createElement("div");
    document.body.appendChild(host);
    expect(() => attachTouchPan(host, term as unknown as Parameters<typeof attachTouchPan>[1])()).not.toThrow();
  });
});
