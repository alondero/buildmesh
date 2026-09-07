#!/usr/bin/env node
// CI gate for `tsc --noEmit -p tsconfig.test.json`.
//
// The test/ tree accumulated ~358 type errors over years without a gate.
// Adding a hard pass/fail here would break the world, so the gate is
// snapshot-based: it runs `tsc` against `tests/unit`, `tests/integration`,
// `tests/utils`, and `tests/setup`, diffs the sorted unique output against
// `tests/.typecheck-baseline.txt`, and exits non-zero ONLY when new errors
// appear (or fixed ones disappear, which would silently shrink the gate).
//
// To widen the gate as errors are fixed, run `npm run typecheck:test` after
// editing the baseline — see the script's `--update-baseline` flag.

import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const BASELINE = join(ROOT, 'tests', '.typecheck-baseline.txt');
const SCOPE = /^(tests\/(unit|integration|utils|setup)\/)/;

// `spawnSync('npx')` ENOENTs on Windows because the npm-shipped launcher is
// `npx.cmd`. Pass `shell: true` so the platform resolves the executable
// through the user's PATHEXT — the only argument we trust is the tsc
// invocation itself, no user-controlled data crosses the shell.
const tscCommand =
  process.platform === 'win32' ? 'npx.cmd tsc' : 'npx tsc';

function runTsc() {
  const proc = spawnSync(tscCommand, ['--noEmit', '-p', 'tsconfig.test.json'], {
    cwd: ROOT,
    encoding: 'utf8',
    shell: true,
  });
  const combined = (proc.stdout ?? '') + (proc.stderr ?? '');
  return combined
    .split(/\r?\n/)
    .filter((line) => SCOPE.test(line))
    .map((line) => line.replace(/\r/g, '').trim())
    .filter(Boolean)
    .sort();
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