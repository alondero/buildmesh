#!/usr/bin/env node
// Enforce the RustSec advisory policy for the Tauri crate (issue #1541).
//
// `cargo audit` splits its findings into *vulnerabilities* (a real, exploitable
// advisory) and *warnings* (unmaintained, unsound, yanked). This repository's
// policy, documented in docs/development/supply-chain.md:
//
//   * A vulnerability fails the gate unconditionally. It must be fixed by
//     upgrading the affected crate; there is no exception mechanism, because a
//     reviewed exception to a known vulnerability is exactly the thing this gate
//     exists to prevent.
//   * A warning fails the gate unless it is listed in
//     .github/rustsec-exceptions.json with an owner, a rationale, and a
//     `reviewBy` date. A warning that is not listed fails as "unreviewed",
//     which is different from and louder than "known and accepted".
//   * A listed exception whose `reviewBy` has passed fails as "expired", so an
//     exception cannot become permanent by neglect — somebody has to re-affirm
//     it (or fix the crate) on a schedule.
//
// `cargo audit --json` writes its report to stdout and its human summary to
// stderr, and exits non-zero when it finds anything. Both facts are handled:
// the exit code is a result, not a failure to run.
//
// Exit codes: 0 clean, 1 policy failure, 2 the audit could not be run.

import { execFile } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const crateDir = path.join(repoRoot, 'src-tauri');
const exceptionsPath = path.join(repoRoot, '.github', 'rustsec-exceptions.json');

/** Kinds of advisory that are never acceptable, listed or not. */
export const BLOCKING_KINDS = ['vulnerability'];

/**
 * Flatten `cargo audit --json` output into one comparable list.
 *
 * The report groups warnings by kind (`warnings.unmaintained`, `.unsound`,
 * `.yanked`, ...) and the yanked group can contain a null advisory, because a
 * yanked crate has no RustSec advisory document. Those are keyed by crate name
 * instead, so they still get a stable identity to match an exception against.
 */
export function parseAuditReport(report) {
  const findings = [];
  for (const vulnerability of report?.vulnerabilities?.list ?? []) {
    findings.push({
      id: vulnerability.advisory?.id ?? null,
      crate: vulnerability.package?.name ?? 'unknown',
      version: vulnerability.package?.version ?? 'unknown',
      kind: 'vulnerability',
      title: vulnerability.advisory?.title ?? '',
    });
  }
  for (const [kind, list] of Object.entries(report?.warnings ?? {})) {
    for (const warning of list ?? []) {
      findings.push({
        id: warning?.advisory?.id ?? null,
        crate: warning?.package?.name ?? warning?.crate ?? 'unknown',
        version: warning?.package?.version ?? warning?.version ?? 'unknown',
        kind,
        title: warning?.advisory?.title ?? '',
      });
    }
  }
  return findings;
}

/** Load the reviewed exceptions, with a hard failure on a malformed file. */
export function loadExceptions(file = exceptionsPath) {
  const raw = JSON.parse(fs.readFileSync(file, 'utf8').replace(/^\uFEFF/, ''));
  const entries = [...(raw.advisories ?? []), ...(raw.yanked ?? [])];
  return { raw, entries };
}

/**
 * Check the exception file itself: every entry needs an owner, a rationale, and a
 * `reviewBy` date, or the review it records did not happen. A malformed file is
 * a gate failure, not a reason to skip the check.
 */
export function validateExceptions(entries, { now = new Date() } = {}) {
  const problems = [];
  const seen = new Set();
  for (const entry of entries) {
    const label = entry.id ?? `${entry.crate}@${entry.version}`;
    for (const field of ['crate', 'owner', 'rationale', 'reviewBy']) {
      if (typeof entry[field] !== 'string' || entry[field].trim() === '') {
        // 'an owner' / 'a rationale', not 'a owner'.
        const article = /^[aeiou]/i.test(field) ? 'an' : 'a';
        problems.push(`exception ${label} is missing ${article} ${field}`);
      }
    }
    if (seen.has(label)) problems.push(`exception ${label} is listed twice`);
    seen.add(label);
    if (typeof entry.reviewBy === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(entry.reviewBy)) {
      const expiry = new Date(`${entry.reviewBy}T00:00:00Z`);
      if (Number.isNaN(expiry.getTime())) {
        problems.push(`exception ${label} has an unparseable reviewBy date '${entry.reviewBy}'`);
      } else if (expiry < now) {
        problems.push(
          `exception ${label} expired on ${entry.reviewBy}: re-affirm the rationale or fix the crate`,
        );
      }
    }
  }
  return problems;
}

/**
 * Compare findings against the reviewed exceptions.
 *
 * Three distinct failures, reported separately because they call for different
 * responses: `vulnerability` (fix it), `unreviewed` (someone must look), and
 * `expired` (the review happened once and the world moved on).
 */
export function evaluateFindings(findings, entries, { now = new Date() } = {}) {
  const byId = new Map();
  const byCrate = new Map();
  for (const entry of entries) {
    if (entry.id) byId.set(entry.id, entry);
    const key = `${entry.crate}@${entry.version}`;
    if (!byCrate.has(key)) byCrate.set(key, entry);
  }

  const failures = [];
  const matched = new Set();
  for (const finding of findings) {
    if (BLOCKING_KINDS.includes(finding.kind)) {
      failures.push({
        ...finding,
        reason: 'vulnerability',
        detail:
          `${finding.id ?? finding.crate} is a vulnerability${finding.title ? `: ${finding.title}` : ''}. `
          + 'Upgrade the affected crate; vulnerabilities have no exception mechanism.',
      });
      continue;
    }
    const entry = (finding.id ? byId.get(finding.id) : null) ?? byCrate.get(`${finding.crate}@${finding.version}`);
    if (!entry) {
      failures.push({
        ...finding,
        reason: 'unreviewed',
        detail:
          `${finding.id ?? `${finding.crate}@${finding.version}`} is a ${finding.kind} advisory with no `
          + 'reviewed exception. Fix the crate, or record an owner, rationale and review date in '
          + '.github/rustsec-exceptions.json.',
      });
      continue;
    }
    matched.add(entry.id ?? `${entry.crate}@${entry.version}`);
    const expiry = new Date(`${entry.reviewBy}T00:00:00Z`);
    if (!Number.isNaN(expiry.getTime()) && expiry < now) {
      failures.push({
        ...finding,
        reason: 'expired',
        detail: `the exception for this advisory expired on ${entry.reviewBy}: re-affirm it or fix the crate`,
      });
    }
  }

  // An exception whose advisory is no longer reported is stale: the crate was
  // fixed or removed, so the rationale describes a tree that no longer exists.
  const stale = [];
  for (const entry of entries) {
    const key = entry.id ?? `${entry.crate}@${entry.version}`;
    if (!matched.has(key)) stale.push({ ...entry, key });
  }

  return { failures, stale };
}

/** Run `cargo audit --json` in the crate directory. */
export function runCargoAudit({ cwd = crateDir } = {}) {
  return new Promise((resolve, reject) => {
    execFile(
      'cargo',
      ['audit', '--json'],
      { cwd, encoding: 'utf8', timeout: 600_000, maxBuffer: 64 * 1024 * 1024 },
      (error, stdout) => {
        if (!stdout || !stdout.trim()) {
          reject(new Error(`cargo audit produced no JSON report (${error?.message ?? 'unknown error'}).`));
          return;
        }
        try {
          // Strip the BOM PowerShell/Windows pipes can prepend, then find the
          // first brace: cargo writes progress lines before the report.
          resolve(JSON.parse(stdout.replace(/^\uFEFF/, '').slice(stdout.replace(/^\uFEFF/, '').indexOf('{'))));
        } catch (parseError) {
          reject(new Error(`Could not parse the cargo audit report: ${parseError.message}`));
        }
      },
    );
  });
}

export async function check({ now = new Date() } = {}) {
  let entries;
  try {
    ({ entries } = loadExceptions());
  } catch (error) {
    return { code: 2, message: `.github/rustsec-exceptions.json could not be read: ${error.message}` };
  }
  const malformed = validateExceptions(entries, { now });
  if (malformed.length > 0) {
    return {
      code: 1,
      message: `The RustSec exception list is not reviewable:\n  ${malformed.join('\n  ')}`,
    };
  }

  let report;
  try {
    report = await runCargoAudit();
  } catch (error) {
    return { code: 2, message: error.message };
  }

  const findings = parseAuditReport(report);
  const { failures, stale } = evaluateFindings(findings, entries, { now });

  if (failures.length === 0 && stale.length === 0) {
    const note = findings.length > 0
      ? ` ${findings.length} reviewed warning(s) remain and are recorded with an owner, rationale and review date.`
      : ' The dependency tree has no advisories or warnings at all.';
    return { code: 0, message: `cargo audit policy check passed.${note}`, findings };
  }

  const parts = [];
  if (failures.length > 0) {
    parts.push(`cargo audit policy failures:\n  ${failures.map((f) => `${f.reason}: ${f.detail}`).join('\n  ')}`);
  }
  if (stale.length > 0) {
    parts.push(
      `Stale exceptions — the advisory is no longer reported, so remove the entry:\n  ${stale.map((entry) => entry.key).join('\n  ')}`,
    );
  }
  return {
    code: 1,
    message: `${parts.join('\n\n')}\n\nSee docs/development/supply-chain.md for the policy.`,
    failures,
    stale,
    findings,
  };
}

async function main() {
  const result = await check();
  if (result.code === 0) process.stdout.write(`${result.message}\n`);
  else process.stderr.write(`::error::${result.message}\n`);
  return result.code;
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(fileURLToPath(import.meta.url))) {
  main().then((code) => {
    process.exitCode = code;
  });
}