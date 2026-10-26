import type { Terminal as XTerm } from "@xterm/xterm";

// xterm.js v6 replaced its native overflow-scroll viewport with VS Code's
// SmoothScrollableElement, so `.xterm-viewport.scrollTop` is not the scroll
// axis — that element is a positional shell now and any scrollTop mutation
// gets re-synced to zero by the synthetic scrollbar. Use the public
// `term.scrollLines()` API instead, converting touch pixel deltas into whole
// rows once we cross a row threshold.
//
// This module is the touch-scroll contract shared by the mobile terminal
// surfaces: the browser mobile client (`src/mobile/`) and the native Android
// client's WebView bundle (`android/terminal/`) both drive scrolling through
// it, so the two stay in step — `tests/unit/mobile-terminal-touch-scroll.test.ts`
// pins that. xterm's own scroller answers the mouse wheel only, which a phone
// does not have, so without this bridge older output stays out of reach.
export function attachTouchPan(host: HTMLElement, term: XTerm): () => void {
  const xtermEl = host.querySelector<HTMLElement>(".xterm");
  if (!xtermEl) return () => {};

  let lastY = 0;
  let panning = false;
  let pendingPx = 0;
  let rowHeight = 0;

  const measureRowHeight = () => {
    const row = host.querySelector<HTMLElement>(".xterm-rows > *");
    const h = row?.getBoundingClientRect().height ?? 0;
    if (h > 0) rowHeight = h;
  };

  const onTouchStart = (e: TouchEvent) => {
    if (e.touches.length !== 1) {
      panning = false;
      return;
    }
    panning = true;
    lastY = e.touches[0].clientY;
    pendingPx = 0;
    measureRowHeight();
  };
  const onTouchMove = (e: TouchEvent) => {
    if (!panning || e.touches.length !== 1) return;
    const y = e.touches[0].clientY;
    // Finger drag follows the page-style convention: swipe up (lastY > y →
    // delta positive) scrolls toward newer output at the bottom, matching
    // xterm's scrollLines(+n) which moves the viewport downward.
    pendingPx += lastY - y;
    lastY = y;
    if (rowHeight > 0) {
      const lines = Math.trunc(pendingPx / rowHeight);
      if (lines !== 0) {
        term.scrollLines(lines);
        pendingPx -= lines * rowHeight;
      }
    }
    e.preventDefault();
  };
  const onTouchEnd = () => {
    panning = false;
    pendingPx = 0;
  };

  xtermEl.addEventListener("touchstart", onTouchStart, { passive: true });
  xtermEl.addEventListener("touchmove", onTouchMove, { passive: false });
  xtermEl.addEventListener("touchend", onTouchEnd, { passive: true });
  xtermEl.addEventListener("touchcancel", onTouchEnd, { passive: true });

  return () => {
    xtermEl.removeEventListener("touchstart", onTouchStart);
    xtermEl.removeEventListener("touchmove", onTouchMove);
    xtermEl.removeEventListener("touchend", onTouchEnd);
    xtermEl.removeEventListener("touchcancel", onTouchEnd);
  };
}
