/**
 * Quick-key strip contract: each button must emit the exact xterm escape
 * sequence a hardware keyboard would produce, since these go straight down
 * the PTY WebSocket. A wrong byte here silently breaks agent CLI menus
 * (arrows/Enter) or mode toggles (Shift+Tab in Claude Code).
 */
import { describe, it, expect } from "vitest";
import { QUICK_KEYS } from "../../src/mobile/screens/quickKeys";

const byId = Object.fromEntries(QUICK_KEYS.map((k) => [k.id, k.seq]));

describe("QUICK_KEYS", () => {
  it("covers the keys soft keyboards are missing", () => {
    expect(byId["esc"]).toBe("\x1b");
    expect(byId["tab"]).toBe("\t");
    expect(byId["shift-tab"]).toBe("\x1b[Z");
    expect(byId["up"]).toBe("\x1b[A");
    expect(byId["down"]).toBe("\x1b[B");
    expect(byId["left"]).toBe("\x1b[D");
    expect(byId["right"]).toBe("\x1b[C");
    expect(byId["enter"]).toBe("\r");
    expect(byId["ctrl-c"]).toBe("\x03");
  });

  it("has unique ids and labels", () => {
    expect(new Set(QUICK_KEYS.map((k) => k.id)).size).toBe(QUICK_KEYS.length);
    expect(new Set(QUICK_KEYS.map((k) => k.label)).size).toBe(QUICK_KEYS.length);
  });
});
