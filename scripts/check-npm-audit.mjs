#!/usr/bin/env node
// Enforce the npm dependency-advisory policy (issue #1541).
//
// The policy is deliberately two-tiered rather than "fail everything":
//
//   * Production dependencies are the shipped surface. A moderate or worse
//     advisory there fails the gate. These packages are what the packaged
//     application loads at run time, so a known vulnerability there is a real
//     exposure to users, not a theoretical one.
//   * Development dependencies fail only on `high` or `critical`. A dev-only
//     advisory is not in a shipped artifact, but a build-time package can still
//     execute attacker-influenced input (install scripts, a parser fed a crafted
//     fixture), so high and critical are still failures. `low` and `moderate`
//     dev-only advisories are reported, not failed, because failing them trains
//     people to reach for `--audit-level=0`.
//
// HOW dev-only status is decided: not from the report. npm's `audit --json`
// output carries no `dev` flag on either the vulnerability or its `via` entries
// (verified against npm 11: the keys are name/severity/isDirect/via/effects/
// range/nodes/fixAvailable). A gate that guesses would silently apply the
// stricter production threshold to everything and quietly contradict its own
// documented policy. So npm is asked twice and the difference is the answer:
// `audit --omit=dev` reports exactly the advisories that affect the production
// tree, so a package present only in the unfiltered report is dev-only.
//
// This is not a substitute for Dependabot's security updates or GitHub's
// vulnerability alerts; it is the gate that turns those notifications into a
// red pull request before merge. See docs/development/supply-chain.md.
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
 * Flatten an `npm audit --json` report into comparable entries.
 *
 * Both the npm 8+ `vulnerabilities` object and the npm 6/7 `advisories` map are
 * handled. An unrecognised shape returns `recognised: false` rather than an
 * empty list, so a format change fails the gate instead of passing it.
 */
export function parseReport(report) {
  const entries = [];
  if (report && typeof report.vulnerabilities === 'object') {
    for (const [name, vulnerability] of Object.entries(report.vulnerabilities)) {
      if (!vulnerability || typeof vulnerability !== 'object') continue;
      const via = Array.isArray(vulnerability.via) ? vulnerability.via : [];
      entries.push({
        name,
        severity: vulnerability.severity ?? 'info',
        direct: Boolean(vulnerability.isDirect),
        // `via` is a list of advisory objects, or (when the advisory is only
        // reachable through another package) plain package-name strings.
        titles: via
          .map((item) => (typeof item === 'string' ? item : item?.title))
          .filter(Boolean),
        range: typeof vulnerability.range === 'string' ? vulnerability.range : '',
      });
    }
    return { recognised: true, entries };
  }
  if (report && typeof report.advisories === 'object') {
    for (const [id, advisory] of Object.entries(report.advisories)) {
      if (!advisory || typeof advisory !== 'object') continue;
      entries.push({
        name: advisory.module_name ?? id,
        severity: advisory.severity ?? 'info',
        direct: Boolean(advisory.is_direct),
        titles: [advisory.title ?? id].filter(Boolean),
        range: advisory.vulnerable_versions ?? '',
      });
    }
    return { recognised: true, entries };
  }
  return { recognised: false, entries: [] };
}

/**
 * Apply the two-tier policy.
 *
 * `devOnlyNames` is the set of packages npm reported in the unfiltered audit but
 * not in `audit --omit=dev`. Anything in it gets the dev threshold; everything
 * else — including a package npm could not classify — gets the production
 * threshold, which is the conservative direction.
 */
export function evaluateEntries(entries, { devOnlyNames = new Set() } = {}) {
  const failures = [];
  for (const entry of entries) {
    const devOnly = devOnlyNames.has(entry.name);
    const threshold = devOnly ? DEVELOPMENT_THRESHOLD : PRODUCTION_THRESHOLD;
    if (!meetsThreshold(entry.severity, threshold)) continue;
    failures.push({
      ...entry,
      threshold,
      scope: devOnly ? 'dev' : 'production',
      reason: devOnly
        ? `dev-only advisory at or above ${DEVELOPMENT_THRESHOLD}`
        : `production advisory at or above ${PRODUCTION_THRESHOLD}`,
    });
  }
  return failures;
}

/**
 * Combine the two reports into a policy decision.
 *
 * Both reports are required unless `productionOnly` is set, in which case the
 * single production report is authoritative and nothing can be classified as
 * dev-only (correctly, since dev dependencies were excluded).
 */
export function evaluateReports(fullReport, productionReport) {
  const full = parseReport(fullReport);
  if (!full.recognised) return { recognised: false, failures: [], entries: [] };
  if (!productionReport) {
    // Production-only mode: npm already told us everything here is production.
    return { recognised: true, failures: evaluateEntries(full.entries), entries: full.entries };
  }
  const production = parseReport(productionReport);
  if (!production.recognised) return { recognised: false, failures: [], entries: [] };
  const productionNames = new Set(production.entries.map((entry) => entry.name));
  const devOnlyNames = new Set(
    full.entries.filter((entry) => !productionNames.has(entry.name)).map((entry) => entry.name),
  );
  return {
    recognised: true,
    failures: evaluateEntries(full.entries, { devOnlyNames }),
    entries: full.entries,
    devOnlyNames,
  };
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
      // Spawning through `cmd /c` with file arguments (not a command line)
      // avoids both shell quoting and Node's shell-argument deprecation warning
      // (DEP0190); the arguments are literal flags chosen by this script.
      execFile(process.env.ComSpec || 'cmd.exe', ['/d', '/s', '/c', 'npm.cmd', ...args], options, done);
      return;
    }
    execFile('npm', args, options, done);
  });
}

function describe(failure) {
  const titles = failure.titles.length > 0 ? failure.titles.join('; ') : '(no advisory title)';
  return `  [${failure.severity}] ${failure.name} (${failure.scope}): ${failure.reason}`
    + `${failure.range ? ` — vulnerable: ${failure.range}` : ''}\n      ${titles}`;
}

export async function check({ productionOnly = false } = {}) {
  let fullReport;
  let productionReport = null;
  try {
    fullReport = await runAudit({ productionOnly });
    if (!productionOnly) {
      // The second run is what makes the dev/production split observable; see the
      // header comment for why npm's own output cannot answer it.
      productionReport = await runAudit({ productionOnly: true });
    }
  } catch (error) {
    return { code: 2, message: error.message };
  }

  const { recognised, failures, entries, devOnlyNames } = evaluateReports(fullReport, productionReport);
  if (!recognised) {
    return {
      code: 2,
      message:
        'Could not recognise the npm audit report format. Treat this as a gate failure: '
        + 'an unreadable audit is not a clean audit.',
    };
  }

  const scopeNote = productionOnly ? ' (production dependencies only)' : '';
  if (failures.length === 0) {
    const devOnly = [...(devOnlyNames ?? [])];
    const reported = devOnly.length > 0
      ? ` ${devOnly.length} dev-only advisory/advisories are below the ${DEVELOPMENT_THRESHOLD} threshold and are reported only.`
      : '';
    return {
      code: 0,
      message: `npm audit found no advisory at or above policy thresholds${scopeNote}.${reported}`,
      entries,
    };
  }

  return {
    code: 1,
    message:
      `npm audit found ${failures.length} advisory/advisories that breach policy${scopeNote}:\n`
      + `${failures.map(describe).join('\n')}\n\n`
      + 'Fix them with `npm audit fix`, or upgrade the dependency deliberately.\n'
      + 'See docs/development/supply-chain.md for the policy and how an exception is recorded.',
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