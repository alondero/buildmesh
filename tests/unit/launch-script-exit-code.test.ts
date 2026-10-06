import { describe, it, expect } from "vitest";
import { readFileSync, writeFileSync, mkdtempSync, rmSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { tmpdir } from "node:os";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "../..");

// Issue #2043 - run-dev.ps1 printed its `OK - ...` line and still left
// powershell.exe reporting a non-zero exit, so a harness read a healthy dev
// launch as a failure.
//
// The launchers run under $ErrorActionPreference = "Stop", so any error record
// aborts the script and powershell.exe returns 1 - including errors from work
// the launch verdict does not depend on. The app holds buildmesh.log open while
// writing, so a bare Get-Content on it can fail with a sharing violation.
//
// Two failure directions have to stay fixed together, which is why the delta
// decision lives in one tested function (Compare-LogGrowth) rather than inline:
//   - unreadable baseline + readable current -> every line looks new, so a
//     naive count comparison false-panics a healthy launch (#2043);
//   - readable baseline + unreadable current -> 0 is never greater than N, so a
//     naive comparison reports "no panic" after the panic hook has already
//     written its entry, masking a real panic-only crash (#158).
//
// A separate, host-level failure is not preventable from inside a script: a
// consumer that stops reading stdout early (`... 2>&1 | Select-Object -First N`)
// closes the pipe under powershell.exe, which returns non-zero even after the
// script reached `exit 0`. The launchers and /use document the contract: the
// `OK - ` line is the authoritative success signal.

const COMMON = "scripts/launcher-common.ps1";
const WINDOWS_LAUNCHERS = ["scripts/run-dev.ps1", "scripts/run.ps1"];
const ALL_LAUNCHERS = [...WINDOWS_LAUNCHERS, "scripts/run-dev.sh", "scripts/run.sh"];

const read = (path: string) => readFileSync(resolve(REPO_ROOT, path), "utf8");

/** Drops `#` line comments and `<# #>` help blocks so assertions see only code. */
const stripComments = (src: string) =>
  src.replace(/<#[\s\S]*?#>/g, "").replace(/^\s*#.*$/gm, "");

// The behavioural suites shell out to PowerShell, which takes longer than
// vitest's 5s default once a loaded CI runner is involved.
const POWERSHELL_TIMEOUT_MS = 60_000;

/**
 * `pwsh` (PowerShell 7, preinstalled on GitHub's Linux runners) or Windows
 * PowerShell. Returns null when neither is on PATH, which is the only reason
 * the behavioural suites are ever skipped.
 */
function resolvePowerShell(): string | null {
  for (const exe of ["pwsh", "powershell.exe"]) {
    try {
      execFileSync(exe, ["-NoProfile", "-Command", "$PSVersionTable.PSVersion.Major"], {
        stdio: "pipe",
        timeout: POWERSHELL_TIMEOUT_MS,
      });
      return exe;
    } catch {
      // Not installed; try the next one.
    }
  }
  return null;
}

const PS = resolvePowerShell();

describe("launcher exit-code contract (issue #2043)", () => {
  describe("structure", () => {
    for (const path of WINDOWS_LAUNCHERS) {
      it(`${path} dot-sources the shared helper instead of inlining log reads`, () => {
        const content = read(path);
        expect(
          content,
          `${path} should dot-source ${COMMON} so the log-read semantics cannot drift ` +
            "between the two launchers.",
        ).toMatch(/\.\s*\(Join-Path\s+\$PSScriptRoot\s+["']launcher-common\.ps1["']\)/);

        // A bare Get-Content under $ErrorActionPreference = "Stop" throws on a
        // sharing violation and aborts the script.
        const bare = stripComments(content)
          .split(/\r?\n/)
          .filter((line) => /\bGet-Content\b/.test(line));
        expect(
          bare,
          `${path} still reads a log with a bare Get-Content. Every read the verdict depends on ` +
            "must go through Read-LogFile, which degrades instead of aborting.",
        ).toEqual([]);
      });

      it(`${path} reports a failure only with a diagnostic, and success only with an OK line`, () => {
        const lines = read(path).split(/\r?\n/);
        const exits = lines
          .map((line, i) => ({ line: line.trim(), i }))
          .filter(({ line }) => /^exit\s+1$/.test(line));
        expect(exits.length, `${path} should have failure exits to check`).toBeGreaterThan(0);

        for (const { i } of exits) {
          // Walk back to the start of the enclosing block (a column-0 `}` or the
          // top of the file) rather than a fixed line count: the diagnostic can
          // legitimately sit several lines above the exit.
          const block: string[] = [];
          for (let j = i - 1; j >= 0; j--) {
            if (lines[j] === "}") break;
            block.unshift(lines[j]);
          }
          expect(
            block.join("\n"),
            `${path}:${i + 1} exits 1 without an ERROR diagnostic in its enclosing block. ` +
              "Callers cannot tell a real launch failure from an incidental abort (#2043).",
          ).toMatch(/Write-(Output|Host)\s+"ERROR:/);
        }

        expect(read(path), `${path} should print an OK line callers can key on`).toMatch(
          /Write-Output "OK - /,
        );
      });
    }

    it("keeps the read warning on the warning stream, not the success stream", () => {
      // Write-Host bypasses 3>&1 and lands on the console; the launchers' stdout
      // is a contract (/verify parses the pre-launch counts out of it).
      expect(stripComments(read(COMMON)), `${COMMON} should warn with Write-Warning`).not.toMatch(
        /\bWrite-Host\b/,
      );
    });

    it("gives all four launchers the same OK prefix so callers match one protocol", () => {
      for (const path of ALL_LAUNCHERS) {
        const ok = read(path).match(/(?:Write-Output|echo)\s+"(OK[^"]*)"/g) ?? [];
        expect(ok.length, `${path} should print an OK line`).toBeGreaterThan(0);
        for (const line of ok) {
          expect(
            line,
            `${path} prints "${line}" - every launcher must use the same "OK - " prefix ` +
              "so a caller does not need a per-platform matcher.",
          ).toMatch(/"OK - /);
        }
      }
    });
  });

  describe("PowerShell availability", () => {
    // Without this, a runner missing pwsh would silently skip every behavioural
    // proof below and still report green - the gap that left the original
    // fix unverified on PR CI.
    it.runIf(process.env.CI === "true")(
      "is present, or the launcher helper proofs would silently skip",
      () => {
        expect(
          PS,
          "No PowerShell on PATH (tried pwsh, powershell.exe). GitHub's ubuntu runners ship " +
            "pwsh; without it the Read-LogFile / Compare-LogGrowth proofs do not run.",
        ).not.toBeNull();
      },
    );
  });

  describe.skipIf(PS === null)("Read-LogFile / Compare-LogGrowth behaviour", () => {
    /**
     * Dot-sources the REAL helper from the repo - no text extraction, so the
     * proofs cannot drift from the shipped file - and reports each property as a
     * `KEY=value` line for the assertions below.
     */
    const runProbe = (body: (dir: string) => string[]) => {
      const dir = mkdtempSync(resolve(tmpdir(), "bm-launcher-"));
      try {
        const probe = resolve(dir, "probe.ps1");
        const commonPath = resolve(REPO_ROOT, COMMON).replace(/'/g, "''");
        writeFileSync(
          probe,
          [
            "$ErrorActionPreference = 'Stop'",
            `. '${commonPath}'`,
            "function Say([string]$k, $v) { Write-Output ($k + '=' + $v) }",
            ...body(dir),
          ].join("\n") + "\n",
          "utf8",
        );
        return execFileSync(PS!, ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", probe], {
          encoding: "utf8",
          timeout: POWERSHELL_TIMEOUT_MS,
        });
      } finally {
        rmSync(dir, { recursive: true, force: true });
      }
    };

    const field = (out: string, key: string) =>
      new RegExp(`^${key}=(.*)$`, "m").exec(out)?.[1]?.trim();

    it("reads a readable file", () => {
      const out = runProbe((dir) => [
        `$p = Join-Path '${dir}' 'readable.log'`,
        `Set-Content -LiteralPath $p -Value @('alpha','beta')`,
        `$r = Read-LogFile $p`,
        `Say 'Readable' $r.Readable`,
        `Say 'Count' $r.Lines.Count`,
        `Say 'First' $r.Lines[0]`,
      ]);
      expect(field(out, "Readable")).toBe("True");
      expect(field(out, "Count")).toBe("2");
      expect(field(out, "First")).toBe("alpha");
    });

    it("treats a missing file as zero lines, not as unreadable", () => {
      // The app may not have created the log yet. Reporting this as unreadable
      // would skip the check on a perfectly normal first launch.
      const out = runProbe((dir) => [
        `$p = Join-Path '${dir}' 'never-created.log'`,
        `$r = Read-LogFile $p`,
        `Say 'Readable' $r.Readable`,
        `Say 'Count' $r.Lines.Count`,
      ]);
      expect(field(out, "Readable")).toBe("True");
      expect(field(out, "Count")).toBe("0");
    });

    it("reports growth when the file gained lines", () => {
      const out = runProbe((dir) => [
        `$p = Join-Path '${dir}' 'growing.log'`,
        `Set-Content -LiteralPath $p -Value @('one')`,
        `$before = Read-LogFile $p`,
        `Add-Content -LiteralPath $p -Value @('two')`,
        `$g = Compare-LogGrowth -Path $p -Before $before`,
        `Say 'Checked' $g.Checked`,
        `Say 'Grew' $g.Grew`,
        `Say 'Count' $g.Lines.Count`,
      ]);
      expect(field(out, "Checked")).toBe("True");
      expect(field(out, "Grew")).toBe("True");
      expect(field(out, "Count")).toBe("2");
    });

    it("reports no growth when the file is unchanged", () => {
      const out = runProbe((dir) => [
        `$p = Join-Path '${dir}' 'steady.log'`,
        `Set-Content -LiteralPath $p -Value @('one')`,
        `$before = Read-LogFile $p`,
        `$g = Compare-LogGrowth -Path $p -Before $before`,
        `Say 'Checked' $g.Checked`,
        `Say 'Grew' $g.Grew`,
      ]);
      expect(field(out, "Checked")).toBe("True");
      expect(field(out, "Grew")).toBe("False");
    });

    it("does not false-panic when the baseline read failed", () => {
      // Regression for #2043: an unreadable baseline must never make every
      // existing line look new.
      const out = runProbe((dir) => [
        `$p = Join-Path '${dir}' 'baseline.log'`,
        `Set-Content -LiteralPath $p -Value @('one','two','three')`,
        `$g = Compare-LogGrowth -Path $p -Before ([pscustomobject]@{ Readable = $false; Lines = @() })`,
        `Say 'Checked' $g.Checked`,
        `Say 'Grew' $g.Grew`,
      ]);
      expect(field(out, "Checked")).toBe("False");
      expect(field(out, "Grew")).toBe("False");
    });

    it("does not silently pass when the post-launch read failed", () => {
      // Regression for #158: collapsing an unreadable read to 0 lines makes
      // `0 -gt N` false and reports a clean launch after a real panic. The
      // result must be unchecked, which is the state the launchers skip.
      // A directory stands in for an unreadable target on every platform.
      const out = runProbe((dir) => [
        `$p = Join-Path '${dir}' 'unreadable.log'`,
        `Set-Content -LiteralPath $p -Value @('one','two')`,
        `$before = Read-LogFile $p`,
        `Remove-Item -LiteralPath $p -Force`,
        `New-Item -ItemType Directory -Path $p -Force | Out-Null`,
        `$g = Compare-LogGrowth -Path $p -Before $before`,
        `Say 'Checked' $g.Checked`,
        `Say 'Grew' $g.Grew`,
      ]);
      expect(field(out, "Checked")).toBe("False");
      expect(field(out, "Grew")).toBe("False");
    });

    it.runIf(process.platform === "win32")(
      "reports unreadable (and never throws) for an exclusively-locked file",
      () => {
        // The real reproduction: the app holds buildmesh.log open for writing.
        const out = runProbe((dir) => [
          `$p = Join-Path '${dir}' 'locked.log'`,
          `Set-Content -LiteralPath $p -Value @('alpha')`,
          `$lock = [System.IO.File]::Open($p, 'Open', 'ReadWrite', 'None')`,
          `try {`,
          `  $r = Read-LogFile $p -Attempts 1`,
          `  Say 'Readable' $r.Readable`,
          `  Say 'Count' $r.Lines.Count`,
          `} finally { $lock.Dispose() }`,
        ]);
        expect(field(out, "Readable")).toBe("False");
        expect(field(out, "Count")).toBe("0");
      },
    );
  });
});
