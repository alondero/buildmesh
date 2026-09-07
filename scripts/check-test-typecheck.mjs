#!/usr/bin/env node
// CI gate for `tsc --noEmit -p tsconfig.test.json`.
//
// The test/ tree accumulated hundreds of type errors over years without a
// gate. Adding a hard pass/fail here would break the world, so the gate is
// snapshot-based: it runs `tsc` against `tests/`, diffs the sorted unique
// output against `tests/.typecheck-baseline.txt`, and exits non-zero ONLY
// when new errors appear (or fixed ones disappear, which would silently
// shrink the gate).
//
// To widen the gate as errors are fixed, run:
//   npm run check:test-typecheck -- --update-baseline

import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const BASELINE = join(ROOT, 'tests', '.typecheck-baseline.txt');
// Match a tsc diagnostic header: `tests/path/file.ts(line,col): error TS####:`
// The captured key (file + location + TS error code) is stable across tsc
// versions; the diagnostic *message* wording isn't, and the multi-line
// continuation under each header is also irrelevant to the gate. Normalizing
// to a stable key means the snapshot survives:
//   * tsc version bumps that reword error messages
//   * the multi-line `Types of property X are incompatible...` explanations
//     that tsc emits under certain errors (the line-by-line filter would
//     otherwise drop them silently and they'd never contribute to the gate)
//   * any test the developer hasn't touched locally (no machine-specific
//     data — absolute paths only appear in expanded type strings inside
//     messages, never in the location header).
const HEADER = /^(\/?tests\/[^\s(]+)\((\d+),(\d+)\):\s+(error|warning)\s+(TS\d+):/;

// Invoke the local tsc binary directly via `process.execPath` rather than
// through `npx` / `npx.cmd`. Spawning a shell to reach a Node script in
// `node_modules/` is the textbook way to earn Node's [DEP0190] deprecation
// warning and is the wrong portability story on Windows (where npx is
// `npx.cmd` and `shell: true` is required to find it).
const tscEntry = join(ROOT, 'node_modules', 'typescript', 'bin', 'tsc');
if (!existsSync(tscEntry)) {
  console.error(`check-test-typecheck: tsc not found at ${tscEntry}`);
  console.error('Run `npm install` (or `npm ci`) to populate node_modules/typescript/.');
  process.exit(2);
}

function runTsc() {
  const proc = spawnSync(
    process.execPath,
    [tscEntry, '--noEmit', '--pretty', 'false', '-p', 'tsconfig.test.json'],
    { cwd: ROOT, encoding: 'utf8' },
  );
  const combined = (proc.stdout ?? '') + (proc.stderr ?? '');
  const keys = new Set();
  for (const raw of combined.split(/\r?\n/)) {
    const line = raw.replace(/\r/g, '');
    const m = line.match(HEADER);
    if (!m) continue;
    // Strip a leading `/` so the path is repo-relative on every OS.
    const file = m[1].replace(/^\/+/, '');
    keys.add(`${file}:${m[2]}:${m[3]}:${m[5]}`);
  }
  return [...keys].sort();
}

function diffLines(current, baseline) {
  const baselineSet = new Set(baseline);
  const currentSet = new Set(current);
  const added = current.filter((line) => !baselineSet.has(line));
  const removed = baseline.filter((line) => !currentSet.has(line));
  return { added, removed };
}

const args = new Set(process.argv.slice(2));
if (args.has('--update-baseline')) {
  const current = runTsc();
  writeFileSync(BASELINE, current.join('\n') + '\n');
  console.log(`Wrote ${current.length} lines to ${BASELINE}`);
  process.exit(0);
}

const current = runTsc();
const baseline = readFileSync(BASELINE, 'utf8')
  .split(/\r?\n/)
  .filter(Boolean);
const { added, removed } = diffLines(current, baseline);

if (added.length === 0 && removed.length === 0) {
  console.log(
    `test-typecheck: OK (${current.length} baseline errors, no changes)`,
  );
  process.exit(0);
}

if (removed.length > 0) {
  console.log(
    `test-typecheck: ${removed.length} baseline error(s) fixed — re-run with --update-baseline to shrink the gate`,
  );
  for (const line of removed) console.log(`  - ${line}`);
}
if (added.length > 0) {
  console.log(`test-typecheck: ${added.length} NEW error(s):`);
  for (const line of added) console.log(`  + ${line}`);
}
process.exit(1);