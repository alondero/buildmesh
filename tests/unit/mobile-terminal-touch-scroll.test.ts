/**
 * Regression: each mobile terminal surface must scroll by touch.
 *
 * xterm.js v6's scroller only answers the mouse wheel, so a phone reaches
 * older output only through a touch → `term.scrollLines()` bridge. Two mobile
 * clients own a terminal surface — the browser mobile client and the native
 * Android client's WebView bundle — and they must share one implementation.
 *
 * The native client shipped without that bridge at all: its terminal could
 * not be scrolled by touch, so a phone user could only ever read the last
 * screen of output, and less of it once the soft keyboard shrank the pane.
 * A second, private copy of the gesture would let that drift back, so this
 * test pins both clients to the one shared owner.
 *
 * Run: npm test -- --run tests/unit/mobile-terminal-touch-scroll.test.ts
 */
import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { join, relative, resolve } from "node:path";

const root = resolve(__dirname, "../..");
const read = (path: string) => readFileSync(join(root, path), "utf8");

const SHARED_MODULE = "src/components/Terminal/attachTouchPan.ts";
const ANDROID_TERMINAL = "android/terminal/terminal.js";
const MOBILE_SCREEN = "src/mobile/screens/TerminalScreen.tsx";

function sourcesUnder(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...sourcesUnder(full));
    else if (/\.(ts|tsx|js|css)$/.test(entry.name)) out.push(full);
  }
  return out;
}

const repoRelative = (path: string) => relative(root, path).replace(/\\/g, "/");

/**
 * The lines of `source` that carry `needle` as code.
 *
 * A commented-out call would satisfy a plain `toContain`, so a guard that
 * watches for the wiring has to ignore comments — otherwise reverting the call
 * to a comment would still pass. A prose mention of the same word is ignored
 * for the same reason: the guard is about code, not about wording.
 */
function liveLines(source: string, needle: string): string[] {
  const out: string[] = [];
  let inBlock = false;
  for (const raw of source.split("\n")) {
    // Strip block comments, carrying the state across lines; `//` comments are
    // removed with the line.
    let line = raw;
    for (;;) {
      if (inBlock) {
        const end = line.indexOf("*/");
        if (end === -1) {
          line = "";
          break;
        }
        line = line.slice(end + 2);
        inBlock = false;
      }
      const start = line.indexOf("/*");
      if (start === -1) break;
      const end = line.indexOf("*/", start + 2);
      if (end === -1) {
        line = line.slice(0, start);
        inBlock = true;
        break;
      }
      line = line.slice(0, start) + line.slice(end + 2);
    }
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("//")) continue;
    if (line.includes(needle)) out.push(raw);
  }
  return out;
}

describe("mobile terminal touch scrolling", () => {
  it("has one owner for the touch → scrollLines mapping", () => {
    const owners = [...sourcesUnder(join(root, "src")), ...sourcesUnder(join(root, "android/terminal"))]
      .filter(path => liveLines(readFileSync(path, "utf8"), "scrollLines(").length > 0)
      .map(repoRelative);
    expect(owners).toEqual([SHARED_MODULE]);
  });

  it("wires the shared touch-pan into the native Android client's terminal bundle", () => {
    const source = read(ANDROID_TERMINAL);
    expect(liveLines(source, "src/components/Terminal/attachTouchPan")).toHaveLength(1);
    expect(liveLines(source, "attachTouchPan(")).toHaveLength(1);
    // The bundle must not grow its own gesture — that is how the native
    // client lost touch scrolling in the first place.
    expect(liveLines(source, "touchmove")).toHaveLength(0);
  });

  it("wires the shared touch-pan into the browser mobile client's terminal screen", () => {
    const source = read(MOBILE_SCREEN);
    expect(liveLines(source, "components/Terminal/attachTouchPan")).toHaveLength(1);
    expect(liveLines(source, "attachTouchPan(")).toHaveLength(1);
    // The app bar keeps its own swipe-to-dismiss gesture, but the scroll
    // bridge itself stays in the shared module.
    expect(liveLines(source, "scrollLines")).toHaveLength(0);
  });

  it("gives the native client a way back to the live tail after a pan", () => {
    const source = read(ANDROID_TERMINAL);
    expect(liveLines(source, "buildmeshScrollToBottom")).toHaveLength(1);
    expect(liveLines(source, "scrollState")).toHaveLength(1);
  });
});
