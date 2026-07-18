import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "../..");

// Issue #152 — the panic hook at src-tauri/src/lib.rs:351-382 calls
// std::backtrace::Backtrace::capture(), which is gated by RUST_BACKTRACE.
// When the env var is unset, capture() returns the "disabled backtrace"
// placeholder and the panic.log entry is diagnostic-useless. The launcher
// is the gap — these scripts set RUST_BACKTRACE=1 BEFORE the Start-Process
// / fork so the binary inherits it. This test pins that contract on all
// four launchers so a future refactor can't silently drop the env var.
interface LauncherSpec {
  path: string;
  /** Substring that appears in the actual launch line (stable across refactors). */
  launchMarker: string;
  /** Regex (multiline-friendly) matching a line that sets RUST_BACKTRACE to 1. */
  setRe: RegExp;
  /** Human description used in the test name. */
  language: string;
}

const LAUNCHERS: LauncherSpec[] = [
  {
    path: "scripts/run.ps1",
    launchMarker: "Start-Process $Binary",
    setRe: /\$env:RUST_BACKTRACE\s*=\s*['"]1['"]/,
    language: "PowerShell (stable)",
  },
  {
    path: "scripts/run-dev.ps1",
    launchMarker: "Start-Process $Binary",
    setRe: /\$env:RUST_BACKTRACE\s*=\s*['"]1['"]/,
    language: "PowerShell (dev profile)",
  },
  {
    path: "scripts/run.sh",
    launchMarker: '"$BINARY" &',
    setRe: /^\s*export\s+RUST_BACKTRACE=1\b/m,
    language: "bash (stable)",
  },
  {
    path: "scripts/run-dev.sh",
    launchMarker: '"$BINARY" &',
    setRe: /^\s*export\s+RUST_BACKTRACE=1\b/m,
    language: "bash (dev profile)",
  },
];

describe("launch scripts enable RUST_BACKTRACE for panic backtraces (issue #152)", () => {
  for (const { path, launchMarker, setRe, language } of LAUNCHERS) {
    it(`${language} (${path}) sets RUST_BACKTRACE=1 before the launch line`, () => {
      const content = readFileSync(resolve(REPO_ROOT, path), "utf8");
      const launchIdx = content.indexOf(launchMarker);
      expect(
        launchIdx,
        `${path} should contain launch marker "${launchMarker}" — did the launch line get refactored?`,
      ).toBeGreaterThan(-1);

      const setIdx = content.search(setRe);
      expect(
        setIdx,
        `${path} should set RUST_BACKTRACE to '1' (PowerShell) or 1 (bash). ` +
          `Without it, Backtrace::capture() in the panic hook returns the ` +
          `disabled placeholder and panic.log loses diagnostic value.`,
      ).toBeGreaterThan(-1);

      expect(
        setIdx,
        `${path} should set RUST_BACKTRACE BEFORE the launch line ` +
          `(setIdx=${setIdx}, launchIdx=${launchIdx}). ` +
          `If the env var is set after the fork, the binary never inherits it.`,
      ).toBeLessThan(launchIdx);
    });
  }
});
