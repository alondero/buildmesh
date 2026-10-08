#!/usr/bin/env node
// Enforce the npm dependency-advisory policy (issue #1541).
//
// The policy is deliberately two-tiered rather than "fail everything":
//
//   * Production dependencies (`--omit=dev`) are the shipped surface. A moderate
//     or worse advisory there fails the gate. These packages are what the
//     packaged application loads at run time, so a known vulnerability there is
//     a real exposure to users, not a theoretical one.
//   * Development dependencies fail only on `high` or `critical`. A dev-only
//     advisory is not in a shipped artifact, but a build-time package can still
//     execute attacker-controlled input (a malicious tarball's install scripts,
//     a vulnerable parser fed a crafted fixture), so high and critical are
//     still treated as failures. `low` and `moderate` dev-only advisories are
//     reported, not failed, because failing them trains people to reach for
//     `--audit-level=0`.
//
// This is not a substitute for Dependabot's security updates or GitHub's
// vulnerability alerts; it is the gate that turns those notifications into a
// red pull request before merge. See docs/development/supply-chain.md for the
// full policy and the exception process.
//
// Exit codes: 0 clean, 1 policy failure, 2 the audit could not be run.

import { execFile } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

export const SEVERITIES = ['info', 'low', 'moderate', 'high', 'critical'];

// Thresholds, as npm severity strings. `moderate` is npm's name for what many
// scanners call medium.
const PRODUCTION_THRESHOLD = 'moderate';
const DEVELOPMENT_THRESHOLD = 'high';

export function meetsThreshold(severity, threshold) {
  return SEVERITIES.indexOf(severity) >= SEVERITIES.indexOf(threshold);
}

/**
 * Decide which advisories breach the policy.
 *
 * npm's JSON report is a flat `advisories` map (npm 6/7 shape) or a `vulnerabilities`
 * object keyed by package name (npm 8+ shape); both are handled so the gate does
 * not silently pass everything if npm changes format — an unrecognised shape is
 * an error, not a clean bill of health.
 */
export function evaluateReport(report) {
  const entries = [];
  if (report && report.vulnerabilities && typeof report.vulnerabilities === 'object') {
    for (const [name, vulnerability] of Object.entries(report.vulnerabilities)) {
      if (!vulnerability || typeof vulnerability !== 'object') continue;
      entries.push({
        name,
        severity: vulnerability.severity ?? 'info',
        direct: Boolean(vulnerability.isDirect),
        via: Array.isArray(vulnerability.via) ? vulnerability.via : [],
        range: typeof vulnerability.range === 'string' ? vulnerability.range : '',
      });
    }
  } else if (report && report.advisories && typeof report.advisories === 'object') {
    for (const [id, advisory] of Object.entries(report.advisories)) {
      if (!advisory || typeof advisory !== 'object') continue;
      entries.push({
        name: advisory.module_name ?? id,
        severity: advisory.severity ?? 'info',
        direct: Boolean(advisory.is_direct),
        via: [advisory.title ?? id],
        range: advisory.vulnerable_versions ?? '',
      });
    }
  } else {
    return { recognised: false, failures: [], advisories: [] };
  }

  // `dev` comes from the `via` entries npm v7+ emits for dev-only trees; a
  // vulnerability with no explicit `dev` flag is treated as production, which is
  // the conservative direction.
  const failures = [];
  for (const entry of entries) {
    const devOnly = entry.via.some(
      (item) => item && typeof item === 'object' && item.source !== undefined && item.dev === true,
    );
    const threshold = devOnly ? DEVELOPMENT_THRESHOLD : PRODUCTION_THRESHOLD;
    if (meetsThreshold(entry.severity, threshold)) {
      failures.push({
        ...entry,
        threshold,
        scope: devOnly ? 'dev' : 'production',
        reason: devOnly
          ? `dev-only advisory at or above ${DEVELOPMENT_THRESHOLD}`
          : `production advisory at or above ${PRODUCTION_THRESHOLD}`,
      });
    }
  }
  return { recognised: true, failures, advisories: entries };
}

/** Run `npm audit --json` and return the parsed report. */
export function runAudit({ cwd = repoRoot, productionOnly = false } = {}) {
  const args = ['audit', '--json'];
  if (productionOnly) args.push('--omit=dev');
  const isWindows = process.platform === 'win32';
  return new Promise((resolve, reject) => {
    const options = { cwd, encoding: 'utf8', timeout: 300_000, maxBuffer: 32 * 1024 * 1024 };
    const done = (error, stdout, stderr) => {
      // npm exits non-zero when it finds advisories. That is a *result*, not a
      // failure to run, so the report is parsed either way and only an
      // unparseable output is an error.
      if (!stdout || !stdout.trim()) {
        reject(new Error(`npm audit produced no JSON report (${stderr || error?.message || 'unknown error'}).`));
        return;
      }
      try {
        resolve(JSON.parse(stdout));
      } catch (parseError) {
        reject(new Error(`Could not parse the npm audit report: ${parseError.message}`));
      }
    };
    if (isWindows) {
      // `npm.cmd` is a batch file, which execFile cannot spawn directly (EINVAL),
      // and passing it as a single command string does not resolve it either.
      // Spawning through `cmd /c` with a *file* argument (not a command line)
      // avoids both shell quoting and Node's shell-argument deprecation warning
      // (DEP0190); the arguments are literal flags chosen by this script.
      execFile(process.env.ComSpec || 'cmd.exe', ['/d', '/s', '/c', 'npm.cmd', ...args], options, done);
      return;
    }
    execFile('npm', args, options, done);
  });
}

export async function check({ productionOnly = false } = {}) {
  let report;
  try {
    report = await runAudit({ productionOnly });
  } catch (error) {
    return { code: 2, message: error.message };
  }
  const { recognised, failures, advisories } = evaluateReport(report);
  if (!recognised) {
    return {
      code: 2,
      message:
        'Could not recognise the npm audit report format. Treat this as a gate failure: '
        + 'an unreadable audit is not a clean audit.',
    };
  }
  if (failures.length === 0) {
    const belowThreshold = advisories.filter((advisory) => !failures.includes(advisory));
    const note = belowThreshold.length > 0
      ? ` ${belowThreshold.length} advisory/advisories are below the policy thresholds and are reported only.`
      : '';
    return {
      code: 0,
      message: `npm audit found no advisory at or above policy thresholds${productionOnly ? ' (production dependencies only)' : ''}.${note}`,
      belowThreshold,
    };
  }
  const lines = failures.map(
    (failure) =>
      `  [${failure.severity}] ${failure.name} (${failure.scope}): ${failure.reason}${failure.range ? ` — vulnerable: ${failure.range}` : ''}`,
  );
  return {
    code: 1,
    message:
      `npm audit found ${failures.length} advisory/advisories that breach policy${productionOnly ? ' (production dependencies only)' : ''}:\n`
      + `${lines.join('\n')}\n\n`
      + 'Fix them (`npm audit fix`) or, if an advisory genuinely cannot be fixed yet, record it in\n'
      + '.github/dependency-audit-exceptions.json with an owner, rationale and review date —\n'
      + 'see docs/development/supply-chain.md.',
    failures,
  };
}

async function main(argv) {
  const productionOnly = argv.includes('--omit=dev');
  const result = await check({ productionOnly });
  if (result.code === 0) process.stdout.write(`${result.message}\n`);
  else process.stderr.write(`::error::${result.message}\n`);
  return result.code;
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(fileURLToPath(import.meta.url))) {
  main(process.argv.slice(2)).then((code) => {
    process.exitCode = code;
  });
}