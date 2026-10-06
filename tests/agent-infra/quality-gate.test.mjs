import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { evaluateQualityGate } from '../../scripts/ci/quality-gate.mjs';

/**
 * `Verification / Quality (Linux)` is a required status check that runs no
 * check of its own: it certifies that `Detect changes`, `Quality gates
 * (Linux)` and the `Quality vitest (<leg>)` legs are green. A required check
 * that GitHub reports as *skipped* counts as satisfied, which makes "does this
 * aggregate ever skip?" the whole ball game — an earlier revision of this
 * split skipped on `frontend == 'false'` and broke the merge gate in both
 * directions (issue #2046 review).
 *
 * The four classification classes come from `scripts/ci/changed-scope.mjs`:
 * `both` (rust=true, frontend=true), `rust` (rust=true, frontend=false),
 * `frontend` (rust=false, frontend=true) and `docs` (both false). The table
 * below walks all four, because the bug only ever appeared in the two classes
 * that are *not* exercised by a change touching `.github/`.
 */
const workflow = fs.readFileSync(path.join(process.cwd(), '.github', 'workflows', 'verify.yml'), 'utf8');

/** The body of one top-level `job:` block, so one job's steps are not read as another's. */
function jobBlock(jobId) {
  const marker = `\n  ${jobId}:\n`;
  const start = workflow.indexOf(marker);
  assert.notEqual(start, -1, `verify.yml has no \`${jobId}\` job`);
  const rest = workflow.slice(start + marker.length);
  const end = rest.search(/\n {2}[a-z][\w-]*:\n/);
  return end === -1 ? workflow.slice(start) : rest.slice(0, end);
}

const CLASSES = {
  'both': { frontendChanged: 'true', testsResult: 'success' },
  'frontend only': { frontendChanged: 'true', testsResult: 'success' },
  'rust only': { frontendChanged: 'false', testsResult: 'skipped' },
  'docs only': { frontendChanged: 'false', testsResult: 'skipped' },
};

test('every classification class passes when its own branches are green', () => {
  for (const [name, cls] of Object.entries(CLASSES)) {
    assert.deepEqual(
      evaluateQualityGate({ changesResult: 'success', gatesResult: 'success', ...cls }),
      { ok: true, error: null },
      `${name} must certify green`,
    );
  }
});

test('a Rust-only pull request certifies green, so `Rust tests + TS bindings` can pass', () => {
  // Regression guard for the blocking failure: `rust-bindings` requires
  // `Quality (Linux)` == success, so if this class failed here, every
  // Rust-only pull request would be unmergeable.
  const decision = evaluateQualityGate({ changesResult: 'success', gatesResult: 'success', frontendChanged: 'false', testsResult: 'skipped' });
  assert.equal(decision.ok, true, decision.error ?? '');
});

test('a docs-only pull request cannot merge with failing static gates', () => {
  // Regression guard for the fail-open failure: the vitest legs are skipped
  // here, so if the gates went unchecked the aggregate would report green
  // with `Quality gates (Linux)` red underneath it.
  const decision = evaluateQualityGate({ changesResult: 'success', gatesResult: 'failure', frontendChanged: 'false', testsResult: 'skipped' });
  assert.equal(decision.ok, false);
  assert.match(decision.error, /quality-gates=failure/);
});

test('a failed or cancelled vitest leg fails the gate whenever the frontend changed', () => {
  for (const testsResult of ['failure', 'cancelled', '']) {
    const decision = evaluateQualityGate({ changesResult: 'success', gatesResult: 'success', frontendChanged: 'true', testsResult });
    assert.equal(decision.ok, false, `tests=${testsResult} must fail`);
    assert.match(decision.error, /quality-tests=/);
  }
});

test('a skipped vitest leg is only accepted with a successful no-frontend classification', () => {
  // The narrow exception. Anything weaker turns the skip into a hole: an empty
  // `frontend` output is what a *failed* classification leaves behind, and a
  // skipped leg with an empty output must not read as "no frontend changed".
  assert.equal(evaluateQualityGate({ changesResult: 'success', gatesResult: 'success', frontendChanged: 'false', testsResult: 'skipped' }).ok, true);
  for (const frontendChanged of ['true', '']) {
    assert.equal(
      evaluateQualityGate({ changesResult: 'success', gatesResult: 'success', frontendChanged, testsResult: 'skipped' }).ok,
      false,
      `frontend='${frontendChanged}' with a skipped leg must fail`,
    );
  }
});

test('no green gate without a successful classification', () => {
  for (const changesResult of ['failure', 'cancelled', '']) {
    const decision = evaluateQualityGate({ changesResult, gatesResult: 'success', frontendChanged: 'false', testsResult: 'skipped' });
    assert.equal(decision.ok, false, `changes=${changesResult} must fail`);
    assert.match(decision.error, /change-scope/);
  }
});

test('the aggregate job never skips itself', () => {
  // The structural half of the contract. The rules above are only reachable if
  // the job runs in every class, and the condition below is where the original
  // regression lived — so assert it stays a bare `always()`, with no
  // output-based skip sneaking back in.
  const aggregate = jobBlock('quality');
  assert.match(aggregate, /needs: \[changes, quality-gates, quality-tests\]/);
  // Compare the whole `if:` line, rather than matching a prefix: an
  // `if: always() && ...` would satisfy a `/if: always\(\)/` match and
  // reintroduce exactly the skip this job must not have. (Scraping the whole
  // block for `needs.changes.outputs.frontend` would be worse still — the
  // guard's `env:` legitimately reads that output.)
  const conditions = aggregate.split('\n').filter((line) => /^ {4}if:/.test(line));
  assert.deepEqual(conditions.map((line) => line.trim()), ['if: always()']);
});

test('the workflow wires the guard to the results the rules read', () => {
  // Pure logic can drift from the wiring that feeds it; this pins the mapping.
  const aggregate = jobBlock('quality');
  assert.match(aggregate, /run: node scripts\/ci\/quality-gate\.mjs/);
  assert.match(aggregate, /CHANGES_RESULT: \$\{\{ needs\.changes\.result \}\}/);
  assert.match(aggregate, /FRONTEND_CHANGED: \$\{\{ needs\.changes\.outputs\.frontend \}\}/);
  assert.match(aggregate, /GATES_RESULT: \$\{\{ needs\.quality-gates\.result \}\}/);
  assert.match(aggregate, /TESTS_RESULT: \$\{\{ needs\.quality-tests\.result \}\}/);
});

test('the Rust aggregate still demands a real success from this one', () => {
  // The coupling that made the skip fatal: `rust-bindings` compares the
  // literal string `success`. It is the reason the Rust-only class has to
  // certify green rather than skip, so it is worth pinning.
  assert.match(jobBlock('rust-bindings'), /if \[ "\$QUALITY_RESULT" != success \]/);
});
