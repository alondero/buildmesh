import { describe, it, expect } from "vitest";
import { readFileSync, writeFileSync, mkdtempSync, rmSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { tmpdir } from "node:os";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "../..");

// Issue #2043 - run-dev.ps1 printed its `OK - ...` line and still left
// powershell.exe reporting a non-zero exit, so harnesses read a healthy dev
// launch as a failure. Two distinct causes, pinned here:
//
//   1. In-script. `$ErrorActionPreference = "Stop"` is script-wide, so ANY
//      error record aborts the script and powershell.exe returns 1 - including
//      errors from reads the launch verdict does not depend on (a log file the
//      app is still writing, a panic log momentarily locked) and from the
//      Stop-Process/Get-Process race on a re-run. The launch must degrade to
//      the next evidence source instead of aborting.
//   2. Host-level. A consumer that stops reading stdout early
//      (`... 2>&1 | Select-Object -First N`, `| head`) closes the pipe under
//      powershell.exe, which then returns non-zero even after `exit 0`. Not
//      preventable from inside the script, so the contract is documented and
//      the `OK - ` line is the authoritative success signal.
//
// The static half pins the structure that prevents (1); the behavioural half
// actually executes the shipped Get-LogLines helper to prove a locked log file
// degrades to "no evidence" instead of aborting the launcher.

interface LauncherSpec {
  path: string;
  okLine: RegExp;
  /** Marker proving the script states the exit-code contract for its readers. */
  contractMarkers: string[];
}

const LAUNCHERS: LauncherSpec[] = [
  {
    path: "scripts/run-dev.ps1",
    okLine: /^\s*Write-Output "OK - /m,
    contractMarkers: ["EXIT CODE (issue #2043)", "Select-Object -First"],
  },
  {
    path: "scripts/run.ps1",
    okLine: /^\s*Write-Output "OK - /m,
    contractMarkers: ["issue #2043", "scripts/run-dev.ps1"],
  },
];

const read = (path: string) => readFileSync(resolve(REPO_ROOT, path), "utf8");

describe("launcher scripts report the launch verdict through their exit code (issue #2043)", () => {
  for (const { path } of LAUNCHERS) {
    describe(path, () => {
      it("reads log files only through the fault-tolerant Get-LogLines helper", () => {
        const content = read(path);
        // A bare `Get-Content` under `$ErrorActionPreference = "Stop"` throws
        // on a sharing violation and aborts the script. Every read the verdict
        // depends on must go through the helper, which catches and degrades.
        const helperStart = content.indexOf("function Get-LogLines {");
        expect(
          helperStart,
          `${path} should define Get-LogLines so a failed log read degrades to "no evidence" ` +
            "instead of aborting a launch that is already up (issue #2043).",
        ).toBeGreaterThan(-1);

        const helperEnd = content.indexOf("\n}\n", helperStart);
        expect(helperEnd, `${path}: could not find the end of Get-LogLines`).toBeGreaterThan(helperStart);

        const outside = content
          .slice(0, helperStart)
          .concat(content.slice(helperEnd + 3))
          .split("\n")
          .filter((line) => /\bGet-Content\b/.test(line));

        expect(
          outside,
          `${path} reads a log with a bare Get-Content outside Get-LogLines. Under ` +
            '$ErrorActionPreference = "Stop" that aborts the script and turns a healthy ' +
            "launch into exit 1.",
        ).toEqual([]);
      });

      it("guards the launched-process query so a reaped process cannot abort the verdict", () => {
        const content = read(path);
        expect(content).toMatch(/try\s*\{[^}]*\$proc\.HasExited[^}]*\}\s*catch\s*\{/);
      });

      it("tolerates the Stop-Process race when replacing an existing instance", () => {
        const content = read(path);
        const stop = content.split("\n").filter((line) => /\|\s*Stop-Process\b/.test(line));
        expect(stop.length, `${path} should stop the existing instance`).toBeGreaterThan(0);
        for (const line of stop) {
          expect(
            line,
            `${path}: Stop-Process needs -ErrorAction SilentlyContinue. A previous instance can ` +
              'exit between Get-Process and Stop-Process, and under $ErrorActionPreference = "Stop" ' +
              "that race aborts the script before it ever launches (issue #2043).",
          ).toMatch(/-ErrorAction\s+SilentlyContinue/);
        }
      });

      it("prints an ERROR line before every exit 1, and an OK line on every success path", () => {
        const lines = read(path).split("\n");
        const exits = lines
          .map((line, i) => ({ line: line.trim(), i }))
          .filter(({ line }) => /^exit\s+1\b/.test(line));

        expect(exits.length, `${path} should have failure exits to check`).toBeGreaterThan(0);

        for (const { i } of exits) {
          // Walk back to the start of the enclosing block: the diagnostic is
          // what makes a real failure distinguishable from a transport glitch,
          // so a bare `exit 1` with nothing said is a regression.
          const window = lines.slice(Math.max(0, i - 8), i).join("\n");
          expect(
            window,
            `${path}:${i + 1} exits 1 without an ERROR diagnostic above it. Callers cannot tell a ` +
              "real launch failure from an incidental abort (issue #2043).",
          ).toMatch(/Write-Output "ERROR:/);
        }

        expect(read(path)).toMatch(LAUNCHERS.find((l) => l.path === path)!.okLine);
      });

      it("documents the exit-code contract and the early-closing-consumer caveat", () => {
        const content = read(path);
        const spec = LAUNCHERS.find((l) => l.path === path)!;
        for (const marker of spec.contractMarkers) {
          expect(
            content,
            `${path} should document the exit-code contract (missing "${marker}"). Callers that ` +
              "only read the exit code abort a healthy launch (issue #2043).",
          ).toContain(marker);
        }
      });
    });
  }
});

// Behavioural: run the helper that actually ships in the launchers. Windows-only
// because it locks a file with an exclusive share mode and executes
// powershell.exe - both are platform-specific capabilities of the launcher, not
// something this test would be asserting on another OS.
//
// These spawn powershell.exe, so they carry an explicit timeout well above
// vitest's 5s default; on a loaded machine the default flaked here.
const POWERSHELL_TIMEOUT_MS = 60_000;

describe.skipIf(process.platform !== "win32")("Get-LogLines degrades instead of aborting (issue #2043)", () => {
  const extractHelper = (path: string) => {
    const content = read(path);
    const start = content.indexOf("function Get-LogLines {");
    const end = content.indexOf("\n}\n", start);
    return content.slice(start, end + 3);
  };

  // `$body` receives the scratch directory so each run owns its fixture files -
  // a shared name in %TEMP% would let concurrent runs fight over the lock.
  const runProbe = (helperSource: string, body: (dir: string) => string[]) => {
    const dir = mkdtempSync(resolve(tmpdir(), "bm-exitcode-"));
    try {
      writeFileSync(resolve(dir, "helper.ps1"), helperSource, "utf8");
      writeFileSync(resolve(dir, "probe.ps1"), `${body(dir).join("\n")}\n`, "utf8");
      return execFileSync(
        "powershell.exe",
        ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", resolve(dir, "probe.ps1")],
        { encoding: "utf8", timeout: POWERSHELL_TIMEOUT_MS },
      ).trim();
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  };

  for (const { path } of LAUNCHERS) {
    it(
      `${path}: returns the file's lines when it can be read`,
      () => {
        const out = runProbe(extractHelper(path), (dir) => [
          ". $PSScriptRoot\\helper.ps1",
          `$p = Join-Path '${dir}' 'readable.log'`,
          "Set-Content -LiteralPath $p -Value @('alpha','beta')",
          "$lines = @(Get-LogLines $p)",
          `"COUNT=" + $lines.Count`,
          `"FIRST=" + $lines[0]`,
        ]);
        expect(out).toContain("COUNT=2");
        expect(out).toContain("FIRST=alpha");
      },
      POWERSHELL_TIMEOUT_MS,
    );

    it(
      `${path}: returns no evidence instead of throwing when the log is exclusively locked`,
      () => {
        const out = runProbe(extractHelper(path), (dir) => [
          ". $PSScriptRoot\\helper.ps1",
          `$p = Join-Path '${dir}' 'locked.log'`,
          "Set-Content -LiteralPath $p -Value @('alpha')",
          // FileShare.None reproduces the app still holding buildmesh.log open
          // for writing, which is what made the launcher abort with exit 1.
          "$lock = [System.IO.File]::Open($p, 'Open', 'ReadWrite', 'None')",
          "try { $lines = @(Get-LogLines $p) } finally { $lock.Dispose() }",
          `"COUNT=" + $lines.Count`,
        ]);
        // The regression: a terminating throw here would escape Get-LogLines
        // and abort the launcher, turning a healthy launch into exit 1.
        expect(out).toContain("COUNT=0");
      },
      POWERSHELL_TIMEOUT_MS,
    );
  }
});